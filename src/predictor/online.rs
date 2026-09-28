//! The Predictor's online mode: AgentZero's policy, frame by frame, as if live
//!
//! The policy is AgentZero's causal model (`agentzero-play`, checkpoints in
//! `runs/policy/*/best.pt` of the AgentZero folder): it sees the frames up to
//! the one in hand, never later ones, and says what the controller does next.
//! It runs on
//!
//! - **a video**, of the kinds the IDM's runs take (a session's segment, a
//!   Cuttlefish review's video, a file), paced at 30 fps as if it were live
//!   (`--realtime`: while the model is busy, frames are skipped as a live
//!   source skips them). The page plays the video along and shows each
//!   frame's predicted action. When it ends, or is stopped, the predictions
//!   are kept as a run of the Predictor (`<results>/<video key>/policy-<checkpoint>/`,
//!   frames at 30 fps from the start of the range), with the truth beside
//!   them for sessions recorded at 30 fps;
//! - **the live capture**: the frames the studio grabs, thinned to 30 fps and
//!   piped raw into `agentzero-play`'s stdin, whose ffmpeg scales them (the
//!   capture card opens only once, and the studio holds it). Each frame's
//!   capture time is kept by its number in the pipe, which `agentzero-play`
//!   reports back with its action.
//!
//! ```text
//! uv run agentzero-play --checkpoint <ckpt> --video <file> [--start-s S] [--end-s E] --realtime --dry-run --json [--cpu]
//! uv run agentzero-play --checkpoint <ckpt> --capture "-f rawvideo ... -i pipe:0" --dry-run --json [--cpu]
//! ```
//!
//! One runs at a time, in a process group of its own (Stop sends it SIGTERM,
//! then SIGKILL after [`KILL_AFTER`]; the studio stops it on exit). It never
//! sends anything itself (`--dry-run`): its JSON lines come back here, and
//! only the studio talks to the proxy.
//!
//! **Letting AgentZero play** (live only, [`Bot`]) is a second step, off by
//! default: the page asks for confirmation each time, for a set time (at most
//! [`MAX_PLAY_S`]). The studio then writes each action to the proxy's replay
//! port as a replay line with `mix` (see [`crate::replay`]): the proxy
//! combines it with the physical controller on every report, so a person
//! holding it corrects the bot live, without pausing it: their buttons add
//! to the bot's, and a stick they push past a small deadzone, or a turn
//! faster than 10 °/s, replaces the bot's while they do. Without `mix`, the
//! proxy would pass on the bot's reports alone. Before a line goes out, the
//! bot's buttons are held to [`Limits`] ([`limits`]): the d-pad and the
//! special blocked unless unticked, Home and Capture never, and no button
//! pressed faster than a person could. Sending ends, with a neutral line
//! and the connection closed (the controller alone again), when the time is
//! up, on the page's Stop bot (or Esc, anywhere on the page), when the mode
//! stops, when no dashboard page has been open for [`PAGE_GONE`], when the
//! policy goes quiet for [`STALL`], when the Replay panel starts playing
//! (the proxy serves one replay client at a time), and when the proxy's
//! frames stop reaching the studio (what reaches the Switch could not be
//! seen; it does not start without them either).
//!
//! The bot also measures a person's fastest tapping of ZR from the proxy's
//! frames ([`limits::Tapping`]), for the page to set the cap from.
//!
//! **The loop's latency** (live), on this machine's clock: from the capture
//! card's timestamp of a frame to its action written to the replay port, in
//! three parts: the frame's hand-off (until the studio writes it into the
//! pipe, on through the pipe and ffmpeg's scaling, then waiting for the model
//! to finish the frame before), the model, and the send (the line back to
//! the studio and onto the socket; while not sending, until the studio has
//! it).
//!
//! Running while the studio records needs `allow_recording` (the recording
//! may want the GPU; the bot's own play may be worth recording): a start is
//! refused while a session records, and a run without it stops when a
//! recording starts.
//!
//! Endpoints under `/api/predictor/online/`:
//!
//! - `GET status`: `{"run", "bot"}`, the mode's [`Status`] (`null` before
//!   the first run) and the [`BotStatus`]
//! - `GET checkpoints?refresh=1`: the policy checkpoints, newest first, what
//!   the installed `agentzero-play` can do and GPU memory
//! - `GET labels?start=&stop=`: frames `[start, stop)` of the current video
//!   run, as the Predictor's `labels` (predictions, truth for sessions,
//!   agreement)
//! - `POST start` with a [`StartRequest`] (409 while one runs)
//! - `POST stop`: stops the run; a video's predictions so far are kept
//! - `POST play` `{"seconds": N}`: let AgentZero play for N seconds
//! - `POST release`: stop sending, the controller is back
//! - `POST limits` with [`Limits`]: what the bot may press, kept in the
//!   studio's state file
//! - `POST measure`: measure a person tapping ZR (`{"cancel": true}` stops
//!   it); its progress and result are in the bot's status
//!
//! Each action also goes to the dashboard's WebSocket as an `agent` message
//! ([`Online::subscribe`]), for the page's overlay; the bot's status goes
//! there with the studio's (see [`crate::web`]), so every app shows Stop bot
//! while it plays.

pub mod limits;

use super::{
    BODY_LIMIT, CHECKPOINT_FILE, Checkpoint, Job, JobState, KILL_AFTER, LOG_LINES, MAX_WINDOW,
    PRED_FILE, Predictor, RUN_FILE, Source, Status as HttpError, agreement, blocking, check_name,
    checkpoints_in, gpu_memory, help_has, now_ms, signal_group,
};
use crate::detector::recording_in_progress;
use crate::dump::{Dumper, Frame};
use crate::objects::write_atomic;
use crate::recorder::RecorderState;
use crate::replay::Action;
use crate::studio::Studio;
use crate::video::{self, PolicyFeed, SharedFrame};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use gameplay_data::labels::{self, Label};
use limits::{Limiter, Limits, Tapping, TappingStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Instant, SystemTime};
use tokio::sync::watch;
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// The policy checkpoints' folder under AgentZero's `runs/` (one level down,
/// so the IDM's list does not show them)
const POLICY_RUNS: &str = "policy";

/// A policy's stored runs are named `policy-<checkpoint>`, beside the IDM's
const STORED_PREFIX: &str = "policy-";

/// Frames per second the policy works at
pub const FPS: f64 = 30.0;

/// Actions the timings are taken over (5 s at 30 fps)
const WINDOW: usize = 150;

/// Capture times of the frames piped to the policy, kept to match its
/// actions to them
const HANDED: usize = 256;

/// Frames waiting for the policy's pipe; more are skipped (it only wants the
/// newest)
const FEED_QUEUE: usize = 2;

/// The button a person's tapping is measured on: the one a turbo repeats
pub const TAPPED: &str = "zr";

/// Longest AgentZero plays per confirmation, in seconds
pub const MAX_PLAY_S: f64 = 600.0;

/// Sending stops when no dashboard page has been open this long
pub const PAGE_GONE: Duration = Duration::from_secs(5);

/// Sending stops when no action came this long (the policy stalled): the
/// Switch would keep the last one
pub const STALL: Duration = Duration::from_millis(500);

/// How often the watchdog looks at the rules above
const WATCH_EVERY: Duration = Duration::from_millis(100);

/// Keys of an action line that stay in a stored prediction
const KEPT_EXTRA: [&str; 4] = ["button_probs", "camera_turn", "seen", "model_ms"];

/// Unix time in ms, with fractions
fn unix_ms() -> f64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

/// A request to start the online mode
#[derive(Clone, Debug, Deserialize)]
pub struct StartRequest {
    /// The video to play; none for the live capture
    #[serde(default)]
    pub source: Option<Source>,
    /// Start of the video's range, in seconds
    #[serde(default)]
    pub start_s: Option<f64>,
    /// End of the range
    #[serde(default)]
    pub end_s: Option<f64>,
    /// Folder under `runs/policy/` holding `best.pt`
    pub checkpoint: String,
    /// Run the policy on the CPU
    #[serde(default)]
    pub cpu: bool,
    /// Run while the studio records
    #[serde(default)]
    pub allow_recording: bool,
}

/// The online mode as the page sees it
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub id: u64,
    /// On the live capture, not a video
    pub live: bool,
    pub source: Option<Source>,
    /// The video's name, or the live input's
    pub title: String,
    pub checkpoint: String,
    pub cpu: bool,
    pub allow_recording: bool,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    /// Running (loading while `loading`), done, failed or cancelled (stopped)
    pub state: JobState,
    /// Loading the model, before the first frame
    pub loading: bool,
    /// Told to stop and not ended yet
    pub stopping: bool,
    pub error: Option<String>,
    /// The last lines of the command's output
    pub log: Vec<String>,
    /// The command line, for running it by hand
    pub command: String,
    /// What the policy runs on, from its first line
    pub device: Option<String>,
    /// Frames between the frame seen and the action predicted
    pub lead: Option<u64>,
    /// Frame rate of the predictions
    pub fps: f64,
    /// Frame `n` of the predictions is frame `n + frame_offset` of the video
    pub frame_offset: u64,
    /// Query of `/api/cuttlefish/video` that plays the video
    pub play: BTreeMap<String, String>,
    /// Session and segment, for the truth
    pub session: Option<(String, String)>,
    /// Folder of the video under the results
    pub key: Option<String>,
    /// Actions predicted, and frames the policy skipped
    pub frames: u64,
    pub skipped: u64,
    /// The last frame the policy saw
    pub seen: Option<u64>,
    /// Latency over the last actions (see the module docs): `handoff` with
    /// its parts `grab`, `pipe` and `wait`, `model`, `send`, `total` (live),
    /// `age` (a video's frame from its time to its action), each `{median,
    /// p95}` ms, and `rate` (actions per second)
    pub timings: Value,
    /// The stored run's checkpoint name, once kept
    pub stored: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

/// One action's times, for [`Status::timings`]
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    /// Unix ms when the studio had the action
    at: f64,
    model: f64,
    /// The frame's hand-off, and its parts: from the capture until the
    /// studio wrote it into the pipe (`grab`), on until the policy had it
    /// (`pipe`: the pipe and ffmpeg's scaling) and until the model started
    /// on it (`wait`: the model busy with the frame before)
    handoff: Option<f64>,
    grab: Option<f64>,
    pipe: Option<f64>,
    wait: Option<f64>,
    send: Option<f64>,
    total: Option<f64>,
    age: Option<f64>,
}

/// A frame piped to the policy: its number there, when it was captured and
/// when the studio began writing it (Unix ms)
#[derive(Clone, Copy, Debug, PartialEq)]
struct Handed {
    number: u64,
    captured_ms: u64,
    written_ms: f64,
}

/// The frames piped lately, shared by the writer and the reader of actions
type HandedLog = Mutex<VecDeque<Handed>>;

/// Median and 95th percentile (nearest rank) of some ms
fn spread(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let rank = |share: f64| {
        values[((share * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1]
    };
    json!({ "median": rank(0.5), "p95": rank(0.95) })
}

/// The timings of [`Status::timings`] over some samples
fn timings(samples: &VecDeque<Sample>) -> Value {
    let pick = |f: fn(&Sample) -> Option<f64>| spread(samples.iter().filter_map(f).collect());
    let rate = match (samples.front(), samples.back()) {
        (Some(first), Some(last)) if last.at > first.at => {
            Some((samples.len() - 1) as f64 * 1000.0 / (last.at - first.at))
        }
        _ => None,
    };
    json!({
        "handoff": pick(|s| s.handoff),
        "grab": pick(|s| s.grab),
        "pipe": pick(|s| s.pipe),
        "wait": pick(|s| s.wait),
        "model": pick(|s| Some(s.model)),
        "send": pick(|s| s.send),
        "total": pick(|s| s.total),
        "age": pick(|s| s.age),
        "rate": rate,
    })
}

/// The current or last run, with what it predicted
struct Run {
    status: Status,
    samples: VecDeque<Sample>,
    /// A video's predictions by frame (of the policy's numbering)
    predictions: BTreeMap<u64, Label>,
    /// Where to keep them: the video's results folder
    dir: Option<PathBuf>,
}

/// What `agentzero-play --help` offers, read once
#[derive(Clone, Debug, Default, Serialize)]
pub struct PlayCapabilities {
    /// `--json`: lines the studio can follow (without it, nothing runs)
    pub json: bool,
    /// Why the help could not be read
    pub error: Option<String>,
}

/// Runs the policy and follows it; plays through the studio's [`Bot`]
pub struct Online {
    predictor: Arc<Predictor>,
    studio: Arc<Studio>,
    run: Mutex<Option<Run>>,
    /// The running `uv`, leader of the run's process group, until its output
    /// ends and the run's thread takes it back to reap it
    child: Mutex<Option<Child>>,
    /// The current run was told to stop
    stop: AtomicBool,
    next_id: Mutex<u64>,
    capabilities: Mutex<Option<PlayCapabilities>>,
    /// Each action, for the dashboard's WebSocket
    ticks: watch::Sender<String>,
}

impl Online {
    /// The online mode of `predictor`, taking frames from `studio`'s capture
    /// and playing through its bot; starts the watchdog that ends sending
    pub fn new(predictor: Arc<Predictor>, studio: Arc<Studio>) -> Arc<Self> {
        let online = Arc::new(Self {
            predictor,
            studio,
            run: Mutex::default(),
            child: Mutex::default(),
            stop: AtomicBool::new(false),
            next_id: Mutex::new(1),
            capabilities: Mutex::default(),
            ticks: watch::Sender::new(String::new()),
        });
        let watching = Arc::clone(&online);
        std::thread::Builder::new()
            .name("agentzero-watch".to_string())
            .spawn(move || watching.watch())
            .expect("cannot start a thread");
        online
    }

    /// The actions as they come, for a dashboard page
    pub fn subscribe(&self) -> watch::Receiver<String> {
        self.ticks.subscribe()
    }

    /// The current or last run
    pub fn status(&self) -> Option<Status> {
        let run = self.run.lock().unwrap();
        let run = run.as_ref()?;
        let mut status = run.status.clone();
        status.timings = timings(&run.samples);
        Some(status)
    }

    fn update(&self, f: impl FnOnce(&mut Run)) {
        if let Some(run) = self.run.lock().unwrap().as_mut() {
            f(run);
        }
    }

    fn running(&self) -> bool {
        self.run
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|run| run.status.state == JobState::Running)
    }

    /// `runs/policy/*/best.pt` in AgentZero, newest first
    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        checkpoints_in(&self.policy_runs())
    }

    fn policy_runs(&self) -> PathBuf {
        self.predictor
            .settings
            .agentzero
            .join("runs")
            .join(POLICY_RUNS)
    }

    /// What `agentzero-play --help` offers, read once (again with `refresh`)
    pub fn capabilities(&self, refresh: bool) -> PlayCapabilities {
        let mut known = self.capabilities.lock().unwrap();
        if let Some(capabilities) = known.as_ref()
            && !refresh
        {
            return capabilities.clone();
        }
        let capabilities = match Command::new("uv")
            .args(["run", "agentzero-play", "--help"])
            .current_dir(&self.predictor.settings.agentzero)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => PlayCapabilities {
                json: help_has(&String::from_utf8_lossy(&output.stdout), "--json"),
                error: None,
            },
            Ok(output) => PlayCapabilities {
                json: false,
                error: Some(format!(
                    "agentzero-play --help failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .last()
                        .unwrap_or_default()
                )),
            },
            Err(e) => PlayCapabilities {
                json: false,
                error: Some(format!("cannot run uv: {e}")),
            },
        };
        *known = Some(capabilities.clone());
        capabilities
    }

    /// Checkpoints, what `agentzero-play` can do, GPU memory, the capture
    pub fn info(&self, refresh: bool) -> Value {
        json!({
            "checkpoints": self.checkpoints(),
            "folder": self.policy_runs(),
            "capabilities": self.capabilities(refresh),
            "gpu": gpu_memory(),
            "input": self.studio.video.input(),
        })
    }

    /// Start the policy on a thread of its own
    pub fn start(self: &Arc<Self>, request: StartRequest) -> Result<Status> {
        check_name(&request.checkpoint)?;
        let checkpoint = self
            .policy_runs()
            .join(&request.checkpoint)
            .join(CHECKPOINT_FILE);
        ensure!(checkpoint.is_file(), "no {}", checkpoint.display());
        if let (Some(start), Some(end)) = (request.start_s, request.end_s) {
            ensure!(
                start >= 0.0 && end > start,
                "the range ends before it starts"
            );
        }
        if !request.allow_recording {
            let busy = self.studio.recorder.status().state != RecorderState::Idle;
            let other = recording_in_progress(&self.predictor.inspector.root());
            ensure!(
                !busy && other.is_none(),
                "the studio is recording{}: tick \"allow while recording\" to run AgentZero anyway",
                other.map(|s| format!(" ({s})")).unwrap_or_default()
            );
        }
        ensure!(
            self.capabilities(false).json,
            "this agentzero-play has no --json yet; update AgentZero, then Recheck"
        );
        let mut args: Vec<String> = ["run", "agentzero-play", "--checkpoint"]
            .map(String::from)
            .to_vec();
        args.push(checkpoint.display().to_string());
        let (title, video) = match &request.source {
            None => {
                let input = self.studio.video.input().context(
                    "the Studio has no video input: choose the capture card there first",
                )?;
                ensure!(
                    request.start_s.is_none() && request.end_s.is_none(),
                    "the live capture has no range"
                );
                args.push("--capture".to_string());
                args.push(video::raw_input(FPS as u32).join(" "));
                (input, None)
            }
            Some(source) => {
                let video = self.predictor.video(source)?;
                args.extend(["--video".to_string(), video.path.display().to_string()]);
                if let Some(start) = request.start_s {
                    args.extend(["--start-s".to_string(), start.to_string()]);
                }
                if let Some(end) = request.end_s {
                    args.extend(["--end-s".to_string(), end.to_string()]);
                }
                args.push("--realtime".to_string());
                (video.title.clone(), Some(video))
            }
        };
        args.extend(["--dry-run", "--json"].map(String::from));
        if request.cpu {
            args.push("--cpu".to_string());
        }
        // Only sessions at the policy's rate line up with their truth
        let session = video
            .as_ref()
            .and_then(|v| v.session.clone())
            .filter(|(s, seg)| {
                self.predictor
                    .inspector
                    .info(s, Some(seg))
                    .ok()
                    .and_then(|info| info["fps"].as_f64())
                    .is_some_and(|fps| (fps - FPS).abs() < 0.01)
            });
        let dir = match &video {
            Some(video) => Some(self.predictor.run_dir(
                &video.key,
                &format!("{STORED_PREFIX}{}", request.checkpoint),
            )?),
            None => None,
        };

        let mut current = self.run.lock().unwrap();
        if current
            .as_ref()
            .is_some_and(|run| run.status.state == JobState::Running)
        {
            bail!("AgentZero is running already; stop it first");
        }
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next - 1
        };
        let status = Status {
            id,
            live: video.is_none(),
            source: request.source.clone(),
            title,
            checkpoint: request.checkpoint.clone(),
            cpu: request.cpu,
            allow_recording: request.allow_recording,
            start_s: request.start_s,
            end_s: request.end_s,
            state: JobState::Running,
            loading: true,
            stopping: false,
            error: None,
            log: Vec::new(),
            command: format!(
                "cd {} && uv {}",
                self.predictor.settings.agentzero.display(),
                args.iter()
                    .map(|a| if a.contains(' ') {
                        format!("\"{a}\"")
                    } else {
                        a.clone()
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            device: None,
            lead: None,
            fps: FPS,
            frame_offset: (request.start_s.unwrap_or(0.0) * FPS).round().max(0.0) as u64,
            play: video.as_ref().map(|v| v.play.clone()).unwrap_or_default(),
            session,
            key: video.as_ref().map(|v| v.key.clone()),
            frames: 0,
            skipped: 0,
            seen: None,
            timings: Value::Null,
            stored: None,
            started_ms: now_ms(),
            finished_ms: None,
        };
        // Before the run shows, so a stop of it is not undone
        self.stop.store(false, Ordering::Relaxed);
        *current = Some(Run {
            status: status.clone(),
            samples: VecDeque::new(),
            predictions: BTreeMap::new(),
            dir,
        });
        drop(current);
        log::info!(
            "AgentZero {} on {}",
            request.checkpoint,
            if status.live {
                "the live capture"
            } else {
                &status.title
            }
        );
        let online = Arc::clone(self);
        let live = status.live;
        std::thread::Builder::new()
            .name("agentzero".to_string())
            .spawn(move || {
                let started = Instant::now();
                let result = online.work(&args, live);
                // Nothing reaches the pipe or the proxy any more
                online.studio.video.set_policy_feed(None);
                online.studio.bot.release(Ended::Stopped);
                let stopped = online.stop.load(Ordering::Relaxed);
                online.update(|run| {
                    let status = &mut run.status;
                    status.finished_ms = Some(now_ms());
                    status.stopping = false;
                    status.loading = false;
                    status.state = match &result {
                        Ok(()) => JobState::Done,
                        Err(_) if stopped => JobState::Cancelled,
                        Err(e) => {
                            status.error = Some(format!("{e:#}"));
                            log::warn!("AgentZero failed: {:#}", e);
                            JobState::Failed
                        }
                    };
                });
                if let Err(e) = online.store(started.elapsed()) {
                    log::warn!("Cannot keep AgentZero's predictions: {:#}", e);
                    online.update(|run| run.status.error = Some(format!("{e:#}")));
                }
            })?;
        Ok(status)
    }

    /// Run the command and follow its lines until it ends
    fn work(self: &Arc<Self>, args: &[String], live: bool) -> Result<()> {
        let (stdin, stdout, stderr) = {
            let mut slot = self.child.lock().unwrap();
            // Under the lock: a stop before this point means nothing runs,
            // one after it finds the process
            ensure!(!self.stop.load(Ordering::Relaxed), "stopped");
            let mut child = Command::new("uv")
                .args(args)
                .current_dir(&self.predictor.settings.agentzero)
                .stdin(if live { Stdio::piped() } else { Stdio::null() })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // `uv run` starts Python, which starts ffmpeg: a stop
                // signals them all
                .process_group(0)
                .spawn()
                .context("cannot run uv")?;
            let stdin = child.stdin.take();
            let stdout = child.stdout.take().context("no stdout")?;
            let stderr = child.stderr.take().context("no stderr")?;
            *slot = Some(child);
            (stdin, stdout, stderr)
        };
        let online = Arc::clone(self);
        let reader = std::thread::spawn(move || {
            let mut log = VecDeque::new();
            crate::cuttlefish::for_each_line(stderr, |line| {
                if line.trim().is_empty() {
                    return;
                }
                log.push_back(line.to_string());
                if log.len() > LOG_LINES {
                    log.pop_front();
                }
                let lines: Vec<String> = log.iter().cloned().collect();
                online.update(|run| run.status.log = lines);
            });
        });
        let handed: Arc<HandedLog> = Arc::default();
        let mut stdin = stdin;
        crate::cuttlefish::for_each_line(stdout, |line| {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                return;
            };
            match value["event"].as_str() {
                Some("start") => {
                    self.update(|run| {
                        run.status.loading = false;
                        run.status.device = value["device"].as_str().map(String::from);
                        run.status.lead = value["lead"].as_u64();
                    });
                    // The model is ready: frames from now on
                    if let Some(stdin) = stdin.take()
                        && !self.stop.load(Ordering::Relaxed)
                    {
                        self.feed(stdin, Arc::clone(&handed));
                    }
                }
                Some("action") => {
                    if let Err(e) = self.on_action(value, &handed) {
                        log::warn!("AgentZero's action: {:#}", e);
                    }
                }
                _ => {}
            }
        });
        let _ = reader.join();
        // The output ended with the command
        let child = self.child.lock().unwrap().take();
        let status = child.context("the run was lost")?.wait()?;
        if self.stop.load(Ordering::Relaxed) {
            bail!("stopped");
        }
        if !status.success() {
            let last = self
                .status()
                .and_then(|s| s.log.last().cloned())
                .unwrap_or_default();
            bail!("agentzero-play failed ({status}): {last}");
        }
        Ok(())
    }

    /// Pipe the grabbed frames into the policy, thinned to its rate, noting
    /// each one's capture time by its number
    fn feed(&self, stdin: ChildStdin, handed: Arc<HandedLog>) {
        // Megabyte frames: a bigger pipe means fewer wakeups. Best effort.
        // SAFETY: fcntl on a descriptor we own
        unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 22) };
        let (frames, queued) = sync_channel(FEED_QUEUE);
        std::thread::spawn(move || write_frames(stdin, queued, &handed));
        self.studio.video.set_policy_feed(Some(PolicyFeed {
            frames,
            fps: FPS as u32,
        }));
    }

    /// One action line: timings, the bot, the page, the predictions
    fn on_action(&self, value: Value, handed: &HandedLog) -> Result<()> {
        let got = unix_ms();
        let number = |key: &str| value[key].as_f64();
        let seen = value["seen"].as_u64().context("no seen")?;
        let model = number("model_ms").unwrap_or_default();
        let ready = number("ready_ms").unwrap_or(got);
        let (id, live) = self
            .run
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, false), |run| (run.status.id, run.status.live));
        let mut sample = Sample {
            at: got,
            model,
            ..Default::default()
        };
        // What went to the Switch, after the bot's limits
        let mut sent = None;
        if live {
            let action = Action::parse(&value["send"].to_string()).context("bad send")?;
            sent = self.studio.bot.send(&action);
            let done = sent.as_ref().map_or(got, |(at, _)| *at);
            let frame = handed
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|frame| frame.number == seen)
                .copied();
            sample.send = Some(done - ready);
            if let Some(frame) = frame {
                let captured = frame.captured_ms as f64;
                let began = ready - model;
                let arrived = number("arrived_ms").unwrap_or(began);
                sample.handoff = Some(began - captured);
                sample.grab = Some(frame.written_ms - captured);
                sample.pipe = Some(arrived - frame.written_ms);
                sample.wait = Some(began - arrived);
                sample.total = Some(done - captured);
            }
        } else if let Some(arrived) = number("arrived_ms") {
            sample.age = Some(ready - arrived);
        }
        let label: Label = serde_json::from_value(value.clone()).context("bad label")?;
        let skipped = value["skipped"].as_u64().unwrap_or_default();
        self.update(|run| {
            if run.samples.len() >= WINDOW {
                run.samples.pop_front();
            }
            run.samples.push_back(sample);
            run.status.frames += 1;
            run.status.skipped = skipped;
            run.status.seen = Some(seen);
            if !run.status.live {
                let mut kept = label.clone();
                kept.extra
                    .retain(|key, _| KEPT_EXTRA.contains(&key.as_str()));
                run.predictions.insert(kept.frame, kept);
            }
        });
        let mut tick = value;
        if let Some(fields) = tick.as_object_mut() {
            for key in ["event", "arrived_ms", "ready_ms"] {
                fields.remove(key);
            }
            fields.insert("type".into(), json!("agent"));
            fields.insert("id".into(), json!(id));
            fields.insert("sent".into(), json!(sent.is_some()));
            // The page draws what reached the Switch while it plays
            if let Some((_, line)) = &sent {
                fields.insert("send".into(), json!(line));
            }
            fields.insert("total_ms".into(), json!(sample.total));
        }
        self.ticks.send_replace(tick.to_string());
        Ok(())
    }

    /// Keep a video run's predictions as a run of the Predictor
    fn store(&self, took: Duration) -> Result<()> {
        let (job, labels, dir) = {
            let run = self.run.lock().unwrap();
            let Some(run) = run.as_ref() else {
                return Ok(());
            };
            let (Some(dir), Some(source), Some(key)) =
                (&run.dir, &run.status.source, &run.status.key)
            else {
                return Ok(());
            };
            if run.predictions.is_empty() {
                return Ok(());
            }
            let status = &run.status;
            let job = Job {
                id: status.id,
                source: source.clone(),
                title: status.title.clone(),
                key: key.clone(),
                checkpoint: format!("{STORED_PREFIX}{}", status.checkpoint),
                start_s: status.start_s,
                end_s: status.end_s,
                cpu: status.cpu,
                state: JobState::Done,
                stopping: false,
                done: status.frames,
                total: None,
                error: None,
                out_of_memory: false,
                log: status.log.clone(),
                command: status.command.clone(),
                fps: Some(FPS),
                frame_offset: status.frame_offset,
                frames: run.predictions.len(),
                play: status.play.clone(),
                session: status.session.clone(),
                started_ms: status.started_ms,
                finished_ms: status.finished_ms,
                seconds: Some((took.as_secs_f64() * 10.0).round() / 10.0),
            };
            let labels: Vec<Label> = run.predictions.values().cloned().collect();
            (job, labels, dir.clone())
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        let partial = dir.join(format!("{PRED_FILE}.part"));
        labels::write_labels(&partial, &labels)?;
        std::fs::rename(&partial, dir.join(PRED_FILE))?;
        write_atomic(&dir.join(RUN_FILE), &serde_json::to_vec_pretty(&job)?)?;
        log::info!(
            "Kept {} of AgentZero's actions in {}",
            labels.len(),
            dir.display()
        );
        self.update(|run| run.status.stored = Some(job.checkpoint.clone()));
        Ok(())
    }

    /// Stop the run: the frames and the bot first, then SIGTERM to its
    /// process group, SIGKILL after [`KILL_AFTER`] to what is left
    pub fn stop(self: &Arc<Self>) {
        self.stop.store(true, Ordering::Relaxed);
        self.studio.video.set_policy_feed(None);
        self.studio.bot.release(Ended::Stopped);
        self.update(|run| run.status.stopping = run.status.state == JobState::Running);
        let pid = {
            let child = self.child.lock().unwrap();
            let Some(child) = child.as_ref() else {
                return;
            };
            log::info!("Stopping AgentZero (pid {})", child.id());
            signal_group(child, libc::SIGTERM);
            child.id()
        };
        let online = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(KILL_AFTER);
            if let Some(child) = online
                .child
                .lock()
                .unwrap()
                .as_ref()
                .filter(|child| child.id() == pid)
            {
                log::warn!("AgentZero outlived SIGTERM; killing it");
                signal_group(child, libc::SIGKILL);
            }
        });
    }

    /// Stop a run at the studio's exit and wait for it to end
    pub fn shutdown(self: &Arc<Self>) {
        if !self.running() {
            return;
        }
        self.stop();
        let deadline = Instant::now() + KILL_AFTER + Duration::from_secs(1);
        while self.running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Let AgentZero play for `seconds` (see the module docs)
    pub fn play(&self, seconds: f64) -> Result<()> {
        let status = self
            .status()
            .context("start AgentZero on the live capture first")?;
        ensure!(
            status.live && status.state == JobState::Running && !status.loading,
            "AgentZero must be running on the live capture"
        );
        ensure!(
            !self.studio.player.status().playing,
            "the Replay panel is playing to the proxy; stop it first"
        );
        // What reaches the Switch shows in those frames only
        ensure!(
            self.studio.link.connected.load(Ordering::Relaxed),
            "the proxy's frames do not reach the studio, so what reaches the Switch \
             could not be seen; connect the proxy first"
        );
        self.studio.bot.play(seconds)
    }

    /// Measure a person tapping [`TAPPED`] as fast as they can, from the
    /// proxy's frames (see [`limits::Tapping`])
    pub fn measure(&self) -> Result<()> {
        // Replayed lines would count as the person's presses
        ensure!(
            !self.studio.player.status().playing,
            "the Replay panel is playing to the proxy; stop it first"
        );
        ensure!(
            self.studio.link.connected.load(Ordering::Relaxed),
            "the proxy's frames do not reach the studio: connect the proxy first"
        );
        self.studio.bot.measure(TAPPED)
    }

    /// End sending on the rules of the module docs, every [`WATCH_EVERY`];
    /// and stop a run that may not run while the studio records
    fn watch(self: Arc<Self>) {
        let mut page_seen = Instant::now();
        loop {
            std::thread::sleep(WATCH_EVERY);
            if self.ticks.receiver_count() > 0 {
                page_seen = Instant::now();
            }
            let reason = if page_seen.elapsed() > PAGE_GONE {
                Some(Ended::Page)
            } else if self.studio.player.status().playing {
                Some(Ended::Replay)
            } else if !self.studio.link.connected.load(Ordering::Relaxed) {
                Some(Ended::Link)
            } else {
                None
            };
            match reason {
                Some(reason) => self.studio.bot.release(reason),
                None => self.studio.bot.check(),
            }
            let recording = self.studio.recorder.status().state != RecorderState::Idle;
            let forbidden = self.run.lock().unwrap().as_ref().is_some_and(|run| {
                let status = &run.status;
                status.state == JobState::Running && !status.allow_recording && !status.stopping
            });
            if recording && forbidden {
                log::info!("The studio started recording: stopping AgentZero");
                self.update(|run| {
                    run.status.error = Some(
                        "stopped: the studio started recording (tick \"allow while recording\" \
                         to keep AgentZero running)"
                            .to_string(),
                    )
                });
                self.stop();
            }
        }
    }

    /// Frames `[start, stop)` of the current video run, as the Predictor's
    /// `labels`
    pub fn labels(&self, start: usize, stop: usize) -> Result<Value> {
        ensure!(stop >= start, "stop comes before start");
        ensure!(
            stop - start <= MAX_WINDOW,
            "at most {MAX_WINDOW} frames at once"
        );
        let (pred, session) = {
            let run = self.run.lock().unwrap();
            let run = run.as_ref().context("AgentZero has not run")?;
            ensure!(!run.status.live, "the live capture has no frames to show");
            let offset = run.status.frame_offset;
            let pred: Vec<Option<Label>> = (start..stop)
                .map(|n| {
                    (n as u64)
                        .checked_sub(offset)
                        .and_then(|k| run.predictions.get(&k))
                        .map(|label| Label {
                            frame: label.frame + offset,
                            ..label.clone()
                        })
                })
                .collect();
            (pred, run.status.session.clone())
        };
        let truth = self.predictor.truth(session.as_ref(), start, stop)?;
        let agreement = truth
            .as_ref()
            .map(|truth| agreement(truth, &pred[..truth.len()]));
        Ok(json!({ "start": start, "pred": pred, "truth": truth, "agreement": agreement }))
    }
}

/// Pipe frames into the policy's stdin until the feed stops, noting each
/// frame's number, capture time and time written as it goes in
fn write_frames(mut stdin: impl Write, frames: Receiver<(u64, SharedFrame)>, handed: &HandedLog) {
    for (number, (captured_ms, frame)) in (0u64..).zip(frames) {
        {
            let mut handed = handed.lock().unwrap();
            if handed.len() >= HANDED {
                handed.pop_front();
            }
            handed.push_back(Handed {
                number,
                captured_ms,
                written_ms: unix_ms(),
            });
        }
        if stdin.write_all(&frame).is_err() {
            return;
        }
    }
}

// ------------------------------------------------------------------ the bot

/// The bot's hold on the proxy's replay port while AgentZero plays, with
/// what it may press ([`Limits`]); a [`Dumper`] of the frames the proxy
/// streams, for measuring a person's tapping. See the module docs.
#[derive(Clone)]
pub struct Bot(Arc<BotInner>);

struct BotInner {
    /// `host:port` of the proxy's replay port
    address: String,
    state: Mutex<BotState>,
}

#[derive(Default)]
struct BotState {
    /// The replay connection while AgentZero plays
    stream: Option<TcpStream>,
    /// End of the time confirmed
    until: Option<Instant>,
    until_ms: u64,
    /// When the last action went out
    last_sent: Option<Instant>,
    /// Actions written to the replay port since it was let play
    sent: u64,
    /// Why sending last ended
    ended: Option<Ended>,
    /// What it may press, and the presses it made lately
    limits: Limits,
    limiter: Limiter,
    /// A person's tapping, measured
    tapping: Option<Tapping>,
}

/// Why AgentZero stopped playing the Switch
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ended {
    /// The time confirmed is up
    Time,
    /// Stop bot on the page, or Esc
    You,
    /// No dashboard page was open for [`PAGE_GONE`]
    Page,
    /// The Replay panel started playing to the proxy
    Replay,
    /// The proxy's frames stopped reaching the studio: what reaches the
    /// Switch could not be seen
    Link,
    /// No action came for [`STALL`]
    Stall,
    /// The online mode stopped
    Stopped,
    /// A write to the replay port failed
    Proxy,
}

/// What the page shows of the bot
#[derive(Clone, Debug, Serialize)]
pub struct BotStatus {
    /// Playing: its actions go to the Switch
    pub playing: bool,
    /// End of the time confirmed (Unix ms)
    pub until_ms: Option<u64>,
    pub sent: u64,
    /// Why sending last ended
    pub ended: Option<Ended>,
    /// Where it sends
    pub address: String,
    /// What it may press
    pub limits: Limits,
    /// The last measurement of a person's tapping
    pub tapping: Option<TappingStatus>,
}

impl BotState {
    /// Write one line; a failed write ends sending
    fn write(&mut self, action: &Action) -> bool {
        let Some(stream) = self.stream.as_mut() else {
            return false;
        };
        let mut line = serde_json::to_vec(action).unwrap_or_default();
        line.push(b'\n');
        if let Err(e) = stream.write_all(&line) {
            log::warn!("AgentZero stopped playing: the proxy's replay port: {e}");
            self.stream = None;
            self.until = None;
            self.ended = Some(Ended::Proxy);
            return false;
        }
        true
    }

    fn measuring(&self) -> bool {
        self.tapping.as_ref().is_some_and(Tapping::active)
    }
}

/// Nothing pressed, and no sticks or gyro of its own: mixed, the controller
/// alone (a stick at 2048 would stand in for a resting one, which reads its
/// calibrated centre)
fn neutral() -> Action {
    Action {
        buttons: Some(Vec::new()),
        mix: true,
        ..Default::default()
    }
}

impl Bot {
    /// A bot for the proxy's replay port at `address` (`host:port`), which
    /// may press what `limits` allow
    pub fn new(address: String, limits: Limits) -> Self {
        Self(Arc::new(BotInner {
            address,
            state: Mutex::new(BotState {
                limits,
                ..Default::default()
            }),
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BotState> {
        self.0.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What it may press
    pub fn limits(&self) -> Limits {
        self.lock().limits
    }

    /// From the next action on, while it plays too
    pub fn set_limits(&self, limits: Limits) {
        self.lock().limits = limits;
    }

    /// Connect to the replay port and play for `seconds`
    pub fn play(&self, seconds: f64) -> Result<()> {
        ensure!(
            seconds > 0.0 && seconds <= MAX_PLAY_S,
            "play between 0 and {MAX_PLAY_S} seconds, not {seconds}"
        );
        {
            let state = self.lock();
            ensure!(state.stream.is_none(), "AgentZero is playing already");
            ensure!(
                !state.measuring(),
                "your tapping is being measured; let it finish first"
            );
        }
        let address = &self.0.address;
        let socket = address
            .to_socket_addrs()?
            .next()
            .context("the replay address did not resolve")?;
        let stream = TcpStream::connect_timeout(&socket, Duration::from_secs(3))
            .with_context(|| format!("cannot reach the proxy's replay port {address}"))?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        let mut state = self.lock();
        let now = Instant::now();
        state.stream = Some(stream);
        state.until = Some(now + Duration::from_secs_f64(seconds));
        state.until_ms = now_ms() + (seconds * 1000.0) as u64;
        state.last_sent = Some(now);
        state.sent = 0;
        state.ended = None;
        state.limiter = Limiter::default();
        log::info!("AgentZero plays through {address} for {seconds} s");
        Ok(())
    }

    /// Send one action, its buttons held to the limits, unless not playing;
    /// answers with the Unix ms it was written at and the line written
    pub fn send(&self, action: &Action) -> Option<(f64, Action)> {
        let mut state = self.lock();
        state.stream.as_ref()?;
        let now = Instant::now();
        let wanted = action.buttons.clone().unwrap_or_default();
        let limits = state.limits;
        let buttons = state.limiter.buttons(&wanted, &limits, now);
        let line = Action {
            buttons: Some(buttons),
            mix: true,
            t_ms: None,
            ..action.clone()
        };
        if !state.write(&line) {
            return None;
        }
        state.sent += 1;
        state.last_sent = Some(now);
        Some((unix_ms(), line))
    }

    /// Stop sending: a neutral line, letting go of everything at once, then
    /// the connection closes and the controller is alone again
    pub fn release(&self, reason: Ended) {
        let mut state = self.lock();
        if state.stream.is_none() {
            return;
        }
        state.write(&neutral());
        state.stream = None;
        state.until = None;
        state.ended = Some(reason);
        log::info!("AgentZero stopped playing ({reason:?})");
    }

    /// End sending when its time is up or the policy stalled
    fn check(&self) {
        let reason = {
            let state = self.lock();
            if state.stream.is_none() {
                return;
            }
            if state.until.is_some_and(|until| Instant::now() >= until) {
                Ended::Time
            } else if state.last_sent.is_some_and(|at| at.elapsed() > STALL) {
                Ended::Stall
            } else {
                return;
            }
        };
        self.release(reason);
    }

    /// Measure a person tapping `button` from the frames to come; not while
    /// it plays, whose presses would count
    pub fn measure(&self, button: &str) -> Result<()> {
        let mut state = self.lock();
        ensure!(
            state.stream.is_none(),
            "AgentZero is playing; stop it before measuring your tapping"
        );
        state.tapping = Some(Tapping::new(button));
        log::info!("Measuring your tapping of {button}");
        Ok(())
    }

    /// Stop a measurement under way
    pub fn cancel_measure(&self) {
        if let Some(tapping) = self.lock().tapping.as_mut() {
            tapping.cancel();
        }
    }

    pub fn status(&self) -> BotStatus {
        let state = self.lock();
        BotStatus {
            playing: state.stream.is_some(),
            until_ms: state.stream.as_ref().map(|_| state.until_ms),
            sent: state.sent,
            ended: state.ended,
            address: self.0.address.clone(),
            limits: state.limits,
            tapping: state.tapping.as_ref().map(Tapping::status),
        }
    }
}

/// The proxy's frames: a person's presses while their tapping is measured
impl Dumper for Bot {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let mut state = self.lock();
        let Some(tapping) = state.tapping.as_mut().filter(|t| t.active()) else {
            return Ok(());
        };
        let Some(report) = Action::from_report(frame.payload()) else {
            return Ok(());
        };
        let pressed = report
            .buttons
            .iter()
            .flatten()
            .any(|b| b == tapping.button());
        tapping.report(frame.timestamp_ms, pressed);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

// ------------------------------------------------------------------ HTTP

impl Online {
    fn get(&self, path: &str, query: &HashMap<String, String>) -> Result<Value, HttpError> {
        let text = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
        let number = |key: &str| -> Result<usize> {
            text(key).parse().with_context(|| format!("bad {key}"))
        };
        match path {
            "status" => Ok(json!({
                "run": self.status(),
                "bot": self.studio.bot.status(),
            })),
            "checkpoints" => Ok(self.info(text("refresh") == "1")),
            "labels" => Ok(self.labels(number("start")?, number("stop")?)?),
            _ => Err(HttpError(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }

    fn post(self: &Arc<Self>, path: &str, body: &[u8]) -> Result<Value, HttpError> {
        match path {
            "start" => {
                let request: StartRequest = serde_json::from_slice(body)
                    .map_err(|e| anyhow::anyhow!("bad request: {e}"))?;
                if self.running() {
                    return Err(HttpError(
                        StatusCode::CONFLICT,
                        anyhow::anyhow!("AgentZero is running already; stop it first"),
                    ));
                }
                Ok(json!(self.start(request)?))
            }
            "stop" => {
                self.stop();
                Ok(json!(self.status()))
            }
            "play" => {
                let seconds = serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["seconds"].as_f64())
                    .context("give seconds")?;
                self.play(seconds)?;
                Ok(json!(self.studio.bot.status()))
            }
            "release" => {
                self.studio.bot.release(Ended::You);
                Ok(json!(self.studio.bot.status()))
            }
            "limits" => {
                let limits: Limits =
                    serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad limits: {e}"))?;
                self.studio.set_bot_limits(limits)?;
                Ok(json!(self.studio.bot.status()))
            }
            "measure" => {
                let cancel = serde_json::from_slice::<Value>(body)
                    .is_ok_and(|v| v["cancel"].as_bool() == Some(true));
                if cancel {
                    self.studio.bot.cancel_measure();
                } else {
                    self.measure()?;
                }
                Ok(json!(self.studio.bot.status()))
            }
            _ => Err(HttpError(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST {path}"),
            )),
        }
    }
}

/// The routes under `/api/predictor/online/`; files and processes are handled
/// off the async workers
pub fn routes(online: Arc<Online>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || {
        warp::path("api")
            .and(warp::path("predictor"))
            .and(warp::path("online"))
    };
    let reader = Arc::clone(&online);
    let get = warp::get()
        .and(base())
        .and(warp::path::tail())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |tail: warp::path::Tail, query: HashMap<String, String>| {
                let online = Arc::clone(&reader);
                blocking(move || online.get(tail.as_str(), &query))
            },
        );
    let post = warp::post()
        .and(base())
        .and(warp::path::tail())
        .and(warp::body::content_length_limit(BODY_LIMIT))
        .and(warp::body::bytes())
        .and_then(
            move |tail: warp::path::Tail, body: warp::hyper::body::Bytes| {
                let online = Arc::clone(&online);
                blocking(move || online.post(tail.as_str(), &body))
            },
        );
    get.or(post).unify().boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(json: &str) -> Action {
        Action::parse(json).unwrap()
    }

    #[test]
    fn neutral_mixed_is_the_controller_alone() {
        let mut report = [0u8; 64];
        report[0] = 0x30;
        report[3] = 0x80; // ZR
        // Left stick pushed and turning; then at rest, off 2048 as real
        // sticks rest, the gyro reading its bias
        for state in [
            r#"{"left_stick": [100, 4000], "gyro": [-300, 20, 5]}"#,
            r#"{"left_stick": [2068, 1876], "right_stick": [2084, 2148], "gyro": [21, -28, 4]}"#,
        ] {
            let mut controller = report;
            action(state).apply(&mut controller);
            let mut mixed = controller;
            neutral().apply(&mut mixed);
            assert_eq!(mixed, controller, "{state}");
        }
    }

    #[test]
    fn spreads() {
        assert_eq!(spread(Vec::new()), Value::Null);
        let s = spread((1..=100).map(f64::from).collect());
        assert_eq!(s["median"], 50.0);
        assert_eq!(s["p95"], 95.0);
        let one = spread(vec![7.0]);
        assert_eq!(
            (one["median"].as_f64(), one["p95"].as_f64()),
            (Some(7.0), Some(7.0))
        );
    }

    #[test]
    fn frames_are_numbered_as_they_go_in() {
        let (frames, queued) = sync_channel(4);
        let handed = HandedLog::default();
        for (ms, byte) in [(1000, 1u8), (1033, 2), (1067, 3)] {
            frames.send((ms, Arc::new(vec![byte; 10]))).unwrap();
        }
        // The feed stops: the writer ends
        drop(frames);
        let mut pipe = Vec::new();
        write_frames(&mut pipe, queued, &handed);
        assert_eq!(pipe.len(), 30);
        assert_eq!(pipe[10], 2);
        let handed = handed.into_inner().unwrap();
        let numbered: Vec<(u64, u64)> = handed.iter().map(|h| (h.number, h.captured_ms)).collect();
        assert_eq!(numbered, [(0, 1000), (1, 1033), (2, 1067)]);
        assert!(handed.iter().all(|h| h.written_ms > 1.7e12));
    }

    /// A stand-in for the proxy's replay port: the lines it got, once the
    /// bot let go
    fn replay_port() -> (String, std::thread::JoinHandle<Vec<Action>>) {
        use std::io::{BufRead, BufReader};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let proxy = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            BufReader::new(stream)
                .lines()
                .map_while(Result::ok)
                .map(|line| action(&line))
                .collect()
        });
        (address, proxy)
    }

    /// A report the proxy read at `read_ms` with `buttons` pressed
    fn report(buttons: &str, read_ms: u64) -> Frame {
        let mut report = [0u8; 64];
        report[0] = 0x30;
        action(&format!(r#"{{"buttons": {buttons}}}"#)).apply(&mut report);
        Frame::new(read_ms, 0, &report)
    }

    #[test]
    fn the_bot_sends_mixed_within_its_limits_and_stops_at_once() {
        let (address, proxy) = replay_port();
        let mut bot = Bot::new(address, Limits::default());
        let wants = |buttons: &str| {
            action(&format!(
                r#"{{"buttons": {buttons}, "left_stick": [2100, 3360], "right_stick": [2048, 2048], "gyro": [0, 0, 0]}}"#
            ))
        };
        // Not playing: nothing goes out
        assert!(bot.send(&wants(r#"["zr"]"#)).is_none());
        assert!(bot.play(0.0).is_err() && bot.play(MAX_PLAY_S + 1.0).is_err());
        bot.play(30.0).unwrap();
        // The d-pad, the special and Home are held back; ZR goes
        let (_, line) = bot
            .send(&wants(r#"["zr", "up", "r_stick", "home"]"#))
            .unwrap();
        assert_eq!(line.buttons, Some(vec!["zr".to_string()]));
        assert!(line.mix);
        // A person pressing B and A on the controller: the bot plays on
        bot.dump(&report(r#"["b", "a"]"#, now_ms())).unwrap();
        // ZR let go at once: held to the shortest press
        let (_, line) = bot.send(&wants("[]")).unwrap();
        assert_eq!(line.buttons, Some(vec!["zr".to_string()]));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(bot.send(&wants("[]")).unwrap().1.buttons, Some(Vec::new()));
        // Pressed again too soon: waits until 130 ms after the last press
        assert_eq!(
            bot.send(&wants(r#"["zr"]"#)).unwrap().1.buttons,
            Some(Vec::new())
        );
        std::thread::sleep(Duration::from_millis(90));
        assert_eq!(
            bot.send(&wants(r#"["zr"]"#)).unwrap().1.buttons,
            Some(vec!["zr".to_string()])
        );
        // Unblocked on the page: the d-pad goes too, Home never
        bot.set_limits(Limits {
            block_dpad: false,
            ..Limits::default()
        });
        assert_eq!(
            bot.send(&wants(r#"["zr", "up", "home"]"#))
                .unwrap()
                .1
                .buttons,
            Some(vec!["up".to_string(), "zr".to_string()])
        );
        assert_eq!(bot.status().sent, 6);
        // Stop: a line that lets go of everything, then nothing more
        bot.release(Ended::You);
        assert!(bot.send(&wants(r#"["zr"]"#)).is_none());
        let status = bot.status();
        assert!(!status.playing);
        assert_eq!(status.ended, Some(Ended::You));
        let lines = proxy.join().unwrap();
        assert_eq!(lines.len(), 7);
        assert!(lines.iter().all(|line| line.mix));
        let last = lines.last().unwrap();
        assert_eq!(last.buttons, Some(Vec::new()));
        assert!(last.left_stick.is_none() && last.gyro.is_none());
    }

    #[test]
    fn a_persons_tapping_is_measured_from_the_proxys_frames() {
        let mut bot = Bot::new("127.0.0.1:9".to_string(), Limits::default());
        bot.measure(TAPPED).unwrap();
        // Every 16 ms, ZR pressed for 48 ms every 112 (8.9 a second)
        for k in 0..800u64 {
            let at = 1_000 + 16 * k;
            let buttons = if (at - 1_000) % 112 < 48 {
                r#"["zr", "zl"]"#
            } else {
                r#"["zl"]"#
            };
            bot.dump(&report(buttons, at)).unwrap();
        }
        let tapping = bot.status().tapping.unwrap();
        assert_eq!(tapping.state, "done");
        let result = tapping.result.unwrap();
        assert_eq!(result.fastest_hz, 8.9);
        assert_eq!(result.cap_hz, 9.8);
        assert_eq!(result.shortest_hold_ms, Some(48));
        // Playing while it measures would count the bot's presses
        bot.measure(TAPPED).unwrap();
        assert!(bot.play(30.0).is_err());
        bot.cancel_measure();
        assert_eq!(bot.status().tapping.unwrap().state, "cancelled");
    }
}
