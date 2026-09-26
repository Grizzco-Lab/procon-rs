//! Vision app backend: object detection and tracking on recorded sessions
//!
//! A run detects objects in a range of a segment's frames (the first, then
//! every `step`-th, `count` of them) with the `gameplay-vision` crate's
//! YOLOv8, optionally tracks them, and keeps each frame's timings. It runs
//! on a thread of its own, one run at a time; the page polls its progress.
//! The network loads once and is reused until another model or device is
//! asked for.
//!
//! The last results of a segment are kept in the `[vision] results` folder:
//!
//! - `<results>/<session>/<segment stem>.objects.jsonl`: one line per
//!   processed frame, in the label format shared with the labeling mode
//!   (see [`crate::objects`]), plus the frame's timings in ms:
//!   `{"frame": 120, "boxes": [...], "ms": {"decode": 0.4, "network": 141.2, "total": 143.0}}`.
//!   `gameplay-vision render` and `prelabel --input` read it as it is.
//! - `<results>/<session>/<segment stem>.run.json`: the run ([`Job`]): model,
//!   device, range, state and timing summary.
//!
//! "Send to labels" merges a segment's results into the annotations folder
//! as model boxes, by the crate's prelabel rule: frames a person has
//! labeled are never touched.
//!
//! Endpoints under `/api/vision/`:
//!
//! - `GET info`: models on offer, whether this build has CUDA, folders
//! - `GET job`: the current or last run (`null` before the first)
//! - `GET results?s=&seg=`: a segment's last results: `{"run", "frames",
//!   "classes", "tracks"}`
//! - `POST run` with a [`RunRequest`] starts a run (409 while one runs)
//! - `POST cancel` stops the current run after its frame
//! - `POST send` with `{"s", "seg", "map"}` writes the results into the
//!   labels; `map` renames classes, such as `person=player`
//!
//! Errors are `{"error": "..."}` with status 400.

use crate::inspect::Inspector;
use crate::objects::write_atomic;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use gameplay_data::session::SessionInfo;
use gameplay_vision::detect::{self, Detector, Weights};
use gameplay_vision::frames::{FrameRange, FrameReader, Segment};
use gameplay_vision::labels::{self, FrameObjects, ObjectBox};
use gameplay_vision::track::{Tracker, TrackerConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// COCO model sizes on offer; larger ones take seconds per frame on a CPU
pub const SIZES: [char; 3] = ['n', 's', 'm'];

/// Most frames in one run
pub const MAX_COUNT: u64 = 20_000;

/// Largest request body
const BODY_LIMIT: u64 = 64 << 10;

/// Our own network, offered as "custom"
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CustomModel {
    /// The safetensors file
    pub weights: PathBuf,
    /// Its size: `n`, `s`, `m`, `l` or `x`
    pub size: char,
    /// Its class names
    pub classes: PathBuf,
}

/// The Vision app's settings, from `[vision]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Folder of the last results per segment
    pub results: PathBuf,
    /// COCO size chosen at first
    pub size: char,
    /// Our own network, if configured
    pub custom: Option<CustomModel>,
    /// Lowest score kept
    pub confidence: f32,
}

impl Settings {
    /// Settings from `[vision]`; relative paths start at `config_dir`, and
    /// the results folder defaults to `default_results`
    pub fn from_config(
        config: crate::config::VisionConfig,
        config_dir: &Path,
        default_results: PathBuf,
    ) -> Result<Self> {
        let size = |text: Option<&str>, allowed: &[char]| -> Result<char> {
            let text = text.unwrap_or("n");
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if allowed.contains(&c) => Ok(c),
                _ => bail!("[vision] size {text:?} is not one of {allowed:?}"),
            }
        };
        let custom = match (config.weights, config.classes) {
            (Some(weights), Some(classes)) => Some(CustomModel {
                weights: config_dir.join(weights),
                size: size(config.weights_size.as_deref(), &['n', 's', 'm', 'l', 'x'])?,
                classes: config_dir.join(classes),
            }),
            (None, None) => None,
            _ => bail!("[vision] weights and classes go together"),
        };
        let confidence = config.confidence.unwrap_or(0.25);
        ensure!(
            (0.0..1.0).contains(&confidence),
            "[vision] confidence must be in 0..1"
        );
        Ok(Self {
            results: config
                .results
                .map_or(default_results, |dir| config_dir.join(dir)),
            size: size(config.size.as_deref(), &SIZES)?,
            custom,
            confidence,
        })
    }
}

fn default_step() -> u64 {
    2
}

fn default_count() -> u64 {
    300
}

fn yes() -> bool {
    true
}

/// What to run: a segment's frames and a model
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct RunRequest {
    /// Session folder name under the Inkspector's root
    pub s: String,
    /// Segment video file
    pub seg: String,
    /// First frame
    #[serde(default)]
    pub start: u64,
    /// Frames between two processed frames
    #[serde(default = "default_step")]
    pub step: u64,
    /// Frames to process
    #[serde(default = "default_count")]
    pub count: u64,
    /// `n`, `s`, `m` or `custom`; by default the configured size
    #[serde(default)]
    pub model: Option<String>,
    /// Run on the CPU even when a GPU is there
    #[serde(default)]
    pub cpu: bool,
    /// Track the boxes, giving them ids
    #[serde(default = "yes")]
    pub track: bool,
}

/// A model on a device: runs with the same key reuse the loaded network
#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelKey {
    /// `n`, `s`, `m` or `custom`
    model: String,
    cpu: bool,
}

impl RunRequest {
    /// Check the request; answers with the model to use
    fn check(&self, settings: &Settings) -> Result<ModelKey> {
        check_name(&self.s)?;
        check_name(&self.seg)?;
        ensure!(self.step >= 1, "step must be at least 1");
        ensure!(
            (1..=MAX_COUNT).contains(&self.count),
            "count must be 1 to {MAX_COUNT}"
        );
        let model = self
            .model
            .clone()
            .unwrap_or_else(|| settings.size.to_string());
        match model.as_str() {
            "custom" => ensure!(
                settings.custom.is_some(),
                "no custom model: set [vision] weights and classes"
            ),
            m if SIZES.iter().any(|s| s.to_string() == m) => {}
            m => bail!("unknown model {m:?}"),
        }
        Ok(ModelKey {
            model,
            cpu: self.cpu,
        })
    }
}

/// A session or segment file name: no path in it
fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
        "bad name {name:?}"
    );
    Ok(())
}

/// State of a run
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    /// Loading (or downloading) the network
    Loading,
    /// Detecting frames
    Running,
    Done,
    Failed,
    /// Stopped by the user; the frames done so far are kept
    Cancelled,
}

/// Mean and 95th percentile of a stage, in ms
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    pub mean: f64,
    pub p95: f64,
}

/// Mean and 95th percentile of `values`
pub fn stat(values: &[f64]) -> Stat {
    if values.is_empty() {
        return Stat::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) as f64 * 0.95).round() as usize;
    Stat {
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        p95: sorted[index],
    }
}

/// One frame's timings, in ms
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FrameTiming {
    /// Waiting for ffmpeg's next frame
    pub decode: f64,
    /// The network, until the device is done
    pub network: f64,
    /// Decode, pre- and postprocessing and the network
    pub total: f64,
}

/// Timings of a run so far
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    pub decode: Stat,
    pub network: Stat,
    pub total: Stat,
    /// Frames per second after the first frame (which includes one-time
    /// setup), from the mean total
    pub frames_per_s: Option<f64>,
}

impl Timings {
    /// The summary of per-frame timings
    pub fn of(frames: &[FrameTiming]) -> Self {
        let column = |f: fn(&FrameTiming) -> f64| frames.iter().map(f).collect::<Vec<_>>();
        let steady = stat(&frames.iter().skip(1).map(|t| t.total).collect::<Vec<_>>());
        Self {
            decode: stat(&column(|t| t.decode)),
            network: stat(&column(|t| t.network)),
            total: stat(&column(|t| t.total)),
            frames_per_s: (frames.len() > 1 && steady.mean > 0.0).then(|| 1000.0 / steady.mean),
        }
    }
}

/// A run, as the page and `run.json` see it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    /// Session and segment file
    pub s: String,
    pub seg: String,
    pub start: u64,
    pub step: u64,
    pub count: u64,
    /// `n`, `s`, `m` or `custom`
    pub model: String,
    /// What the model is called, such as `YOLOv8n · COCO`
    pub model_name: String,
    /// `cpu` or `cuda`, once loaded
    pub device: Option<String>,
    pub track: bool,
    pub state: JobState,
    /// Frames processed
    pub done: u64,
    /// Boxes found
    pub boxes: usize,
    /// Time to load the network; `None` when it was loaded already
    pub load_ms: Option<f64>,
    pub timings: Timings,
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

/// The loaded network
type Loaded = (ModelKey, Arc<Detector>);

/// Runs detection jobs and keeps their results
pub struct Vision {
    inspector: Arc<Inspector>,
    settings: Settings,
    /// The current or last run
    job: Mutex<Option<Job>>,
    cancel: AtomicBool,
    model: Mutex<Option<Loaded>>,
    next_id: Mutex<u64>,
}

/// Unix time in ms
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1e4).round() / 10.0
}

impl Vision {
    pub fn new(inspector: Arc<Inspector>, settings: Settings) -> Self {
        Self {
            inspector,
            settings,
            job: Mutex::default(),
            cancel: AtomicBool::new(false),
            model: Mutex::default(),
            next_id: Mutex::new(1),
        }
    }

    /// Models on offer and where things are
    pub fn info(&self) -> Value {
        let loaded = self
            .model
            .lock()
            .unwrap()
            .as_ref()
            .map(|(key, _)| key.clone());
        json!({
            "sizes": SIZES.map(String::from),
            "size": self.settings.size.to_string(),
            "custom": self.settings.custom,
            "confidence": self.settings.confidence,
            "cuda": cfg!(feature = "cuda"),
            "loaded": loaded.map(|key| json!({ "model": key.model, "cpu": key.cpu })),
            "results": self.settings.results,
            "annotations": self.inspector.annotations().dir(),
        })
    }

    /// The current or last run
    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap().clone()
    }

    /// Start a run on a thread of its own
    pub fn run(self: &Arc<Self>, request: RunRequest) -> Result<Job> {
        let key = request.check(&self.settings)?;
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
            s: request.s.clone(),
            seg: request.seg.clone(),
            start: request.start,
            step: request.step,
            count: request.count,
            model: key.model.clone(),
            model_name: self.model_name(&key.model),
            device: None,
            track: request.track,
            state: JobState::Loading,
            done: 0,
            boxes: 0,
            load_ms: None,
            timings: Timings::default(),
            error: None,
            started_ms: now_ms(),
            finished_ms: None,
        };
        *current = Some(job.clone());
        drop(current);
        self.cancel.store(false, Ordering::Relaxed);
        let vision = Arc::clone(self);
        std::thread::Builder::new()
            .name("vision".to_string())
            .spawn(move || {
                let result = vision.work(&request, &key);
                vision.update(|job| {
                    job.finished_ms = Some(now_ms());
                    match result {
                        Ok(()) if job.state == JobState::Cancelled => {}
                        Ok(()) => job.state = JobState::Done,
                        Err(e) => {
                            log::warn!("Vision run failed: {:#}", e);
                            job.state = JobState::Failed;
                            job.error = Some(format!("{e:#}"));
                        }
                    }
                });
                if let Some(job) = vision.job()
                    && let Err(e) = vision.save_run(&job)
                {
                    log::warn!("Cannot save the vision run: {:#}", e);
                }
            })?;
        Ok(job)
    }

    /// Stop the current run after its frame
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn update(&self, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.job.lock().unwrap().as_mut() {
            f(job);
        }
    }

    fn model_name(&self, model: &str) -> String {
        match (model, &self.settings.custom) {
            ("custom", Some(custom)) => custom
                .weights
                .file_name()
                .map_or("custom".to_string(), |n| n.to_string_lossy().into_owned()),
            (size, _) => format!("YOLOv8{size} · COCO"),
        }
    }

    /// The network for `key`, loaded once and kept until another is asked
    /// for; the time it took if it was loaded now
    fn detector(&self, key: &ModelKey) -> Result<(Arc<Detector>, Option<f64>)> {
        let mut model = self.model.lock().unwrap();
        if let Some((loaded, detector)) = &*model
            && loaded == key
        {
            return Ok((Arc::clone(detector), None));
        }
        // Free the old network before loading the next
        *model = None;
        let start = Instant::now();
        let weights = match (key.model.as_str(), &self.settings.custom) {
            ("custom", Some(custom)) => Weights::File {
                path: custom.weights.clone(),
                size: custom.size,
                classes: detect::read_class_names(&custom.classes)?,
            },
            (size, _) => Weights::Coco(size.chars().next().context("no model size")?),
        };
        let mut detector = Detector::load(&weights, detect::device(key.cpu)?)?;
        detector.confidence = self.settings.confidence;
        let detector = Arc::new(detector);
        *model = Some((key.clone(), Arc::clone(&detector)));
        Ok((detector, Some(ms(start.elapsed()))))
    }

    /// The segment a request names
    fn segment(&self, session: &str, file: &str) -> Result<Segment> {
        let dir = self.inspector.root().join(session);
        let info = SessionInfo::read(&dir)?;
        let index = info
            .video
            .segments
            .iter()
            .position(|s| s.file == file)
            .with_context(|| format!("{session} has no segment {file}"))?;
        Segment::open(&dir, index)
    }

    /// Load the model, then detect (and track) frame by frame
    fn work(&self, request: &RunRequest, key: &ModelKey) -> Result<()> {
        let segment = self.segment(&request.s, &request.seg)?;
        let (detector, load_ms) = self.detector(key)?;
        let device = if detector.device().is_cpu() {
            "cpu"
        } else {
            "cuda"
        };
        self.update(|job| {
            job.state = JobState::Running;
            job.device = Some(device.to_string());
            job.load_ms = load_ms;
        });
        let range = FrameRange {
            first: request.start,
            step: request.step,
            count: Some(request.count),
        };
        let mut reader = FrameReader::start(&segment, range)?;
        let mut tracker = request
            .track
            .then(|| Tracker::new(TrackerConfig::default()));
        let mut frames = Vec::new();
        let mut timings = Vec::new();
        let result = loop {
            if self.cancel.load(Ordering::Relaxed) {
                self.update(|job| job.state = JobState::Cancelled);
                break Ok(());
            }
            let wait = Instant::now();
            let frame = match reader.next() {
                None => break Ok(()),
                Some(Err(e)) => break Err(e),
                Some(Ok(frame)) => frame,
            };
            let decode = wait.elapsed();
            let (mut boxes, timing) = match detector.detect(&frame.rgb) {
                Ok(found) => found,
                Err(e) => break Err(e),
            };
            if let Some(tracker) = &mut tracker {
                let tracked = tracker.update(frame.number, &boxes);
                assign_ids(&mut boxes, &tracked);
            }
            let time = FrameTiming {
                decode: ms(decode),
                network: ms(timing.forward),
                total: ms(decode + timing.total()),
            };
            timings.push(time);
            let found = boxes.len();
            let mut line = FrameObjects::new(frame.number, boxes);
            line.extra.insert("ms".to_string(), json!(time));
            frames.push(line);
            let summary = Timings::of(&timings);
            self.update(|job| {
                job.done += 1;
                job.boxes += found;
                job.timings = summary;
            });
        };
        if result.is_ok() && frames.is_empty() && !self.cancel.load(Ordering::Relaxed) {
            bail!(
                "no frames from frame {}: the segment is shorter",
                request.start
            );
        }
        // What was done is kept, even when stopped or failed on the way
        if !frames.is_empty() {
            let path = self.results_path(&segment.session, segment.stem(), labels::OBJECTS_EXT);
            labels::write_objects(&path, &frames)?;
        }
        result
    }

    /// `<results>/<session>/<stem><suffix>`
    fn results_path(&self, session: &str, stem: &str, suffix: &str) -> PathBuf {
        self.settings
            .results
            .join(session)
            .join(format!("{stem}{suffix}"))
    }

    fn stem(file: &str) -> &str {
        Path::new(file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(file)
    }

    /// Write a finished run's `run.json` next to its frames
    fn save_run(&self, job: &Job) -> Result<()> {
        if job.done == 0 {
            return Ok(());
        }
        let path = self.results_path(&job.s, Self::stem(&job.seg), ".run.json");
        write_atomic(&path, &serde_json::to_vec_pretty(job)?)
    }

    /// A segment's last results with their per-class and per-track
    /// summaries; no frames if it has none
    pub fn results(&self, session: &str, segment: &str) -> Result<Value> {
        check_name(session)?;
        check_name(segment)?;
        let stem = Self::stem(segment);
        let frames = labels::read_objects(&self.results_path(session, stem, labels::OBJECTS_EXT))?;
        let run: Option<Job> = std::fs::read(self.results_path(session, stem, ".run.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let (classes, tracks) = summarize(&frames);
        Ok(json!({
            "run": run,
            "frames": frames,
            "classes": classes,
            "tracks": tracks,
        }))
    }

    /// Write a segment's results into the labels as model boxes, renamed by
    /// `map` (`from=to` pairs); boxes of classes the labels do not have are
    /// left out and counted
    pub fn send(&self, session: &str, segment: &str, map: &str) -> Result<Value> {
        check_name(session)?;
        check_name(segment)?;
        let maps = labels::parse_class_map(map)?;
        let path = self.results_path(session, Self::stem(segment), labels::OBJECTS_EXT);
        let mut frames = labels::read_objects(&path)?;
        ensure!(!frames.is_empty(), "no results for {session}/{segment} yet");
        let annotations = self.inspector.annotations();
        let classes = annotations.classes()?;
        let dropped =
            labels::map_classes(&mut frames, &maps, |c| classes.iter().any(|k| k.name == c));
        for frame in &mut frames {
            // The timings stay with the results
            frame.extra.clear();
        }
        let boxes: usize = frames.iter().map(|f| f.boxes.len()).sum();
        let first = frames.iter().find(|f| !f.boxes.is_empty()).map(|f| f.frame);
        let (file, stats) = annotations.merge_model_boxes(session, segment, frames)?;
        log::info!(
            "Sent {session}/{segment} to the labels: {} added, {} replaced, {} kept",
            stats.added,
            stats.replaced,
            stats.kept
        );
        Ok(json!({
            "file": file,
            "boxes": boxes,
            "added": stats.added,
            "replaced": stats.replaced,
            "kept": stats.kept,
            "dropped": dropped,
            "first": first,
        }))
    }
}

/// Give detections the ids of the tracks that matched them: each tracked
/// box takes the free detection of its class it overlaps most
pub fn assign_ids(detections: &mut [ObjectBox], tracked: &[ObjectBox]) {
    for track in tracked {
        let best = detections
            .iter_mut()
            .filter(|d| d.id.is_none() && d.class == track.class)
            .map(|d| (labels::iou(d.rect(), track.rect()), d))
            .filter(|(overlap, _)| *overlap > 0.0)
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, detection)) = best {
            detection.id = track.id;
        }
    }
}

/// Per class: boxes, frames and mean score; per track: class, frames, first
/// and last frame
pub fn summarize(frames: &[FrameObjects]) -> (Vec<Value>, Vec<Value>) {
    // class -> (boxes, frames, score sum)
    let mut classes: BTreeMap<&str, (usize, usize, f64)> = BTreeMap::new();
    // id -> (class, frames, first, last)
    let mut tracks: BTreeMap<u64, (&str, usize, u64, u64)> = BTreeMap::new();
    for frame in frames {
        let mut seen: Vec<&str> = Vec::new();
        for b in &frame.boxes {
            let entry = classes.entry(&b.class).or_default();
            entry.0 += 1;
            entry.2 += b.score.unwrap_or(0.0);
            if !seen.contains(&b.class.as_str()) {
                seen.push(&b.class);
                entry.1 += 1;
            }
            if let Some(id) = b.id {
                let track = tracks
                    .entry(id)
                    .or_insert((&b.class, 0, frame.frame, frame.frame));
                track.1 += 1;
                track.2 = track.2.min(frame.frame);
                track.3 = track.3.max(frame.frame);
            }
        }
    }
    let mut classes: Vec<Value> = classes
        .into_iter()
        .map(|(class, (boxes, frames, score))| {
            json!({
                "class": class,
                "boxes": boxes,
                "frames": frames,
                "mean_score": (score / boxes as f64 * 1000.0).round() / 1000.0,
            })
        })
        .collect();
    classes.sort_by_key(|c| core::cmp::Reverse(c["boxes"].as_u64()));
    let tracks = tracks
        .into_iter()
        .map(|(id, (class, frames, first, last))| {
            json!({ "id": id, "class": class, "frames": frames, "first": first, "last": last })
        })
        .collect();
    (classes, tracks)
}

// ------------------------------------------------------------------ HTTP

/// An error with the status it answers with
struct Status(StatusCode, anyhow::Error);

impl From<anyhow::Error> for Status {
    fn from(e: anyhow::Error) -> Self {
        Status(StatusCode::BAD_REQUEST, e)
    }
}

impl Vision {
    fn get(&self, path: &str, query: &HashMap<String, String>) -> Result<Value, Status> {
        let text = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
        match path {
            "info" => Ok(self.info()),
            "job" => Ok(json!(self.job())),
            "results" => Ok(self.results(text("s"), text("seg"))?),
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }

    fn post(self: &Arc<Self>, path: &str, body: &[u8]) -> Result<Value, Status> {
        let json_body = || -> Result<Value, Status> {
            serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad JSON: {e}").into())
        };
        match path {
            "run" => {
                let request: RunRequest = serde_json::from_slice(body)
                    .map_err(|e| anyhow::anyhow!("bad run request: {e}"))?;
                let running = self.job().is_some_and(|job| !job.finished());
                if running {
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
            "send" => {
                let body = json_body()?;
                let text = |key: &str| body[key].as_str().unwrap_or_default();
                Ok(self.send(text("s"), text("seg"), text("map"))?)
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST {path}"),
            )),
        }
    }
}

/// The routes under `/api/vision/`; files and runs are handled off the
/// async workers
pub fn routes(vision: Arc<Vision>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || warp::path("api").and(warp::path("vision"));
    let reader = Arc::clone(&vision);
    let get = warp::get()
        .and(base())
        .and(warp::path::tail())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |tail: warp::path::Tail, query: HashMap<String, String>| {
                let vision = Arc::clone(&reader);
                blocking(move || vision.get(tail.as_str(), &query))
            },
        );
    let post = warp::post()
        .and(base())
        .and(warp::path::tail())
        .and(warp::body::content_length_limit(BODY_LIMIT))
        .and(warp::body::bytes())
        .and_then(
            move |tail: warp::path::Tail, body: warp::hyper::body::Bytes| {
                let vision = Arc::clone(&vision);
                blocking(move || vision.post(tail.as_str(), &body))
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
    use crate::config::VisionConfig;

    fn settings() -> Settings {
        Settings {
            results: PathBuf::from("/r"),
            size: 'n',
            custom: None,
            confidence: 0.25,
        }
    }

    #[test]
    fn settings_from_config() {
        let dir = Path::new("/cfg");
        let s = Settings::from_config(VisionConfig::default(), dir, PathBuf::from("/d/Vision"))
            .unwrap();
        assert_eq!(s.results, Path::new("/d/Vision"));
        assert_eq!(s.size, 'n');
        assert_eq!(s.custom, None);
        assert_eq!(s.confidence, 0.25);

        let config = VisionConfig {
            results: Some("out".into()),
            size: Some("s".into()),
            weights: Some("w.safetensors".into()),
            classes: Some("classes.json".into()),
            ..Default::default()
        };
        let s = Settings::from_config(config, dir, PathBuf::new()).unwrap();
        assert_eq!(s.results, Path::new("/cfg/out"));
        assert_eq!(s.size, 's');
        let custom = s.custom.unwrap();
        assert_eq!(custom.weights, Path::new("/cfg/w.safetensors"));
        assert_eq!(custom.size, 'n');

        for bad in [
            VisionConfig {
                size: Some("x".into()),
                ..Default::default()
            },
            VisionConfig {
                weights: Some("w".into()),
                ..Default::default()
            },
            VisionConfig {
                confidence: Some(1.5),
                ..Default::default()
            },
        ] {
            assert!(Settings::from_config(bad, dir, PathBuf::new()).is_err());
        }
    }

    #[test]
    fn run_requests() {
        let request: RunRequest =
            serde_json::from_str(r#"{"s": "2026-09-25_11-26-22", "seg": "video-01.mkv"}"#).unwrap();
        assert_eq!((request.start, request.step, request.count), (0, 2, 300));
        assert!(request.track && !request.cpu);
        let key = request.check(&settings()).unwrap();
        assert_eq!(key.model, "n");

        let with = |json: &str| {
            let mut value: Value =
                serde_json::from_str(r#"{"s": "a", "seg": "video-01.mkv"}"#).unwrap();
            for (k, v) in serde_json::from_str::<serde_json::Map<String, Value>>(json).unwrap() {
                value[k] = v;
            }
            serde_json::from_value::<RunRequest>(value)
                .unwrap()
                .check(&settings())
        };
        assert_eq!(with(r#"{"model": "m"}"#).unwrap().model, "m");
        assert!(with(r#"{"model": "x"}"#).is_err());
        assert!(with(r#"{"model": "custom"}"#).is_err());
        assert!(with(r#"{"step": 0}"#).is_err());
        assert!(with(r#"{"count": 0}"#).is_err());
        assert!(with(r#"{"s": "../etc"}"#).is_err());
        assert!(with(r#"{"seg": "a/b.mkv"}"#).is_err());
    }

    #[test]
    fn timing_summary() {
        assert_eq!(stat(&[]), Stat::default());
        let values: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = stat(&values);
        assert_eq!(s.mean, 50.5);
        assert_eq!(s.p95, 95.0);
        let frames = [
            FrameTiming {
                decode: 30.0,
                network: 150.0,
                total: 200.0,
            },
            FrameTiming {
                decode: 1.0,
                network: 99.0,
                total: 100.0,
            },
            FrameTiming {
                decode: 1.0,
                network: 99.0,
                total: 100.0,
            },
        ];
        let t = Timings::of(&frames);
        assert_eq!(t.network.p95, 150.0);
        // The first frame is left out of the rate
        assert_eq!(t.frames_per_s, Some(10.0));
        assert_eq!(Timings::of(&frames[..1]).frames_per_s, None);
    }

    #[test]
    fn detections_take_track_ids() {
        let det = |class, x| ObjectBox::model(class, [x, 0.1, 0.1, 0.1], 0.9);
        let mut detections = vec![det("clock", 0.8), det("boat", 0.1), det("boat", 0.5)];
        let tracked = vec![
            ObjectBox {
                id: Some(7),
                ..det("boat", 0.51)
            },
            ObjectBox {
                id: Some(8),
                ..det("clock", 0.3)
            },
        ];
        assign_ids(&mut detections, &tracked);
        assert_eq!(
            detections.iter().map(|d| d.id).collect::<Vec<_>>(),
            [None, None, Some(7)]
        );
    }

    #[test]
    fn summaries() {
        let b = |class: &str, id: Option<u64>, score| ObjectBox {
            id,
            ..ObjectBox::model(class, [0.1, 0.1, 0.1, 0.1], score)
        };
        let frames = vec![
            FrameObjects::new(10, vec![b("boat", Some(1), 0.5), b("boat", None, 0.7)]),
            FrameObjects::new(12, vec![]),
            FrameObjects::new(14, vec![b("boat", Some(1), 0.6), b("clock", Some(2), 0.9)]),
        ];
        let (classes, tracks) = summarize(&frames);
        assert_eq!(
            classes[0],
            json!({"class": "boat", "boxes": 3, "frames": 2, "mean_score": 0.6})
        );
        assert_eq!(classes[1]["class"], "clock");
        assert_eq!(
            tracks[0],
            json!({"id": 1, "class": "boat", "frames": 2, "first": 10, "last": 14})
        );
        assert_eq!(tracks[1]["first"], 14);
    }

    #[test]
    fn results_are_stored_and_sent_to_labels() {
        let dir = std::env::temp_dir().join(format!("procon-vision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let inspector = Arc::new(Inspector::new(
            Some(dir.join("sessions")),
            crate::recorder::Recorder::new("/tmp/procon-test-"),
            dir.join("calibration.json"),
            dir.join("annotations"),
        ));
        let vision = Vision::new(
            Arc::clone(&inspector),
            Settings {
                results: dir.join("results"),
                ..settings()
            },
        );
        let (s, seg) = ("2026-09-25_11-26-22", "video-01.mkv");
        assert_eq!(vision.results(s, seg).unwrap()["frames"], json!([]));
        assert!(vision.send(s, seg, "").is_err());

        // A stored run: frame 0 finds a person, frame 2 a clock
        let mut frames = vec![
            FrameObjects::new(0, vec![ObjectBox::model("person", [0.1; 4], 0.8)]),
            FrameObjects::new(2, vec![ObjectBox::model("clock", [0.5; 4], 0.9)]),
        ];
        frames[0].extra.insert("ms".into(), json!({"decode": 1.0}));
        labels::write_objects(
            &vision.results_path(s, "video-01", labels::OBJECTS_EXT),
            &frames,
        )
        .unwrap();
        let results = vision.results(s, seg).unwrap();
        assert_eq!(results["frames"][0]["ms"]["decode"], 1.0);
        assert_eq!(results["classes"].as_array().unwrap().len(), 2);

        // A person labeled frame 2 already; the model must not touch it
        let annotations = inspector.annotations();
        let user = crate::objects::ObjectBox {
            class: "chum".into(),
            x: 0.2,
            y: 0.2,
            w: 0.1,
            h: 0.1,
            id: None,
            by: "user".into(),
            score: None,
            extra: Default::default(),
        };
        annotations
            .save_frame(s, seg, 2, vec![user.clone()], &[])
            .unwrap();
        let sent = vision
            .send(s, seg, "person=player, clock=golden_egg")
            .unwrap();
        assert_eq!(sent["added"], 1);
        assert_eq!(sent["kept"], 1);
        assert_eq!(sent["first"], 0);
        let labeled = annotations.read(s, seg).unwrap();
        assert_eq!(labeled[&0].boxes[0].class, "player");
        assert_eq!(labeled[&0].boxes[0].by, "model");
        assert!(!labeled[&0].extra.contains_key("ms"));
        assert_eq!(labeled[&2].boxes, vec![user]);

        // Classes the labels do not know are dropped and counted
        let sent = vision.send(s, seg, "").unwrap();
        assert_eq!(sent["dropped"], json!({"clock": 1, "person": 1}));
        assert!(vision.send(s, seg, "person").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
