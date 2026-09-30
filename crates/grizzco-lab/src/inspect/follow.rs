//! Follow: the labeling mode's boxes carried over the next frames
//!
//! The user boxes objects on one frame and asks to follow them; a tracker
//! (AgentZero's `agentzero-track-serve`, SAM 2 behind a small local HTTP
//! service) finds them on the following or preceding frames, and its boxes
//! are written into the annotations as model boxes (`"by": "model"`, with a
//! score and one track `id` per object) for the user to accept or correct.
//!
//! The rules:
//!
//! - labeled frames do not stop a Follow ([`follow_span`]); the rule is per
//!   object ([`apply_followed`]): a followed object's box is added to a
//!   frame unless a person's box of the same class overlaps it (IoU above
//!   [`COVERED_IOU`](crate::inspect::objects::COVERED_IOU)), which stands for it
//!   there, and the object is followed on; a line a person left empty is
//!   not changed, and boxes of people are never changed or removed;
//! - an object the tracker loses (low score, empty mask, sudden jump) is
//!   not followed any further;
//! - on the frames a Follow covers, the model boxes of the same track ids
//!   give way to the new ones, so following again from a corrected frame
//!   replaces what drifted.
//!
//! One Follow runs at a time, on a thread; boxes are written every
//! [`WRITE_EVERY`] frames so they show up while it runs. The page talks only
//! to the studio, which calls the tracker (`[inspect] tracker`) and can
//! start it (`[inspect] tracker_command` in `tracker_dir`).
//!
//! Endpoints under `/api/inspect/follow/`:
//!
//! - `GET status`: whether the tracker answers, its device and memory, how
//!   to start it, and the current or last Follow
//! - `GET job`: the current or last Follow alone (`null` before the first),
//!   for polling
//! - `POST run` with a [`FollowRequest`] starts a Follow (409 while one runs)
//! - `POST cancel` stops the current Follow; what was written stays
//! - `POST start` starts the tracker with the configured command
//!
//! Errors are `{"error": "..."}` with status 400 (or 409, 503).

use crate::config::InspectConfig;
use crate::inspect::Inspector;
use crate::inspect::objects::{FollowWrite, ObjectBox, follow_span};
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// Where the tracker listens unless `[inspect] tracker` says otherwise
pub const DEFAULT_TRACKER: &str = "http://127.0.0.1:7340";

/// Most frames one Follow covers per direction
pub const MAX_COUNT: u64 = 1800;

/// Frames tracked between writes to the annotations
pub const WRITE_EVERY: usize = 10;

/// Largest request body
const BODY_LIMIT: u64 = 256 << 10;

/// Follow's settings, from `[inspect]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The tracker's base URL
    pub url: String,
    /// Command that starts the tracker
    pub command: Vec<String>,
    /// Folder the command runs in
    pub dir: PathBuf,
}

impl Settings {
    /// Settings from `[inspect]`; relative paths start at `config_dir`
    pub fn from_config(config: &InspectConfig, config_dir: &Path) -> Result<Self> {
        let url = config
            .tracker
            .as_deref()
            .unwrap_or(DEFAULT_TRACKER)
            .trim_end_matches('/')
            .to_string();
        ensure!(
            url.starts_with("http://"),
            "[inspect] tracker must be an http:// URL, not {url:?}"
        );
        let command = match &config.tracker_command {
            Some(command) => command.clone(),
            None => {
                let mut command = ["uv", "run", "agentzero-track-serve"]
                    .map(String::from)
                    .to_vec();
                if let Some(port) = port_of(&url) {
                    command.extend(["--port".to_string(), port.to_string()]);
                }
                command
            }
        };
        Ok(Self {
            url,
            command,
            dir: config_dir.join(config.tracker_dir.as_deref().unwrap_or("../AgentZero")),
        })
    }

    /// The command as the page shows it, to run by hand
    fn command_line(&self) -> String {
        format!("cd {} && {}", self.dir.display(), self.command.join(" "))
    }
}

/// The port of an `http://host:port` URL
pub(crate) fn port_of(url: &str) -> Option<u16> {
    let host = url.strip_prefix("http://")?.split('/').next()?;
    host.rsplit_once(':')?.1.parse().ok()
}

fn default_count() -> u64 {
    60
}

fn default_device() -> String {
    "auto".to_string()
}

fn default_min_score() -> f64 {
    0.5
}

/// Which way a Follow goes from its frame
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Towards the end of the video
    #[default]
    Forward,
    /// Towards its start
    Backward,
    /// Forward, then backward
    Both,
}

impl Direction {
    /// The passes to make: `true` forward, `false` backward
    fn passes(self) -> &'static [bool] {
        match self {
            Direction::Forward => &[true],
            Direction::Backward => &[false],
            Direction::Both => &[true, false],
        }
    }
}

/// What `POST run` asks for
#[derive(Clone, Debug, Deserialize)]
pub struct FollowRequest {
    /// Session and segment file
    pub s: String,
    #[serde(default)]
    pub seg: String,
    /// The frame the boxes are drawn on
    pub frame: u64,
    /// Frames to follow in each direction
    #[serde(default = "default_count")]
    pub count: u64,
    #[serde(default)]
    pub direction: Direction,
    /// The boxes to follow, as on the frame
    pub boxes: Vec<ObjectBox>,
    /// `auto`, `cuda` or `cpu`
    #[serde(default = "default_device")]
    pub device: String,
    /// Score below which an object counts as lost
    #[serde(default = "default_min_score")]
    pub min_score: f64,
}

impl FollowRequest {
    fn check(&self) -> Result<()> {
        ensure!(!self.boxes.is_empty(), "no boxes to follow");
        ensure!(
            (1..=MAX_COUNT).contains(&self.count),
            "count must be 1 to {MAX_COUNT}"
        );
        ensure!(
            ["auto", "cuda", "cpu"].contains(&self.device.as_str()),
            "device must be auto, cuda or cpu"
        );
        ensure!(
            (0.0..1.0).contains(&self.min_score),
            "min_score must be in 0..1"
        );
        Ok(())
    }
}

/// State of a Follow
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Waiting for the tracker (which may be loading its model)
    Starting,
    Running,
    Done,
    Failed,
    /// Stopped by the user; what was written stays
    Cancelled,
}

/// An object the tracker lost
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lost {
    pub id: u64,
    pub class: String,
    /// The first frame it was not found on
    pub frame: u64,
    /// `low score`, `gone` or `jumped`
    pub reason: String,
}

/// A Follow, as the page sees it
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Job {
    pub id: u64,
    pub s: String,
    pub seg: String,
    pub frame: u64,
    pub count: u64,
    pub direction: Direction,
    pub state: JobState,
    /// The pass under way: `forward` or `backward`
    pub pass: Option<String>,
    /// Frames to follow over all passes, and frames done
    pub planned: u64,
    pub done: u64,
    /// Objects asked for, and those still followed in this pass
    pub objects: usize,
    pub following: usize,
    /// `cuda` or `cpu`, and why the tracker chose it
    pub device: Option<String>,
    pub device_reason: Option<String>,
    /// Time the tracker took to load its model for this Follow, if it did
    pub load_ms: Option<f64>,
    /// Mean time per frame so far
    pub ms_per_frame: Option<f64>,
    /// Frames written, and followed boxes left out because a person's box
    /// already stands for the object there
    pub written: usize,
    pub covered: usize,
    pub lost: Vec<Lost>,
    /// Why passes stopped short, such as "the segment ends"
    pub notes: Vec<String>,
    pub error: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

impl Job {
    fn finished(&self) -> bool {
        matches!(
            self.state,
            JobState::Done | JobState::Failed | JobState::Cancelled
        )
    }
}

/// A box from the tracker
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TrackedBox {
    pub id: u64,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub score: f64,
}

/// An object the tracker lost on a frame
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TrackedLost {
    pub id: u64,
    pub reason: String,
}

/// A line of the tracker's answer
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Start {
        device: String,
        reason: String,
        load_ms: Option<f64>,
        frames: u64,
    },
    Frame {
        frame: u64,
        ms: f64,
        boxes: Vec<TrackedBox>,
        lost: Vec<TrackedLost>,
    },
    End {
        frames: u64,
        ms_per_frame: Option<f64>,
        reason: String,
    },
    Error {
        error: String,
    },
}

/// A tracked box as a model box of the labels, with the class of its track
pub fn model_box(tracked: &TrackedBox, class: &str) -> ObjectBox {
    ObjectBox {
        class: class.to_string(),
        x: tracked.x,
        y: tracked.y,
        w: tracked.w,
        h: tracked.h,
        id: Some(Value::from(tracked.id)),
        by: "model".to_string(),
        score: Some(tracked.score),
        extra: Map::new(),
    }
}

/// Runs Follows and the tracker the studio started
pub struct Follow {
    inspector: Arc<Inspector>,
    settings: Settings,
    job: Mutex<Option<Job>>,
    cancel: AtomicBool,
    next_id: Mutex<u64>,
    /// The tracker, when started from the page
    service: Mutex<Option<Child>>,
}

/// Unix time in ms
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A client for the tracker; `timeout` bounds the wait for an answer's
/// head, not the answer, which streams while frames are done
pub(crate) fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_recv_response(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

impl Follow {
    pub fn new(inspector: Arc<Inspector>, settings: Settings) -> Self {
        Self {
            inspector,
            settings,
            job: Mutex::default(),
            cancel: AtomicBool::new(false),
            next_id: Mutex::new(1),
            service: Mutex::default(),
        }
    }

    /// The current or last Follow
    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap().clone()
    }

    fn update(&self, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.job.lock().unwrap().as_mut() {
            f(job);
        }
    }

    /// Whether the tracker answers, how to start it, and the Follow
    pub fn status(&self) -> Value {
        let health = agent(Duration::from_millis(1500))
            .get(format!("{}/health", self.settings.url))
            .call()
            .context("no answer")
            .and_then(|mut response| {
                let text = response.body_mut().read_to_string()?;
                ensure!(response.status() == 200, "answered {}", response.status());
                Ok(serde_json::from_str::<Value>(&text)?)
            });
        let started = self.service_state();
        json!({
            "url": self.settings.url,
            "ok": health.is_ok(),
            "health": health.as_ref().ok(),
            "error": health.as_ref().err().map(|e| format!("{e:#}")),
            "command": self.settings.command_line(),
            "can_start": !self.settings.command.is_empty() && self.settings.dir.is_dir(),
            "started": started,
            "job": self.job(),
        })
    }

    /// What became of the tracker the studio started, if it did
    fn service_state(&self) -> Option<String> {
        let mut service = self.service.lock().unwrap();
        let child = service.as_mut()?;
        Some(match child.try_wait() {
            Ok(None) => "running".to_string(),
            Ok(Some(status)) => format!("exited ({status})"),
            Err(e) => format!("unknown ({e})"),
        })
    }

    /// Start the tracker with the configured command, unless the one
    /// started before still runs
    pub fn start_service(&self) -> Result<Value> {
        let mut service = self.service.lock().unwrap();
        if let Some(child) = service.as_mut()
            && child.try_wait()?.is_none()
        {
            return Ok(json!({ "started": "running" }));
        }
        let (program, args) = self
            .settings
            .command
            .split_first()
            .context("no [inspect] tracker_command")?;
        ensure!(
            self.settings.dir.is_dir(),
            "no folder {}",
            self.settings.dir.display()
        );
        // A group of its own: `uv run` starts Python as its child, and both
        // are stopped together (see `stop_service`)
        let child = Command::new(program)
            .args(args)
            .current_dir(&self.settings.dir)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot run {}", self.settings.command_line()))?;
        log::info!(
            "Started the tracker (pid {}): {}",
            child.id(),
            self.settings.command_line()
        );
        *service = Some(child);
        Ok(json!({ "started": "running" }))
    }

    /// Stop the tracker the studio started, if it did: its whole process
    /// group, so the Python behind `uv run` goes too
    pub fn stop_service(&self) {
        if let Some(mut child) = self.service.lock().unwrap().take() {
            if let Ok(None) = child.try_wait() {
                log::info!("Stopping the tracker (pid {})", child.id());
                // SAFETY: kill(2) with a negative pid signals the group the
                // child leads; it touches no memory
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
            }
            let _ = child.wait();
        }
    }

    /// Stop the current Follow after its next frame
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Start a Follow on a thread of its own
    pub fn run(self: &Arc<Self>, request: FollowRequest) -> Result<Job> {
        request.check()?;
        let mut current = self.job.lock().unwrap();
        if let Some(job) = &*current
            && !job.finished()
        {
            bail!("a Follow is under way; cancel it first");
        }
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next - 1
        };
        let job = Job {
            id,
            s: request.s.clone(),
            seg: request.seg.clone(),
            frame: request.frame,
            count: request.count,
            direction: request.direction,
            state: JobState::Starting,
            pass: None,
            planned: 0,
            done: 0,
            objects: request.boxes.len(),
            following: request.boxes.len(),
            device: None,
            device_reason: None,
            load_ms: None,
            ms_per_frame: None,
            written: 0,
            covered: 0,
            lost: Vec::new(),
            notes: Vec::new(),
            error: None,
            started_ms: now_ms(),
            finished_ms: None,
        };
        *current = Some(job.clone());
        drop(current);
        self.cancel.store(false, Ordering::Relaxed);
        let follow = Arc::clone(self);
        std::thread::Builder::new()
            .name("follow".to_string())
            .spawn(move || {
                let result = follow.work(&request);
                follow.update(|job| {
                    job.finished_ms = Some(now_ms());
                    job.pass = None;
                    match result {
                        Ok(()) if follow.cancel.load(Ordering::Relaxed) => {
                            job.state = JobState::Cancelled
                        }
                        Ok(()) => job.state = JobState::Done,
                        Err(e) => {
                            log::warn!("Follow failed: {:#}", e);
                            job.state = JobState::Failed;
                            job.error = Some(format!("{e:#}"));
                        }
                    }
                });
            })?;
        Ok(job)
    }

    fn work(&self, request: &FollowRequest) -> Result<()> {
        let seg = (!request.seg.is_empty()).then_some(request.seg.as_str());
        let (file, video, total) = self.inspector.video(&request.s, seg)?;
        let total = total as u64;
        ensure!(request.frame < total, "no frame {}", request.frame);
        let annotations = self.inspector.annotations();
        let ids = annotations.follow_ids(&request.s, &file, request.frame, &request.boxes)?;
        let classes: BTreeMap<u64, String> = ids
            .iter()
            .zip(&request.boxes)
            .map(|(&id, b)| (id, b.class.clone()))
            .collect();
        let spans: Vec<(bool, u64)> = request
            .direction
            .passes()
            .iter()
            .map(|&forward| {
                let span = follow_span(request.frame, request.count, forward, total);
                (forward, span)
            })
            .collect();
        let notes = spans
            .iter()
            .filter(|&&(_, span)| span < request.count)
            .map(|&(forward, _)| {
                if forward {
                    "the segment ends"
                } else {
                    "the segment starts"
                }
                .to_string()
            })
            .collect();
        self.update(|job| {
            job.seg = file.clone();
            job.planned = spans.iter().map(|(_, span)| span).sum();
            job.notes = notes;
        });
        for &(forward, span) in &spans {
            if span == 0 || self.cancel.load(Ordering::Relaxed) {
                continue;
            }
            self.update(|job| {
                job.pass = Some(if forward { "forward" } else { "backward" }.to_string());
                job.following = ids.len();
            });
            self.pass(request, &file, &video, forward, span, &ids, &classes)?;
        }
        Ok(())
    }

    /// Follow one way over `span` frames, writing as frames come
    #[allow(clippy::too_many_arguments)]
    fn pass(
        &self,
        request: &FollowRequest,
        file: &str,
        video: &Path,
        forward: bool,
        span: u64,
        ids: &[u64],
        classes: &BTreeMap<u64, String>,
    ) -> Result<()> {
        let prompts: Vec<Value> = ids
            .iter()
            .zip(&request.boxes)
            .map(|(id, b)| json!({ "id": id, "x": b.x, "y": b.y, "w": b.w, "h": b.h }))
            .collect();
        let body = json!({
            "video": video,
            "start": request.frame,
            "count": span,
            "direction": if forward { "forward" } else { "backward" },
            "boxes": prompts,
            "device": request.device,
            "min_score": request.min_score,
        });
        // The first request may download and load the model
        let response = agent(Duration::from_secs(600))
            .post(format!("{}/track", self.settings.url))
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| {
                anyhow::anyhow!(
                    "tracker not running at {} ({e}); start it with `{}`",
                    self.settings.url,
                    self.settings.command_line()
                )
            })?;
        let status = response.status();
        let mut reader = std::io::BufReader::new(response.into_body().into_reader());
        if status != 200 {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut reader, &mut text)?;
            let error = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or(text);
            bail!("the tracker answered {status}: {error}");
        }
        let annotations = self.inspector.annotations();
        let mut pending: Vec<(u64, Vec<ObjectBox>)> = Vec::new();
        let mut frame_ms: Vec<f64> = Vec::new();
        let write = |pending: &mut Vec<(u64, Vec<ObjectBox>)>| -> Result<()> {
            if pending.is_empty() {
                return Ok(());
            }
            let FollowWrite { written, covered } =
                annotations.write_followed(&request.s, file, pending, ids)?;
            pending.clear();
            self.update(|job| {
                job.written += written;
                job.covered += covered;
            });
            Ok(())
        };
        let mut line = String::new();
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                // Dropping the answer closes the connection, which stops the tracker
                break;
            }
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let message: Message = serde_json::from_str(&line)
                .with_context(|| format!("bad line from the tracker: {}", line.trim()))?;
            match message {
                Message::Start {
                    device,
                    reason,
                    load_ms,
                    ..
                } => self.update(|job| {
                    job.state = JobState::Running;
                    job.device = Some(device);
                    job.device_reason = Some(reason);
                    job.load_ms = job.load_ms.or(load_ms);
                }),
                Message::Frame {
                    frame,
                    ms,
                    boxes,
                    lost,
                } => {
                    frame_ms.push(ms);
                    let class = |id: u64| classes.get(&id).map_or("object", String::as_str);
                    let boxes: Vec<ObjectBox> =
                        boxes.iter().map(|b| model_box(b, class(b.id))).collect();
                    let following = boxes.len();
                    pending.push((frame, boxes));
                    let mean = frame_ms.iter().sum::<f64>() / frame_ms.len() as f64;
                    self.update(|job| {
                        job.done += 1;
                        job.following = following;
                        job.ms_per_frame = Some((mean * 10.0).round() / 10.0);
                        job.lost.extend(lost.into_iter().map(|l| Lost {
                            id: l.id,
                            class: class(l.id).to_string(),
                            frame,
                            reason: l.reason,
                        }));
                    });
                    if pending.len() >= WRITE_EVERY {
                        write(&mut pending)?;
                    }
                }
                Message::End { frames, reason, .. } => {
                    if frames < span && reason != "done" {
                        let way = if forward { "forward" } else { "backward" };
                        self.update(|job| job.notes.push(format!("{way}: {reason}")));
                    }
                }
                Message::Error { error } => {
                    write(&mut pending)?;
                    bail!("the tracker failed: {error}");
                }
            }
        }
        write(&mut pending)
    }
}

// ------------------------------------------------------------------ HTTP

/// An error with the status it answers with
struct Status(StatusCode, anyhow::Error);

impl From<anyhow::Error> for Status {
    fn from(e: anyhow::Error) -> Self {
        Status(StatusCode::BAD_REQUEST, e)
    }
}

impl Follow {
    fn post(self: &Arc<Self>, path: &str, body: &[u8]) -> Result<Value, Status> {
        match path {
            "run" => {
                let request: FollowRequest = serde_json::from_slice(body)
                    .map_err(|e| anyhow::anyhow!("bad Follow request: {e}"))?;
                if self.job().is_some_and(|job| !job.finished()) {
                    return Err(Status(
                        StatusCode::CONFLICT,
                        anyhow::anyhow!("a Follow is under way; cancel it first"),
                    ));
                }
                Ok(json!(self.run(request)?))
            }
            "cancel" => {
                self.cancel();
                Ok(json!(self.job()))
            }
            "start" => self
                .start_service()
                .map_err(|e| Status(StatusCode::SERVICE_UNAVAILABLE, e)),
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST {path}"),
            )),
        }
    }
}

/// The routes under `/api/inspect/follow/`; calls to the tracker and the
/// annotations are made off the async workers
pub fn routes(follow: Arc<Follow>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || warp::path!("api" / "inspect" / "follow" / String);
    let reader = Arc::clone(&follow);
    let get = warp::get().and(base()).and_then(move |path: String| {
        let follow = Arc::clone(&reader);
        blocking(move || match path.as_str() {
            "status" => Ok(follow.status()),
            "job" => Ok(json!(follow.job())),
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        })
    });
    let post = warp::post()
        .and(base())
        .and(warp::body::content_length_limit(BODY_LIMIT))
        .and(warp::body::bytes())
        .and_then(move |path: String, body: warp::hyper::body::Bytes| {
            let follow = Arc::clone(&follow);
            blocking(move || follow.post(&path, &body))
        });
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

    #[test]
    fn settings_from_config() {
        let dir = Path::new("/studio");
        let settings = Settings::from_config(&InspectConfig::default(), dir).unwrap();
        assert_eq!(settings.url, DEFAULT_TRACKER);
        assert_eq!(
            settings.command,
            ["uv", "run", "agentzero-track-serve", "--port", "7340"]
        );
        assert_eq!(settings.dir, Path::new("/studio/../AgentZero"));
        assert_eq!(
            settings.command_line(),
            "cd /studio/../AgentZero && uv run agentzero-track-serve --port 7340"
        );

        let config = InspectConfig {
            tracker: Some("http://gpu-box:7400/".to_string()),
            tracker_command: Some(vec!["./serve".to_string()]),
            tracker_dir: Some("/opt/tracker".to_string()),
            ..InspectConfig::default()
        };
        let settings = Settings::from_config(&config, dir).unwrap();
        assert_eq!(settings.url, "http://gpu-box:7400");
        assert_eq!(settings.command, ["./serve"]);
        assert_eq!(settings.dir, Path::new("/opt/tracker"));

        let https = InspectConfig {
            tracker: Some("https://x".to_string()),
            ..InspectConfig::default()
        };
        assert!(Settings::from_config(&https, dir).is_err());
    }

    #[test]
    fn requests() {
        let body = r#"{"s": "s", "frame": 3, "boxes": [{"class": "chum", "x": 0.1, "y": 0.2, "w": 0.1, "h": 0.1}]}"#;
        let request: FollowRequest = serde_json::from_str(body).unwrap();
        assert_eq!(
            (request.count, request.direction, request.device.as_str()),
            (60, Direction::Forward, "auto")
        );
        request.check().unwrap();
        let both: FollowRequest = serde_json::from_str(&body.replace(
            "\"frame\": 3",
            "\"frame\": 3, \"direction\": \"both\", \"count\": 0",
        ))
        .unwrap();
        assert_eq!(both.direction.passes(), [true, false]);
        assert!(both.check().is_err());
        let empty: FollowRequest =
            serde_json::from_str(r#"{"s": "s", "frame": 3, "boxes": []}"#).unwrap();
        assert!(empty.check().is_err());
    }

    #[test]
    fn tracker_lines() {
        let start = r#"{"type": "start", "device": "cuda", "reason": "9.0 GiB free", "dtype": "bfloat16", "model": "m", "load_ms": null, "frames": 60}"#;
        assert_eq!(
            serde_json::from_str::<Message>(start).unwrap(),
            Message::Start {
                device: "cuda".to_string(),
                reason: "9.0 GiB free".to_string(),
                load_ms: None,
                frames: 60
            }
        );
        let frame = r#"{"type": "frame", "frame": 11, "ms": 35.2, "boxes": [{"id": 4, "x": 0.1, "y": 0.2, "w": 0.3, "h": 0.4, "score": 0.97}], "lost": [{"id": 5, "reason": "low score", "score": 0.2}]}"#;
        let Message::Frame { boxes, lost, .. } = serde_json::from_str(frame).unwrap() else {
            panic!("not a frame");
        };
        assert_eq!(lost[0].id, 5);
        let b = model_box(&boxes[0], "cohock");
        assert_eq!((b.class.as_str(), b.by.as_str()), ("cohock", "model"));
        assert_eq!((b.track_id(), b.score), (Some(4), Some(0.97)));
        let error = r#"{"type": "error", "error": "CUDA out of memory"}"#;
        assert!(matches!(
            serde_json::from_str::<Message>(error).unwrap(),
            Message::Error { .. }
        ));
    }

    #[test]
    fn ports() {
        assert_eq!(port_of("http://127.0.0.1:7340"), Some(7340));
        assert_eq!(port_of("http://host:80/x"), Some(80));
        assert_eq!(port_of("http://host"), None);
    }
}
