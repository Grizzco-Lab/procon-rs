//! Predictor app backend: what the inverse dynamics model (IDM) predicts
//!
//! The IDM lives in AgentZero (`[predictor] agentzero`, default
//! `../AgentZero` next to the config) and labels a video with the controller
//! input it sees: buttons, sticks, gyro and the camera turn. That is its
//! purpose, labeling gameplay nobody recorded a controller for, so any video
//! can be predicted: a recorded session's segment, a Cuttlefish review's
//! video or a video file on this machine.
//!
//! A run is `agentzero-predict` in the AgentZero folder, one at a time, as a
//! child process the page follows:
//!
//! ```text
//! uv run agentzero-predict <session dir> --segment <file> --checkpoint <ckpt> -o <out>
//! uv run agentzero-predict --video <file> [--start-s S --end-s E] --checkpoint <ckpt> -o <out>
//! ```
//!
//! It prints `progress <done>/<total>` lines on stderr and writes labels as
//! JSON lines (see [`gameplay_data::labels`]) with `button_probs` and
//! `camera_turn: [x, y]` besides. Which options the installed command has
//! (`--video`, `--start-s`/`--end-s`, `--cpu`) is read from its `--help`
//! once; without `--video` only sessions can run.
//!
//! Each run's output is kept in `[predictor] results` (default
//! `Predictions` next to the Inkspector's root), one folder per video and
//! checkpoint: `<video key>/<checkpoint>/pred.jsonl` and `run.json` (the
//! [`Job`]: video, range, checkpoint, time taken, frame rate).
//!
//! Endpoints under `/api/predictor/`:
//!
//! - `GET info?refresh=1`: folders, checkpoints (`runs/*/best.pt`, newest
//!   first), what `agentzero-predict` can do, GPU memory
//! - `GET job`: the current or last run (`null` before the first)
//! - `GET runs`: every stored run, newest first
//! - `GET labels?key=&ckpt=&start=&stop=`: frames `[start, stop)` of a
//!   stored run: `{"pred", "truth"}` (truth for session videos, else
//!   `null`) and their [`agreement`]
//! - `GET agreement?key=&ckpt=&start=&stop=`: only the agreement, over up
//!   to an hour of frames
//! - `POST run` with a [`RunRequest`] starts a run (409 while one runs)
//! - `POST cancel` stops the current run
//!
//! Errors are `{"error": "..."}` with status 400.

use crate::cuttlefish::{Cuttlefish, VideoKind, VideoRef};
use crate::inspect::{Inspector, ffprobe};
use crate::objects::write_atomic;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use gameplay_data::labels::{self, Label};
use gameplay_data::session::SessionInfo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// The checkpoint file in each of AgentZero's run folders
const CHECKPOINT_FILE: &str = "best.pt";

/// A run's predictions in its folder
const PRED_FILE: &str = "pred.jsonl";

/// A run's description in its folder
const RUN_FILE: &str = "run.json";

/// Lines of the command's output kept for the page
const LOG_LINES: usize = 40;

/// Most frames asked for in one `labels` request
const MAX_WINDOW: usize = 5000;

/// Most frames one `agreement` request covers (an hour at 60 fps)
const MAX_AGREEMENT: usize = 216_000;

/// Largest request body
const BODY_LIMIT: u64 = 16 << 10;

/// The Predictor's settings, from `[predictor]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// AgentZero's folder, where `uv run agentzero-predict` runs
    pub agentzero: PathBuf,
    /// Folder of stored runs
    pub results: PathBuf,
}

impl Settings {
    /// Settings from `[predictor]`; relative paths start at `config_dir`,
    /// and the results folder defaults to `default_results`
    pub fn from_config(
        config: crate::config::PredictorConfig,
        config_dir: &Path,
        default_results: PathBuf,
    ) -> Self {
        Self {
            agentzero: config_dir.join(config.agentzero.as_deref().unwrap_or("../AgentZero")),
            results: config
                .results
                .map_or(default_results, |dir| config_dir.join(dir)),
        }
    }
}

/// The video to predict
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// A segment of a session under the Inkspector's root
    Session { s: String, seg: String },
    /// A Cuttlefish review's video
    Review { id: String },
    /// A video file on this machine, by its full path
    File { path: String },
}

/// What the installed `agentzero-predict` can do, from its `--help`
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Capabilities {
    /// `--video <file>`: any video, not only sessions
    pub video: bool,
    /// `--start-s` and `--end-s`: a time range
    pub range: bool,
    /// `--cpu`: run without the GPU
    pub cpu: bool,
    /// Why the help could not be read
    pub error: Option<String>,
}

impl Capabilities {
    /// The options named in a `--help` text
    pub fn from_help(help: &str) -> Self {
        let has = |option: &str| {
            help.split(|c: char| c.is_whitespace() || c == '[' || c == ']' || c == ',')
                .any(|word| word == option)
        };
        Self {
            video: has("--video"),
            range: has("--start-s") && has("--end-s"),
            cpu: has("--cpu"),
            error: None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// What to predict and with which checkpoint
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunRequest {
    pub source: Source,
    /// Start of the range, in seconds of the video
    #[serde(default)]
    pub start_s: Option<f64>,
    /// End of the range, in seconds
    #[serde(default)]
    pub end_s: Option<f64>,
    /// Run folder under `runs/` holding `best.pt`, such as `v1`
    pub checkpoint: String,
    /// Run on the CPU (only if `agentzero-predict` has `--cpu`)
    #[serde(default)]
    pub cpu: bool,
    /// Replace the stored run of the same video and checkpoint
    #[serde(default = "default_true")]
    pub replace: bool,
}

/// State of a run
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Loading the model, or predicting
    Running,
    Done,
    Failed,
    /// Stopped by the user; nothing is kept
    Cancelled,
}

/// A run, as the page and `run.json` see it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    pub source: Source,
    /// What the video is called on the page
    pub title: String,
    /// Folder of the video under the results
    pub key: String,
    /// Checkpoint: the run folder's name
    pub checkpoint: String,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub cpu: bool,
    pub state: JobState,
    /// Windows or frames done and in all, from `progress` lines
    pub done: u64,
    pub total: Option<u64>,
    pub error: Option<String>,
    /// The GPU ran out of memory
    #[serde(default)]
    pub out_of_memory: bool,
    /// The last lines of the command's output
    #[serde(default)]
    pub log: Vec<String>,
    /// The command line, for running it by hand
    pub command: String,
    /// Frame rate of the video
    pub fps: Option<f64>,
    /// Frame `n` of the predictions is frame `n + frame_offset` of the video
    /// (a range predicted with frames counted from its start)
    #[serde(default)]
    pub frame_offset: u64,
    /// Predicted frames, once done
    #[serde(default)]
    pub frames: usize,
    /// Query of `/api/cuttlefish/video` that plays the video
    pub play: BTreeMap<String, String>,
    /// Session and segment, for videos with a controller recording
    pub session: Option<(String, String)>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// Seconds the command took
    pub seconds: Option<f64>,
}

impl Job {
    fn finished(&self) -> bool {
        self.state != JobState::Running
    }
}

/// A video resolved to its file and where its runs go
#[derive(Clone, Debug, PartialEq)]
struct Video {
    path: PathBuf,
    title: String,
    key: String,
    play: BTreeMap<String, String>,
    session: Option<(String, String)>,
}

/// A checkpoint on offer
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Checkpoint {
    /// The run folder's name
    pub name: String,
    pub path: PathBuf,
    pub modified_ms: u64,
    pub bytes: u64,
}

/// Predictions (`None` where a frame has none) and, for session videos,
/// truth of a range of frames
type Window = (Vec<Option<Label>>, Option<Vec<Label>>);

/// Predictions of one file by frame, with the file's change time
type Cached = (PathBuf, Option<SystemTime>, Arc<BTreeMap<u64, Label>>);

/// Runs the IDM and keeps its predictions
pub struct Predictor {
    inspector: Arc<Inspector>,
    cuttlefish: Arc<Cuttlefish>,
    settings: Settings,
    capabilities: Mutex<Option<Capabilities>>,
    job: Mutex<Option<Job>>,
    child: Mutex<Option<Child>>,
    cancel: AtomicBool,
    next_id: Mutex<u64>,
    predictions: Mutex<Option<Cached>>,
}

/// Unix time in ms
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A session, segment, review or checkpoint name: no path in it
fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
        "bad name {name:?}"
    );
    Ok(())
}

/// A folder name from any text: path characters and spaces become `_`
fn folder_name(text: &str) -> String {
    let name: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.trim_start_matches('.').chars().take(80).collect()
}

/// FNV-1a of a text, as 8 hex digits
fn short_hash(text: &str) -> String {
    let hash = text.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("{hash:08x}")
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned())
}

/// The arguments after `uv` for a run
fn command_args(
    video: &Video,
    request: &RunRequest,
    capabilities: &Capabilities,
    checkpoint: &Path,
    output: &Path,
    session_dir: Option<&Path>,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = ["run", "agentzero-predict"].map(String::from).to_vec();
    match (&video.session, session_dir) {
        (Some((_, seg)), Some(dir)) => {
            args.push(dir.display().to_string());
            args.extend(["--segment".to_string(), seg.clone()]);
        }
        _ => {
            ensure!(
                capabilities.video,
                "this agentzero-predict has no --video mode yet, so only recorded sessions \
                 can be predicted; update AgentZero"
            );
            args.extend(["--video".to_string(), video.path.display().to_string()]);
        }
    }
    if request.start_s.is_some() || request.end_s.is_some() {
        ensure!(
            capabilities.range,
            "this agentzero-predict has no --start-s/--end-s; clear the range to predict the whole video"
        );
        if let Some(start) = request.start_s {
            args.extend(["--start-s".to_string(), start.to_string()]);
        }
        if let Some(end) = request.end_s {
            args.extend(["--end-s".to_string(), end.to_string()]);
        }
    }
    if request.cpu {
        ensure!(capabilities.cpu, "this agentzero-predict has no --cpu");
        args.push("--cpu".to_string());
    }
    args.extend([
        "--checkpoint".to_string(),
        checkpoint.display().to_string(),
        "-o".to_string(),
        output.display().to_string(),
    ]);
    Ok(args)
}

/// `progress <done>/<total>` from a line of the command's stderr
fn parse_progress(line: &str) -> Option<(u64, u64)> {
    let (done, total) = line.trim().strip_prefix("progress ")?.split_once('/')?;
    Some((done.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// Whether the command's output says the GPU ran out of memory
fn out_of_memory(log: &[String]) -> bool {
    log.iter().any(|line| {
        let line = line.to_lowercase();
        line.contains("out of memory") || line.contains("outofmemoryerror")
    })
}

/// Where frame 0 of the predictions sits in the video: a range's first
/// frame if the file counts frames from the range's start, else 0
fn frame_offset(start_s: Option<f64>, fps: Option<f64>, first_frame: Option<u64>) -> u64 {
    let (Some(start_s), Some(fps), Some(first)) = (start_s, fps, first_frame) else {
        return 0;
    };
    let start = (start_s * fps).round().max(0.0) as u64;
    if start > 0 && first < start / 2 {
        start
    } else {
        0
    }
}

impl Predictor {
    pub fn new(inspector: Arc<Inspector>, cuttlefish: Arc<Cuttlefish>, settings: Settings) -> Self {
        Self {
            inspector,
            cuttlefish,
            settings,
            capabilities: Mutex::default(),
            job: Mutex::default(),
            child: Mutex::default(),
            cancel: AtomicBool::new(false),
            next_id: Mutex::new(1),
            predictions: Mutex::default(),
        }
    }

    /// What `agentzero-predict --help` offers, read once (again with
    /// `refresh`)
    pub fn capabilities(&self, refresh: bool) -> Capabilities {
        let mut known = self.capabilities.lock().unwrap();
        if let Some(capabilities) = known.as_ref()
            && !refresh
        {
            return capabilities.clone();
        }
        let capabilities = match Command::new("uv")
            .args(["run", "agentzero-predict", "--help"])
            .current_dir(&self.settings.agentzero)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => {
                Capabilities::from_help(&String::from_utf8_lossy(&output.stdout))
            }
            Ok(output) => Capabilities {
                error: Some(format!(
                    "agentzero-predict --help failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .last()
                        .unwrap_or_default()
                )),
                ..Default::default()
            },
            Err(e) => Capabilities {
                error: Some(format!("cannot run uv: {e}")),
                ..Default::default()
            },
        };
        *known = Some(capabilities.clone());
        capabilities
    }

    /// `runs/*/best.pt` in AgentZero, newest first
    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        let runs = self.settings.agentzero.join("runs");
        let Ok(entries) = std::fs::read_dir(&runs) else {
            return Vec::new();
        };
        let mut found: Vec<Checkpoint> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let path = entry.path().join(CHECKPOINT_FILE);
                let meta = std::fs::metadata(&path).ok()?;
                let modified_ms = meta
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                Some(Checkpoint {
                    name,
                    path,
                    modified_ms,
                    bytes: meta.len(),
                })
            })
            .collect();
        found.sort_by_key(|c| core::cmp::Reverse(c.modified_ms));
        found
    }

    /// Folders, checkpoints, the command's options and GPU memory
    pub fn info(&self, refresh: bool) -> Value {
        json!({
            "agentzero": self.settings.agentzero,
            "found": self.settings.agentzero.join("pyproject.toml").is_file(),
            "results": self.settings.results,
            "checkpoints": self.checkpoints(),
            "capabilities": self.capabilities(refresh),
            "gpu": gpu_memory(),
        })
    }

    /// The current or last run
    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.job.lock().unwrap().as_mut() {
            f(job);
        }
    }

    /// The file, title and results folder of a video
    fn video(&self, source: &Source) -> Result<Video> {
        match source {
            Source::Session { s, seg } => {
                check_name(s)?;
                check_name(seg)?;
                let dir = self.inspector.root().join(s);
                let info = SessionInfo::read(&dir)?;
                ensure!(
                    info.video.segments.iter().any(|x| &x.file == seg),
                    "{s} has no segment {seg}"
                );
                Ok(Video {
                    path: dir.join(seg),
                    title: format!("{s} · {seg}"),
                    key: format!("{s}.{}", file_stem(Path::new(seg))),
                    play: BTreeMap::from([
                        ("kind".to_string(), "session".to_string()),
                        ("ref".to_string(), format!("{s}/{seg}")),
                    ]),
                    session: Some((s.clone(), seg.clone())),
                })
            }
            Source::Review { id } => {
                check_name(id)?;
                let review = self.cuttlefish.review(id)?;
                let video = &review.video;
                let path = self.cuttlefish.video_path(video, Some(id))?;
                // A review of a session's recording has its controller data too
                let session = (video.kind == VideoKind::Session && video.file.is_none())
                    .then(|| {
                        let s = video.reference.split('/').next()?.to_string();
                        let seg = path.file_name()?.to_str()?.to_string();
                        Some((s, seg))
                    })
                    .flatten();
                let mut play = BTreeMap::from([
                    ("kind".to_string(), kind_name(video.kind).to_string()),
                    ("ref".to_string(), video.reference.clone()),
                    ("r".to_string(), id.clone()),
                ]);
                if let Some(file) = &video.file {
                    play.insert("file".to_string(), file.clone());
                }
                Ok(Video {
                    title: video
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("Review {id}")),
                    path,
                    key: format!("review-{id}"),
                    play,
                    session,
                })
            }
            Source::File { path } => {
                let video = VideoRef {
                    kind: VideoKind::File,
                    reference: path.clone(),
                    start_s: None,
                    end_s: None,
                    file: None,
                    title: None,
                    channel: None,
                    upload_date: None,
                };
                let path = self.cuttlefish.video_path(&video, None)?;
                let name = path
                    .file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().into_owned());
                Ok(Video {
                    key: format!(
                        "file-{}-{}",
                        folder_name(&file_stem(&path)),
                        short_hash(&video.reference)
                    ),
                    title: name,
                    path,
                    play: BTreeMap::from([
                        ("kind".to_string(), "file".to_string()),
                        ("ref".to_string(), video.reference),
                    ]),
                    session: None,
                })
            }
        }
    }

    /// The frame rate of a video: a session's recorded rate, else ffprobe's
    fn fps(&self, video: &Video) -> Option<f64> {
        if let Some((s, seg)) = &video.session
            && let Ok(info) = self.inspector.info(s, Some(seg))
            && let Some(fps) = info["fps"].as_f64()
        {
            return Some(fps);
        }
        let probe = ffprobe(&video.path, "stream=avg_frame_rate,r_frame_rate").ok()?;
        ["avg_frame_rate", "r_frame_rate"].iter().find_map(|key| {
            let (num, den) = probe["streams"][0][key].as_str()?.split_once('/')?;
            let fps = num.parse::<f64>().ok()? / den.parse::<f64>().ok()?;
            (fps.is_finite() && fps > 0.0).then_some(fps)
        })
    }

    /// `<results>/<key>/<checkpoint>`
    fn run_dir(&self, key: &str, checkpoint: &str) -> Result<PathBuf> {
        check_name(key)?;
        check_name(checkpoint)?;
        Ok(self.settings.results.join(key).join(checkpoint))
    }

    /// Start a run on a thread of its own
    pub fn run(self: &Arc<Self>, request: RunRequest) -> Result<Job> {
        if let (Some(start), Some(end)) = (request.start_s, request.end_s) {
            ensure!(
                start >= 0.0 && end > start,
                "the range ends before it starts"
            );
        }
        check_name(&request.checkpoint)?;
        let checkpoint = self
            .settings
            .agentzero
            .join("runs")
            .join(&request.checkpoint)
            .join(CHECKPOINT_FILE);
        ensure!(checkpoint.is_file(), "no {}", checkpoint.display());
        let video = self.video(&request.source)?;
        let capabilities = self.capabilities(false);
        let dir = self.run_dir(&video.key, &request.checkpoint)?;
        ensure!(
            request.replace || !dir.join(PRED_FILE).is_file(),
            "{} has predictions from {} already",
            video.title,
            request.checkpoint
        );
        let output = dir.join(format!("{PRED_FILE}.part"));
        let session_dir = video
            .session
            .as_ref()
            .map(|(s, _)| self.inspector.root().join(s));
        let args = command_args(
            &video,
            &request,
            &capabilities,
            &checkpoint,
            &output,
            session_dir.as_deref(),
        )?;
        let fps = self.fps(&video);

        let mut current = self.job.lock().unwrap();
        if let Some(job) = &*current
            && !job.finished()
        {
            bail!("a run is under way; cancel it first");
        }
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next - 1
        };
        let job = Job {
            id,
            source: request.source.clone(),
            title: video.title.clone(),
            key: video.key.clone(),
            checkpoint: request.checkpoint.clone(),
            start_s: request.start_s,
            end_s: request.end_s,
            cpu: request.cpu,
            state: JobState::Running,
            done: 0,
            total: None,
            error: None,
            out_of_memory: false,
            log: Vec::new(),
            command: format!(
                "cd {} && uv {}",
                self.settings.agentzero.display(),
                args.join(" ")
            ),
            fps,
            frame_offset: 0,
            frames: 0,
            play: video.play.clone(),
            session: video.session.clone(),
            started_ms: now_ms(),
            finished_ms: None,
            seconds: None,
        };
        *current = Some(job.clone());
        drop(current);
        self.cancel.store(false, Ordering::Relaxed);
        log::info!("Predicting {} with {}", video.title, request.checkpoint);
        let predictor = Arc::clone(self);
        std::thread::Builder::new()
            .name("predictor".to_string())
            .spawn(move || {
                let started = Instant::now();
                let result = predictor.work(&args, &dir, &request);
                let cancelled = predictor.cancel.load(Ordering::Relaxed);
                predictor.update(|job| {
                    job.finished_ms = Some(now_ms());
                    job.seconds = Some((started.elapsed().as_secs_f64() * 10.0).round() / 10.0);
                    match result {
                        _ if cancelled => job.state = JobState::Cancelled,
                        Ok(frames) => {
                            job.state = JobState::Done;
                            job.frames = frames.0;
                            job.frame_offset = frame_offset(job.start_s, job.fps, frames.1);
                        }
                        Err(e) => {
                            job.state = JobState::Failed;
                            job.out_of_memory = out_of_memory(&job.log);
                            job.error = Some(if job.out_of_memory {
                                "The GPU ran out of memory: something else (training?) is using it. \
                                 Wait for it to finish and run again"
                                    .to_string()
                            } else {
                                format!("{e:#}")
                            });
                            log::warn!("Prediction failed: {:#}", e);
                        }
                    }
                });
                if let Some(job) = predictor.job()
                    && job.state == JobState::Done
                    && let Err(e) = write_atomic(&dir.join(RUN_FILE), &serde_json::to_vec_pretty(&job).unwrap_or_default())
                {
                    log::warn!("Cannot save the prediction run: {:#}", e);
                }
            })?;
        Ok(job)
    }

    /// Run the command, following its output; on success the predictions
    /// replace the stored ones. Answers with the frames predicted and the
    /// first frame's number.
    fn work(
        &self,
        args: &[String],
        dir: &Path,
        request: &RunRequest,
    ) -> Result<(usize, Option<u64>)> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let partial = dir.join(format!("{PRED_FILE}.part"));
        let _ = std::fs::remove_file(&partial);
        let mut child = Command::new("uv")
            .args(args)
            .current_dir(&self.settings.agentzero)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run uv")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;
        *self.child.lock().unwrap() = Some(child);
        let log = Arc::new(Mutex::new(VecDeque::new()));
        let keep = |log: &Mutex<VecDeque<String>>, line: &str| {
            let mut log = log.lock().unwrap();
            log.push_back(line.to_string());
            if log.len() > LOG_LINES {
                log.pop_front();
            }
        };
        let out_log = Arc::clone(&log);
        let reader = std::thread::spawn(move || {
            crate::cuttlefish::for_each_line(stdout, |line| {
                if !line.trim().is_empty() {
                    keep(&out_log, line);
                }
            });
        });
        crate::cuttlefish::for_each_line(stderr, |line| {
            if let Some((done, total)) = parse_progress(line) {
                self.update(|job| {
                    job.done = done;
                    job.total = Some(total);
                });
            } else if !line.trim().is_empty() {
                keep(&log, line);
                let lines: Vec<String> = log.lock().unwrap().iter().cloned().collect();
                self.update(|job| job.log = lines);
            }
        });
        let _ = reader.join();
        let status = self
            .child
            .lock()
            .unwrap()
            .take()
            .context("the run was lost")?
            .wait()?;
        let lines: Vec<String> = log.lock().unwrap().iter().cloned().collect();
        self.update(|job| job.log = lines.clone());
        if !status.success() {
            let _ = std::fs::remove_file(&partial);
            let last = lines
                .iter()
                .rev()
                .find(|l| !l.trim().is_empty())
                .cloned()
                .unwrap_or_default();
            bail!("agentzero-predict failed ({status}): {last}");
        }
        let predicted =
            labels::read_labels(&partial).context("agentzero-predict wrote no predictions")?;
        ensure!(
            !predicted.is_empty(),
            "agentzero-predict wrote no predictions"
        );
        std::fs::rename(&partial, dir.join(PRED_FILE))?;
        log::info!(
            "Predicted {} frames into {} ({:?}..{:?} s)",
            predicted.len(),
            dir.display(),
            request.start_s,
            request.end_s
        );
        Ok((predicted.len(), predicted.iter().map(|l| l.frame).min()))
    }

    /// Stop the current run
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }

    /// Every stored run, newest first
    pub fn runs(&self) -> Result<Value> {
        let root = &self.settings.results;
        let mut runs: Vec<Job> = Vec::new();
        let Ok(videos) = std::fs::read_dir(root) else {
            return Ok(json!({ "results": root, "runs": [] }));
        };
        for video in videos.filter_map(|e| e.ok()) {
            let Ok(checkpoints) = std::fs::read_dir(video.path()) else {
                continue;
            };
            for checkpoint in checkpoints.filter_map(|e| e.ok()) {
                let dir = checkpoint.path();
                if !dir.join(PRED_FILE).is_file() {
                    continue;
                }
                match std::fs::read(dir.join(RUN_FILE))
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| Ok(serde_json::from_slice::<Job>(&bytes)?))
                {
                    Ok(job) => runs.push(job),
                    Err(e) => log::warn!("Skipping {}: {:#}", dir.display(), e),
                }
            }
        }
        runs.sort_by_key(|job| core::cmp::Reverse(job.finished_ms.unwrap_or(job.started_ms)));
        Ok(json!({ "results": root, "runs": runs }))
    }

    /// A stored run
    fn stored(&self, key: &str, checkpoint: &str) -> Result<(PathBuf, Job)> {
        let dir = self.run_dir(key, checkpoint)?;
        let bytes = std::fs::read(dir.join(RUN_FILE))
            .with_context(|| format!("no run {key}/{checkpoint}"))?;
        Ok((dir, serde_json::from_slice(&bytes)?))
    }

    /// Predictions of a file by frame, read once per change of the file
    fn predictions(&self, path: &Path) -> Result<Arc<BTreeMap<u64, Label>>> {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut cached = self.predictions.lock().unwrap();
        if let Some((cached_path, cached_modified, labels)) = cached.as_ref()
            && cached_path == path
            && *cached_modified == modified
        {
            return Ok(Arc::clone(labels));
        }
        let labels: BTreeMap<u64, Label> = labels::read_labels(path)?
            .into_iter()
            .map(|label| (label.frame, label))
            .collect();
        let labels = Arc::new(labels);
        *cached = Some((path.to_path_buf(), modified, Arc::clone(&labels)));
        Ok(labels)
    }

    /// Frames `[start, stop)` of the video of a stored run: predictions,
    /// truth for session videos, and how well they agree
    pub fn labels(&self, key: &str, checkpoint: &str, start: usize, stop: usize) -> Result<Value> {
        ensure!(
            stop.saturating_sub(start) <= MAX_WINDOW,
            "at most {MAX_WINDOW} frames at once"
        );
        let (pred, truth) = self.window(key, checkpoint, start, stop)?;
        let agreement = truth
            .as_ref()
            .map(|truth| agreement(truth, &pred[..truth.len()]));
        Ok(json!({
            "start": start,
            "pred": pred,
            "truth": truth,
            "agreement": agreement,
        }))
    }

    /// How well a stored run agrees with the truth over frames `[start,
    /// stop)`; `null` for videos without a controller recording
    pub fn agreement(
        &self,
        key: &str,
        checkpoint: &str,
        start: usize,
        stop: usize,
    ) -> Result<Value> {
        ensure!(
            stop.saturating_sub(start) <= MAX_AGREEMENT,
            "at most {MAX_AGREEMENT} frames at once"
        );
        let (pred, truth) = self.window(key, checkpoint, start, stop)?;
        Ok(json!(
            truth.map(|truth| agreement(&truth, &pred[..truth.len()]))
        ))
    }

    /// Predictions and, for session videos, truth of frames `[start, stop)`
    /// of a stored run's video
    fn window(&self, key: &str, checkpoint: &str, start: usize, stop: usize) -> Result<Window> {
        ensure!(stop >= start, "stop comes before start");
        let (dir, job) = self.stored(key, checkpoint)?;
        let predicted = self.predictions(&dir.join(PRED_FILE))?;
        let offset = job.frame_offset;
        let pred: Vec<Option<Label>> = (start..stop)
            .map(|n| {
                (n as u64)
                    .checked_sub(offset)
                    .and_then(|k| predicted.get(&k))
                    .map(|label| Label {
                        frame: label.frame + offset,
                        ..label.clone()
                    })
            })
            .collect();
        let truth: Option<Vec<Label>> = match &job.session {
            Some((s, seg)) => {
                let mut value = self
                    .inspector
                    .labels(s, Some(seg), start..stop, None, None)?;
                Some(serde_json::from_value(value["truth"].take())?)
            }
            None => None,
        };
        Ok((pred, truth))
    }
}

/// `/api/cuttlefish/video`'s name of a kind
fn kind_name(kind: VideoKind) -> &'static str {
    match kind {
        VideoKind::Session => "session",
        VideoKind::File => "file",
        VideoKind::Youtube => "youtube",
    }
}

/// Used and total GPU memory in MiB, from `nvidia-smi`
fn gpu_memory() -> Option<Value> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (used, total) = text.lines().next()?.split_once(',')?;
    Some(json!({
        "used_mib": used.trim().parse::<u64>().ok()?,
        "total_mib": total.trim().parse::<u64>().ok()?,
    }))
}

/// Pearson correlation of pairs; `None` with fewer than 3 or no spread
pub fn correlation(pairs: &[(f64, f64)]) -> Option<f64> {
    if pairs.len() < 3 {
        return None;
    }
    let n = pairs.len() as f64;
    let (mx, my) = pairs
        .iter()
        .fold((0.0, 0.0), |(x, y), (a, b)| (x + a / n, y + b / n));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in pairs {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx) * (a - mx);
        syy += (b - my) * (b - my);
    }
    (sxx > 1e-12 && syy > 1e-12).then(|| sxy / (sxx * syy).sqrt())
}

/// The camera turn `[x, y]` of a label, if it has one
fn camera_turn(label: &Label) -> Option<[f64; 2]> {
    serde_json::from_value(label.extra.get("camera_turn")?.clone()).ok()
}

/// How well predictions agree with the truth over the frames both have
/// (truth with controller reports): F1 per button pressed in either, and
/// the correlation of each stick axis, gyro pitch and yaw, and camera turn
pub fn agreement(truth: &[Label], pred: &[Option<Label>]) -> Value {
    let pairs: Vec<(&Label, &Label)> = truth
        .iter()
        .zip(pred)
        .filter(|(t, _)| t.valid == Some(true))
        .filter_map(|(t, p)| Some((t, p.as_ref()?)))
        .collect();
    let pressed =
        |label: &Label| -> BTreeSet<String> { label.buttons.iter().flatten().cloned().collect() };
    let mut counts: BTreeMap<String, [u64; 3]> = BTreeMap::new();
    for (t, p) in &pairs {
        let (t, p) = (pressed(t), pressed(p));
        for name in t.union(&p) {
            let count = counts.entry(name.clone()).or_default();
            match (t.contains(name), p.contains(name)) {
                (true, true) => count[0] += 1,
                (false, true) => count[1] += 1,
                (true, false) => count[2] += 1,
                (false, false) => {}
            }
        }
    }
    let buttons: Vec<Value> = counts
        .into_iter()
        .map(|(name, [tp, fp, fn_])| {
            json!({
                "name": name,
                "f1": 2.0 * tp as f64 / (2 * tp + fp + fn_) as f64,
                "truth": tp + fn_,
                "pred": tp + fp,
            })
        })
        .collect();
    type Pick = fn(&Label) -> Option<f64>;
    let signals: [(&str, Pick); 8] = [
        ("left_x", |l| Some(l.left_stick?[0])),
        ("left_y", |l| Some(l.left_stick?[1])),
        ("right_x", |l| Some(l.right_stick?[0])),
        ("right_y", |l| Some(l.right_stick?[1])),
        ("gyro_pitch", |l| Some(l.gyro_deg?[1])),
        ("gyro_yaw", |l| Some(l.gyro_deg?[2])),
        ("turn_x", |l| Some(camera_turn(l)?[0])),
        ("turn_y", |l| Some(camera_turn(l)?[1])),
    ];
    let signals: Vec<Value> = signals
        .iter()
        .map(|(name, pick)| {
            let values: Vec<(f64, f64)> = pairs
                .iter()
                .filter_map(|(t, p)| Some((pick(t)?, pick(p)?)))
                .collect();
            json!({ "name": name, "r": correlation(&values), "n": values.len() })
        })
        .collect();
    json!({ "frames": pairs.len(), "buttons": buttons, "signals": signals })
}

// ------------------------------------------------------------------ HTTP

/// An error with the status it answers with
struct Status(StatusCode, anyhow::Error);

impl From<anyhow::Error> for Status {
    fn from(e: anyhow::Error) -> Self {
        Status(StatusCode::BAD_REQUEST, e)
    }
}

impl Predictor {
    fn get(&self, path: &str, query: &HashMap<String, String>) -> Result<Value, Status> {
        let text = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
        let number = |key: &str| -> Result<usize> {
            text(key).parse().with_context(|| format!("bad {key}"))
        };
        match path {
            "info" => Ok(self.info(text("refresh") == "1")),
            "job" => Ok(json!(self.job())),
            "runs" => Ok(self.runs()?),
            "labels" => {
                Ok(self.labels(text("key"), text("ckpt"), number("start")?, number("stop")?)?)
            }
            "agreement" => {
                Ok(self.agreement(text("key"), text("ckpt"), number("start")?, number("stop")?)?)
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }

    fn post(self: &Arc<Self>, path: &str, body: &[u8]) -> Result<Value, Status> {
        match path {
            "run" => {
                let request: RunRequest = serde_json::from_slice(body)
                    .map_err(|e| anyhow::anyhow!("bad run request: {e}"))?;
                if self.job().is_some_and(|job| !job.finished()) {
                    return Err(Status(
                        StatusCode::CONFLICT,
                        anyhow::anyhow!("a run is under way; cancel it first"),
                    ));
                }
                Ok(json!(self.run(request)?))
            }
            "cancel" => {
                self.cancel();
                Ok(json!(self.job()))
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST {path}"),
            )),
        }
    }
}

/// The routes under `/api/predictor/`; files and processes are handled off
/// the async workers
pub fn routes(predictor: Arc<Predictor>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || warp::path("api").and(warp::path("predictor"));
    let reader = Arc::clone(&predictor);
    let get = warp::get()
        .and(base())
        .and(warp::path::tail())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |tail: warp::path::Tail, query: HashMap<String, String>| {
                let predictor = Arc::clone(&reader);
                blocking(move || predictor.get(tail.as_str(), &query))
            },
        );
    let post = warp::post()
        .and(base())
        .and(warp::path::tail())
        .and(warp::body::content_length_limit(BODY_LIMIT))
        .and(warp::body::bytes())
        .and_then(
            move |tail: warp::path::Tail, body: warp::hyper::body::Bytes| {
                let predictor = Arc::clone(&predictor);
                blocking(move || predictor.post(tail.as_str(), &body))
            },
        );
    get.or(post).unify().boxed()
}

/// Run `answer` on a blocking thread and turn it into a JSON response
async fn blocking(
    answer: impl FnOnce() -> Result<Value, Status> + Send + 'static,
) -> Result<Response<Vec<u8>>, core::convert::Infallible> {
    let (status, value) = match tokio::task::spawn_blocking(answer).await {
        Ok(Ok(value)) => (StatusCode::OK, value),
        Ok(Err(Status(status, e))) => (status, json!({ "error": format!("{e:#}") })),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    };
    Ok(Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(value.to_string().into_bytes())
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP_NOW: &str =
        "usage: agentzero-predict [-h] --checkpoint CHECKPOINT [--segment SEGMENT]
                         [-o OUTPUT] [--stride STRIDE] [--batch BATCH]
                         input";

    const HELP_NEXT: &str = "usage: agentzero-predict [-h] [--video VIDEO] [--start-s START_S]
                         [--end-s END_S] [--cpu] --checkpoint CHECKPOINT [input]
options:
  --video VIDEO   a video file
  --cpu           run on the CPU";

    #[test]
    fn capabilities_from_help() {
        assert_eq!(Capabilities::from_help(HELP_NOW), Capabilities::default());
        let next = Capabilities::from_help(HELP_NEXT);
        assert!(next.video && next.range && next.cpu);
        // Words that merely start alike do not count
        assert!(!Capabilities::from_help("--videos --cpus").video);
    }

    fn video(session: bool) -> Video {
        Video {
            path: PathBuf::from("/d/2026-09-25_11-26-22/video-01.mkv"),
            title: String::new(),
            key: "k".into(),
            play: BTreeMap::new(),
            session: session.then(|| ("2026-09-25_11-26-22".into(), "video-01.mkv".into())),
        }
    }

    fn request(json: &str) -> RunRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn commands() {
        let now = Capabilities::from_help(HELP_NOW);
        let next = Capabilities::from_help(HELP_NEXT);
        let ckpt = Path::new("/az/runs/v1/best.pt");
        let out = Path::new("/p/k/v1/pred.jsonl.part");
        let session = request(
            r#"{"source": {"kind": "session", "s": "2026-09-25_11-26-22", "seg": "video-01.mkv"}, "checkpoint": "v1"}"#,
        );
        let args = command_args(
            &video(true),
            &session,
            &now,
            ckpt,
            out,
            Some(Path::new("/d/2026-09-25_11-26-22")),
        )
        .unwrap();
        assert_eq!(
            args.join(" "),
            "run agentzero-predict /d/2026-09-25_11-26-22 --segment video-01.mkv \
             --checkpoint /az/runs/v1/best.pt -o /p/k/v1/pred.jsonl.part"
        );

        let file = request(
            r#"{"source": {"kind": "file", "path": "/v/a.mp4"}, "checkpoint": "v1", "start_s": 5, "end_s": 7.5, "cpu": true}"#,
        );
        // No --video yet: say so instead of running
        let error = command_args(&video(false), &file, &now, ckpt, out, None).unwrap_err();
        assert!(error.to_string().contains("--video"));
        let args = command_args(&video(false), &file, &next, ckpt, out, None).unwrap();
        assert_eq!(
            args[2..8].join(" "),
            "--video /d/2026-09-25_11-26-22/video-01.mkv --start-s 5 --end-s 7.5"
        );
        assert!(args.contains(&"--cpu".to_string()));
        // A range without --start-s is refused, not dropped
        let ranged = RunRequest {
            start_s: Some(1.0),
            ..session.clone()
        };
        assert!(
            command_args(
                &video(true),
                &ranged,
                &now,
                ckpt,
                out,
                Some(Path::new("/d"))
            )
            .is_err()
        );
    }

    #[test]
    fn progress_and_errors() {
        assert_eq!(parse_progress("progress 3/120"), Some((3, 120)));
        assert_eq!(parse_progress("  progress 10 / 20 "), Some((10, 20)));
        assert_eq!(parse_progress("predicting 300 frames"), None);
        assert!(out_of_memory(&[
            "torch.OutOfMemoryError: CUDA out of memory. Tried to allocate 2.00 GiB".into()
        ]));
        assert!(!out_of_memory(&["ValueError: bad".into()]));
    }

    #[test]
    fn range_offsets() {
        // Frames counted from the range's start
        assert_eq!(frame_offset(Some(10.0), Some(30.0), Some(0)), 300);
        // Frames counted from the video's start
        assert_eq!(frame_offset(Some(10.0), Some(30.0), Some(300)), 0);
        assert_eq!(frame_offset(None, Some(30.0), Some(0)), 0);
        assert_eq!(frame_offset(Some(0.0), Some(30.0), Some(0)), 0);
    }

    #[test]
    fn names() {
        assert_eq!(folder_name("工房 满潮/76蛋"), "工房_满潮_76蛋");
        assert_eq!(folder_name("..hidden"), "hidden");
        assert_eq!(short_hash("a").len(), 8);
        assert_ne!(short_hash("/a.mp4"), short_hash("/b.mp4"));
        assert!(check_name("../x").is_err());
    }

    fn label(frame: u64, valid: bool, buttons: &[&str], left_x: f64, turn: Option<f64>) -> Label {
        let mut label = Label {
            frame,
            valid: Some(valid),
            buttons: Some(buttons.iter().map(|b| b.to_string()).collect()),
            left_stick: Some([left_x, 2048.0]),
            right_stick: Some([2048.0, 2048.0]),
            gyro_deg: Some([0.0, frame as f64, -(frame as f64)]),
            extra: Default::default(),
        };
        if let Some(turn) = turn {
            label.extra.insert("camera_turn".into(), json!([turn, 0.0]));
        }
        label
    }

    #[test]
    fn agreement_numbers() {
        let truth = vec![
            label(0, true, &["zr"], 1000.0, None),
            label(1, true, &["zr", "a"], 2000.0, None),
            label(2, true, &[], 3000.0, None),
            label(3, false, &["b"], 0.0, None),
            label(4, true, &["zr"], 4000.0, None),
        ];
        let pred = vec![
            Some(label(0, true, &["zr"], 1100.0, Some(1.0))),
            Some(label(1, true, &["zr"], 1900.0, Some(2.0))),
            Some(label(2, true, &["zr"], 3200.0, Some(3.0))),
            Some(label(3, true, &[], 0.0, None)),
            None,
        ];
        let a = agreement(&truth, &pred);
        // Frames 0..=2: frame 3 has no reports, frame 4 no prediction
        assert_eq!(a["frames"], 3);
        let buttons = a["buttons"].as_array().unwrap();
        assert_eq!(buttons[0]["name"], "a");
        assert_eq!(buttons[0]["f1"], 0.0);
        // zr: 2 hits, 1 false alarm
        assert_eq!(buttons[1]["name"], "zr");
        assert_eq!(buttons[1]["f1"], 0.8);
        let signal = |name: &str| {
            a["signals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["name"] == name)
                .unwrap()
                .clone()
        };
        assert!(signal("left_x")["r"].as_f64().unwrap() > 0.95);
        assert_eq!(signal("gyro_yaw")["r"], 1.0);
        // Sticks that never move have no correlation
        assert_eq!(signal("right_x")["r"], Value::Null);
        // The truth has no camera turn
        assert_eq!(signal("turn_x")["n"], 0);
    }

    #[test]
    fn correlations() {
        assert_eq!(correlation(&[(1.0, 1.0), (2.0, 2.0)]), None);
        let r = correlation(&[(1.0, 3.0), (2.0, 2.0), (3.0, 1.0)]).unwrap();
        assert!((r + 1.0).abs() < 1e-9);
        assert_eq!(correlation(&[(1.0, 1.0), (1.0, 2.0), (1.0, 3.0)]), None);
    }
}
