//! Inkspector app backend: browse recorded sessions and check the controller
//! labels of every video frame against the picture
//!
//! Sessions are read with `gameplay-data`, the same code the training side
//! uses, so what the Inkspector shows is what a model learns from. Frames are
//! decoded with ffmpeg on request: a seek to just before the frame, then a
//! short window of frames at 360p (recordings have a keyframe every second),
//! kept in a small cache for stepping and playback.
//!
//! All endpoints are `GET` under `/api/inspect/`; `s` is a session folder
//! name under the root, `seg` a segment's video file (default the first) and
//! `delay` the `video_delay_ms` to align with (default the calibrated one):
//!
//! - `sessions`: a summary of every session under the root
//! - `info?s=&seg=`: frame count, fps, delays and calibration of a segment
//! - `frame?s=&seg=&n=`: frame `n` as a 640x360 JPEG
//! - `labels?s=&seg=&start=&stop=&delay=&pred=`: labels of frames
//!   `[start, stop)`, and those of a predictions file (`.jsonl`) if given
//! - `random?s=&seg=&delay=&active=1`: a random frame, with `active` one
//!   where a button changes or the gyro turns
//! - `POST delay` with `{"s": session, "video_delay_ms": ms}` sets a
//!   session's delay by hand in the calibration file, `{"s": session,
//!   "remove": true}` removes it again; answered with the new calibration
//! - `audio?s=&seg=`: the segment's sound track as WebM (Opus copied, not
//!   re-encoded), with HTTP range support for seeking; it starts with the
//!   first frame, so audio time `t` is frame `t * fps`
//! - `classes`: the object classes of the labeling mode (see
//!   [`crate::objects`]), with the annotations folder
//! - `objects?s=&seg=`: every labeled frame of a segment
//! - `POST objects` with `{"s", "seg", "frame", "boxes", "base"}` replaces a
//!   frame's boxes (`base`: the model boxes the page loaded for it) and
//!   answers with the frame as saved

use crate::objects::{Annotations, ObjectBox};
use crate::recorder::Recorder;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use gameplay_data::align::{self, FrameActions};
use gameplay_data::calibration::{
    Calibration, Calibrations, read_calibrations, remove_manual_delay, set_manual_delay,
};
use gameplay_data::controller::ControllerLog;
use gameplay_data::labels::{self, Label};
use gameplay_data::session::{SESSION_FILE, SessionInfo};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Output size of decoded frames
const FRAME_WIDTH: u32 = 640;
const FRAME_HEIGHT: u32 = 360;

/// Frames decoded before a missed frame, for stepping back
const DECODE_BEFORE: usize = 2;

/// Frames decoded from a missed frame on, for stepping forward and playback
const DECODE_AFTER: usize = 45;

/// Decoded frames kept per segment
const CACHED_FRAMES: usize = 400;

/// Segments kept open, with their controller log and cached frames
const OPEN_SEGMENTS: usize = 3;

/// Alignments kept per segment, one per delay tried
const CACHED_ALIGNMENTS: usize = 8;

/// Angular rate above which a frame counts as active for random picks
const ACTIVE_GYRO_DPS: f64 = 60.0;

/// Sessions the Inkspector reads, and what it keeps open
pub struct Inspector {
    /// Folder holding the session folders; None follows the recording prefix
    root: Option<PathBuf>,
    recorder: Recorder,
    /// AgentZero's `calibration.json`
    calibration: PathBuf,
    open: Mutex<Vec<Arc<OpenSegment>>>,
    /// Video header facts by file, which never change once recorded
    probes: Mutex<HashMap<PathBuf, Arc<Probe>>>,
    /// Last predictions file read, with its labels by frame
    predictions: Mutex<Option<(PathBuf, Arc<Predictions>)>>,
    /// Object labels of the labeling mode
    annotations: Annotations,
}

/// Body of an Inkspector reply
pub struct Reply {
    pub body: Vec<u8>,
    pub content_type: &'static str,
    /// `Content-Range` of a partial reply (status 206)
    pub content_range: Option<String>,
}

impl Reply {
    fn whole(body: Vec<u8>, content_type: &'static str) -> Self {
        Self {
            body,
            content_type,
            content_range: None,
        }
    }
}

/// Predicted labels by frame
type Predictions = BTreeMap<u64, Label>;

/// What ffprobe says about a video
#[derive(Default)]
struct Probe {
    width: u32,
    height: u32,
    fps: f64,
    /// Frame count from the video stream's `DURATION` tag, if written
    duration_s: Option<f64>,
    /// Sorted frame presentation times in ms, for variable-rate files only
    pts_ms: Option<Vec<f64>>,
}

/// One segment being inspected
struct OpenSegment {
    session: String,
    file: String,
    video: PathBuf,
    start_unix_ms: u64,
    fps: f64,
    probe: Arc<Probe>,
    frames: usize,
    log: ControllerLog,
    calibration: Option<Calibration>,
    summary: Value,
    has_audio: bool,
    /// The sound track as WebM, extracted on first request
    audio: Mutex<Option<Arc<Vec<u8>>>>,
    alignments: Mutex<Vec<(u64, Arc<FrameActions>)>>,
    jpegs: Mutex<BTreeMap<usize, Arc<Vec<u8>>>>,
    /// Held while ffmpeg decodes, so parallel misses wait for one window
    decoding: Mutex<()>,
}

impl Inspector {
    /// `root` of None reads the recording prefix's folder
    pub fn new(
        root: Option<PathBuf>,
        recorder: Recorder,
        calibration: PathBuf,
        annotations: PathBuf,
    ) -> Self {
        Self {
            root,
            recorder,
            calibration,
            open: Mutex::default(),
            probes: Mutex::default(),
            predictions: Mutex::default(),
            annotations: Annotations::new(annotations),
        }
    }

    /// Folder holding the session folders
    pub fn root(&self) -> PathBuf {
        self.root
            .clone()
            .unwrap_or_else(|| self.recorder.prefix_dir())
    }

    fn calibrations(&self) -> Calibrations {
        read_calibrations(&self.calibration).unwrap_or_else(|e| {
            log::warn!("Ignoring the calibration file: {:#}", e);
            Calibrations::new()
        })
    }

    /// Summaries of every session under the root, newest first
    pub fn sessions(&self) -> Result<Value> {
        let root = self.root();
        let calibrations = self.calibrations();
        let mut names: Vec<String> = std::fs::read_dir(&root)
            .with_context(|| format!("cannot list {}", root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().join(SESSION_FILE).is_file())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort_unstable_by(|a, b| b.cmp(a));
        let sessions: Vec<Value> = names
            .iter()
            .filter_map(|name| {
                self.summary(&root.join(name), calibrations.get(name))
                    .inspect_err(|e| log::warn!("Skipping session {name}: {:#}", e))
                    .ok()
            })
            .collect();
        Ok(json!({ "root": root, "sessions": sessions }))
    }

    /// What the picker shows about a session; only older sessions, which do
    /// not record their height and fps, have a video header read
    fn summary(&self, dir: &Path, calibration: Option<&Calibration>) -> Result<Value> {
        let info = SessionInfo::read(dir)?;
        let video = &info.video;
        let (mut height, mut fps) = (video.height, video.fps.map(f64::from));
        if (height.is_none() || fps.is_none())
            && let Some(first) = video.segments.first()
        {
            let probe = self.probe(&dir.join(&first.file), false)?;
            height = height.or(Some(probe.height));
            fps = fps.or(Some(probe.fps));
        }
        let name = dir.file_name().unwrap_or_default().to_string_lossy();
        Ok(json!({
            "name": name,
            "started_at_unix_ms": info.started_at_unix_ms,
            "stopped_at_unix_ms": info.stopped_at_unix_ms,
            "segments": video.segments.iter().map(|s| json!({
                "file": s.file,
                "sound": s.has_audio(),
            })).collect::<Vec<_>>(),
            "height": height,
            "fps": fps,
            "sound": video.segments.iter().any(|s| s.has_audio()),
            "controller_reports": info.controller.frames,
            "game_settings": info.game_settings,
            "calibration": calibration_json(calibration),
        }))
    }

    /// Header facts of a video, and its frame times if it has no
    /// `DURATION` tag or `packets` is asked for (reads the whole file once)
    fn probe(&self, video: &Path, packets: bool) -> Result<Arc<Probe>> {
        if let Some(probe) = self.probes.lock().unwrap().get(video)
            && (!packets || probe.pts_ms.is_some())
        {
            return Ok(Arc::clone(probe));
        }
        let header = ffprobe(
            video,
            "stream=width,height,r_frame_rate:stream_tags=DURATION",
        )?;
        let stream = &header["streams"][0];
        let (num, den) = stream["r_frame_rate"]
            .as_str()
            .and_then(|rate| rate.split_once('/'))
            .context("no frame rate")?;
        let mut probe = Probe {
            width: stream["width"].as_u64().unwrap_or_default() as u32,
            height: stream["height"].as_u64().unwrap_or_default() as u32,
            fps: num.parse::<f64>()? / den.parse::<f64>()?,
            duration_s: stream["tags"]["DURATION"].as_str().and_then(parse_duration),
            pts_ms: None,
        };
        if packets {
            let all = ffprobe(video, "packet=pts_time")?;
            let mut pts: Vec<f64> = all["packets"]
                .as_array()
                .context("no packets")?
                .iter()
                .filter_map(|p| p["pts_time"].as_str()?.parse::<f64>().ok())
                .map(|s| s * 1000.0)
                .collect();
            pts.sort_by(f64::total_cmp);
            probe.pts_ms = Some(pts);
        }
        let probe = Arc::new(probe);
        self.probes
            .lock()
            .unwrap()
            .insert(video.to_path_buf(), Arc::clone(&probe));
        Ok(probe)
    }

    /// The open segment `file` (default the first) of session `name`
    fn segment(&self, name: &str, file: Option<&str>) -> Result<Arc<OpenSegment>> {
        ensure!(
            !name.is_empty() && !name.contains(['/', '\\']) && name != "..",
            "bad session name {name:?}"
        );
        {
            let mut open = self.open.lock().unwrap();
            if let Some(i) = open
                .iter()
                .position(|s| s.session == name && file.is_none_or(|f| f == s.file))
            {
                let segment = open.remove(i);
                open.push(Arc::clone(&segment));
                return Ok(segment);
            }
        }
        let dir = self.root().join(name);
        let info = SessionInfo::read(&dir)?;
        let entry = match file {
            Some(file) => info.video.segments.iter().find(|s| s.file == file),
            None => info.video.segments.first(),
        }
        .with_context(|| format!("{name} has no segment {}", file.unwrap_or("")))?;
        let start_unix_ms = entry
            .start_unix_ms
            .with_context(|| format!("{} has no frames", entry.file))?;
        let video = dir.join(&entry.file);
        let mut probe = self.probe(&video, false)?;
        let fps = info.video.fps.map(f64::from);
        let frames = match (fps, probe.duration_s) {
            (Some(fps), Some(duration)) => (duration * fps).round() as usize,
            _ => {
                probe = self.probe(&video, true)?;
                probe.pts_ms.as_ref().map_or(0, Vec::len)
            }
        };
        let calibrations = self.calibrations();
        let segment = Arc::new(OpenSegment {
            session: name.to_string(),
            file: entry.file.clone(),
            video,
            start_unix_ms,
            fps: fps.unwrap_or(probe.fps),
            probe,
            frames,
            log: ControllerLog::read(&dir.join(&info.controller.file))?,
            calibration: calibrations.get(name).cloned(),
            summary: self.summary(&dir, calibrations.get(name))?,
            has_audio: entry.has_audio(),
            audio: Mutex::default(),
            alignments: Mutex::default(),
            jpegs: Mutex::default(),
            decoding: Mutex::default(),
        });
        let mut open = self.open.lock().unwrap();
        open.push(Arc::clone(&segment));
        if open.len() > OPEN_SEGMENTS {
            open.remove(0);
        }
        Ok(segment)
    }

    /// Frame count, fps, delays and calibration of a segment
    pub fn info(&self, name: &str, file: Option<&str>) -> Result<Value> {
        let segment = self.segment(name, file)?;
        Ok(json!({
            "session": segment.session,
            "segment": segment.file,
            "frames": segment.frames,
            "fps": segment.fps,
            "controller_shift_ms": 0.0,
            "video_delay_ms": segment.default_delay(),
            "calibration": calibration_json(segment.calibration.as_ref()),
            "sound": segment.has_audio,
            "summary": segment.summary,
        }))
    }

    /// Frame `n` of a segment as JPEG
    pub fn frame(&self, name: &str, file: Option<&str>, n: usize) -> Result<Arc<Vec<u8>>> {
        let segment = self.segment(name, file)?;
        ensure!(n < segment.frames, "no frame {n}");
        segment.jpeg(n)
    }

    /// Labels of frames `[start, stop)` aligned with `delay`, and those of
    /// the predictions file `pred` if given
    pub fn labels(
        &self,
        name: &str,
        file: Option<&str>,
        range: core::ops::Range<usize>,
        delay: Option<f64>,
        pred: Option<&str>,
    ) -> Result<Value> {
        let segment = self.segment(name, file)?;
        let range = range.start.min(segment.frames)..range.end.min(segment.frames);
        let actions = segment.actions(delay.unwrap_or_else(|| segment.default_delay()));
        let truth = labels::frame_labels(&actions, range.clone());
        let predictions = pred
            .map(|path| -> Result<Value> {
                let predicted = self.predictions(Path::new(path))?;
                Ok(range
                    .map(|n| {
                        predicted
                            .get(&(n as u64))
                            .map_or(Value::Null, |label| json!(label))
                    })
                    .collect())
            })
            .transpose()?;
        Ok(json!({ "truth": truth, "predictions": predictions }))
    }

    /// Labels of a predictions file by frame, read once per file
    fn predictions(&self, path: &Path) -> Result<Arc<Predictions>> {
        ensure!(
            path.extension().is_some_and(|e| e == "jsonl"),
            "predictions must be a .jsonl file"
        );
        let mut cached = self.predictions.lock().unwrap();
        if let Some((cached_path, labels)) = cached.as_ref()
            && cached_path == path
        {
            return Ok(Arc::clone(labels));
        }
        let labels: Predictions = labels::read_labels(path)?
            .into_iter()
            .map(|label| (label.frame, label))
            .collect();
        let labels = Arc::new(labels);
        *cached = Some((path.to_path_buf(), Arc::clone(&labels)));
        Ok(labels)
    }

    /// A random frame; with `active`, one where a button changes or the
    /// gyro turns faster than [`ACTIVE_GYRO_DPS`] (if there is any)
    pub fn random(
        &self,
        name: &str,
        file: Option<&str>,
        delay: Option<f64>,
        active: bool,
    ) -> Result<usize> {
        let segment = self.segment(name, file)?;
        ensure!(segment.frames > 0, "the segment has no frames");
        let mut candidates = Vec::new();
        if active {
            let actions = segment.actions(delay.unwrap_or_else(|| segment.default_delay()));
            for n in 0..actions.len() {
                let changed = n > 0 && actions.buttons[n] != actions.buttons[n - 1];
                let [x, y, z] = actions.gyro[n].map(f64::from);
                let turning = (x * x + y * y + z * z).sqrt() * segment.fps > ACTIVE_GYRO_DPS;
                if changed || turning {
                    candidates.push(n);
                }
            }
        }
        let pick = random_below(if candidates.is_empty() {
            segment.frames
        } else {
            candidates.len()
        });
        Ok(candidates.get(pick).copied().unwrap_or(pick))
    }
}

impl Inspector {
    /// Set a session's delay by hand (`Some`) or remove the one set by hand
    /// (`None`), then answer with the session's calibration as the page
    /// shows it
    pub fn set_delay(&self, name: &str, delay_ms: Option<f64>) -> Result<Value> {
        ensure!(
            self.root().join(name).join(SESSION_FILE).is_file(),
            "no session {name}"
        );
        match delay_ms {
            Some(delay) => set_manual_delay(&self.calibration, name, delay)?,
            None => {
                remove_manual_delay(&self.calibration, name)?;
            }
        }
        // Open segments keep the calibration they were opened with
        self.open.lock().unwrap().retain(|s| s.session != name);
        Ok(calibration_json(self.calibrations().get(name)))
    }

    /// Replace the boxes of a frame from `{"s", "seg", "frame", "boxes",
    /// "base"}`; answers with the frame as saved
    pub fn save_objects(&self, body: &Value) -> Result<Value> {
        let session = body["s"].as_str().context("no session given")?;
        let segment = self.segment(session, body["seg"].as_str().filter(|s| !s.is_empty()))?;
        let frame = body["frame"].as_u64().context("no frame given")?;
        ensure!((frame as usize) < segment.frames, "no frame {frame}");
        let boxes = |key: &str| -> Result<Vec<ObjectBox>> {
            match &body[key] {
                Value::Null => Ok(Vec::new()),
                value => {
                    serde_json::from_value(value.clone()).with_context(|| format!("bad {key}"))
                }
            }
        };
        let saved = self.annotations.save_frame(
            session,
            &segment.file,
            frame,
            boxes("boxes")?,
            &boxes("base")?,
        )?;
        Ok(json!(saved))
    }

    /// Answer `GET /api/inspect/<endpoint>?<query>`: the body and its
    /// content type
    pub fn handle(
        &self,
        endpoint: &str,
        query: &HashMap<String, String>,
        range: Option<&str>,
    ) -> Result<Reply> {
        let text = |key: &str| query.get(key).map(String::as_str);
        let session = || text("s").context("no session given");
        let number = |key: &str| -> Result<Option<f64>> {
            text(key)
                .map(|value| value.parse().with_context(|| format!("bad {key}")))
                .transpose()
        };
        let index = |key: &str| -> Result<usize> {
            Ok(text(key)
                .with_context(|| format!("no {key} given"))?
                .parse()?)
        };
        let seg = text("seg").filter(|s| !s.is_empty());
        let value = match endpoint {
            "sessions" => self.sessions()?,
            "info" => self.info(session()?, seg)?,
            "frame" => {
                let jpeg = self.frame(session()?, seg, index("n")?)?;
                return Ok(Reply::whole(jpeg.to_vec(), "image/jpeg"));
            }
            "audio" => {
                let audio = self.segment(session()?, seg)?.audio()?;
                return Ok(ranged(&audio, range, "audio/webm"));
            }
            "labels" => self.labels(
                session()?,
                seg,
                index("start")?..index("stop")?,
                number("delay")?,
                text("pred").filter(|p| !p.is_empty()),
            )?,
            "classes" => json!({
                "dir": self.annotations.dir(),
                "classes": self.annotations.classes()?,
            }),
            "objects" => {
                let segment = self.segment(session()?, seg)?;
                let frames = self.annotations.read(&segment.session, &segment.file)?;
                json!({ "frames": frames.into_values().collect::<Vec<_>>() })
            }
            "random" => json!({
                "frame": self.random(session()?, seg, number("delay")?, text("active") != Some("0"))?,
            }),
            _ => bail!("no endpoint {endpoint}"),
        };
        Ok(Reply::whole(
            value.to_string().into_bytes(),
            "application/json",
        ))
    }
}

impl OpenSegment {
    /// The calibrated delay if its confidence is high or medium, else 0
    fn default_delay(&self) -> f64 {
        self.calibration
            .as_ref()
            .and_then(Calibration::applied_delay_ms)
            .unwrap_or(0.0)
    }

    /// Actions of every frame aligned with `delay` ms of video delay
    fn actions(&self, delay: f64) -> Arc<FrameActions> {
        let key = delay.to_bits();
        let mut alignments = self.alignments.lock().unwrap();
        if let Some((_, actions)) = alignments.iter().find(|(k, _)| *k == key) {
            return Arc::clone(actions);
        }
        let times = match &self.probe.pts_ms {
            // As the training code does: frame n at start + its timestamp
            Some(pts_ms) => align::variable_rate_times(self.start_unix_ms, pts_ms, delay),
            None => align::constant_rate_times(self.start_unix_ms, self.frames, self.fps, delay),
        };
        let actions = Arc::new(align::align(&self.log, &times, 1000.0 / self.fps, 0.0));
        alignments.push((key, Arc::clone(&actions)));
        if alignments.len() > CACHED_ALIGNMENTS {
            alignments.remove(0);
        }
        actions
    }

    /// The sound track as WebM, extracted once: ffmpeg copies the Opus
    /// packets into a file (so the WebM gets its duration and seek index)
    fn audio(&self) -> Result<Arc<Vec<u8>>> {
        ensure!(self.has_audio, "{} has no sound track", self.file);
        let mut audio = self.audio.lock().unwrap();
        if let Some(audio) = audio.as_ref() {
            return Ok(Arc::clone(audio));
        }
        let path = std::env::temp_dir().join(format!(
            "procon-inspect-{}-{}.webm",
            std::process::id(),
            random_below(usize::MAX)
        ));
        let output = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(&self.video)
            .args(["-vn", "-c:a", "copy", "-f", "webm"])
            .arg(&path)
            .output()
            .context("cannot run ffmpeg")?;
        let bytes = std::fs::read(&path);
        let _ = std::fs::remove_file(&path);
        ensure!(
            output.status.success(),
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let bytes = Arc::new(bytes?);
        *audio = Some(Arc::clone(&bytes));
        Ok(bytes)
    }

    /// Seconds from the start of the file to just before frame `n`
    fn seek_s(&self, n: usize) -> f64 {
        let ms = match &self.probe.pts_ms {
            Some(pts) => pts[n] - pts[0] - 0.5,
            // Container timestamps are rounded to 1 ms; half a frame early is
            // safely after the previous frame
            None => (n as f64 - 0.5) * 1000.0 / self.fps,
        };
        ms.max(0.0) / 1000.0
    }

    /// Frame `n` as JPEG, decoding a window around it on a miss
    fn jpeg(&self, n: usize) -> Result<Arc<Vec<u8>>> {
        if let Some(jpeg) = self.jpegs.lock().unwrap().get(&n) {
            return Ok(Arc::clone(jpeg));
        }
        let _decoding = self.decoding.lock().unwrap();
        // Another request may have decoded it meanwhile
        if let Some(jpeg) = self.jpegs.lock().unwrap().get(&n) {
            return Ok(Arc::clone(jpeg));
        }
        let first = n.saturating_sub(DECODE_BEFORE);
        let count = (n + DECODE_AFTER).min(self.frames) - first;
        let decoded = self.decode(first, count)?;
        ensure!(n - first < decoded.len(), "frame {n} did not decode");
        let mut jpegs = self.jpegs.lock().unwrap();
        for (i, jpeg) in decoded.into_iter().enumerate() {
            jpegs.insert(first + i, Arc::new(jpeg));
        }
        // Forget the frames farthest from this one
        while jpegs.len() > CACHED_FRAMES {
            let (&low, _) = jpegs.first_key_value().unwrap();
            let (&high, _) = jpegs.last_key_value().unwrap();
            let far = if n - low.min(n) > high.max(n) - n {
                low
            } else {
                high
            };
            jpegs.remove(&far);
        }
        Ok(Arc::clone(&jpegs[&n]))
    }

    /// Decode `count` frames from frame `first` on as 640x360 JPEGs
    fn decode(&self, first: usize, count: usize) -> Result<Vec<Vec<u8>>> {
        let mut args: Vec<String> = ["-v", "error", "-ss"].map(String::from).to_vec();
        args.push(format!("{:.4}", self.seek_s(first)));
        args.extend(["-i".to_string(), self.video.display().to_string()]);
        args.extend(["-frames:v".to_string(), count.to_string()]);
        if (self.probe.width, self.probe.height) != (FRAME_WIDTH, FRAME_HEIGHT) {
            args.extend([
                "-vf".to_string(),
                format!("scale={FRAME_WIDTH}:{FRAME_HEIGHT}"),
            ]);
        }
        args.extend(
            ["-an", "-fps_mode", "passthrough", "-f", "image2pipe"]
                .into_iter()
                .chain(["-c:v", "mjpeg", "-q:v", "3", "-"])
                .map(String::from),
        );
        let output = Command::new("ffmpeg")
            .args(&args)
            .output()
            .context("cannot run ffmpeg")?;
        if !output.status.success() {
            bail!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(split_jpegs(&output.stdout))
    }
}

/// Split concatenated JPEGs at their end markers (`FF D9`); inside the
/// entropy-coded data an `FF` byte is always followed by `00` or a restart
/// marker, so the end marker cannot appear early
fn split_jpegs(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut jpegs = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD9 {
            jpegs.push(bytes[start..i + 2].to_vec());
            start = i + 2;
            i += 2;
        } else {
            i += 1;
        }
    }
    jpegs
}

/// `bytes`, or the part a `Range: bytes=start-end` header asks for
fn ranged(bytes: &[u8], range: Option<&str>, content_type: &'static str) -> Reply {
    let total = bytes.len();
    let asked = range
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.split_once('-'))
        .and_then(|(start, end)| {
            let start: usize = start.parse().ok()?;
            let end = match end {
                "" => total.checked_sub(1)?,
                end => end.parse::<usize>().ok()?.min(total.checked_sub(1)?),
            };
            (start <= end).then_some((start, end))
        });
    match asked {
        Some((start, end)) => Reply {
            body: bytes[start..=end].to_vec(),
            content_type,
            content_range: Some(format!("bytes {start}-{end}/{total}")),
        },
        None => Reply::whole(bytes.to_vec(), content_type),
    }
}

/// Run ffprobe on the first video stream and return its JSON output
pub(crate) fn ffprobe(video: &Path, entries: &str) -> Result<Value> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            entries,
        ])
        .args(["-of", "json"])
        .arg(video)
        .output()
        .context("cannot run ffprobe")?;
    ensure!(
        output.status.success(),
        "ffprobe failed on {}: {}",
        video.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Seconds in a Matroska `DURATION` tag such as `00:07:07.766000000`
fn parse_duration(tag: &str) -> Option<f64> {
    let mut parts = tag.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// A session's own estimate; for a delay set by hand, the computed one kept
/// next to it
fn own_estimate(calibration: &Calibration) -> Option<Value> {
    let fields = |e: &Value| {
        json!({
            "video_delay_ms": e["video_delay_ms"],
            "interval_ms": e["interval_ms"],
            "confidence": e["confidence"],
        })
    };
    if calibration.source.as_deref() == Some("manual") {
        return calibration.extra.get("computed").map(fields);
    }
    Some(fields(&serde_json::to_value(calibration).ok()?))
}

/// What the page shows about a session's delay: the one applied, with its
/// source and interval, the own estimate and, when none applies, why
fn calibration_json(calibration: Option<&Calibration>) -> Value {
    let applied = calibration.and_then(Calibration::applied);
    let reason = match (calibration, applied) {
        (_, Some(_)) => None,
        (None, None) => Some("not calibrated yet (agentzero-calibrate)"),
        (Some(_), None) => {
            Some("too little aiming or jumping to measure, and no measured session of this setup")
        }
    };
    json!({
        "applied": applied,
        "reason": reason,
        "own": calibration.and_then(own_estimate),
    })
}

/// A number in `[0, bound)` from the clock; good enough to pick a frame
fn random_below(bound: usize) -> usize {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    // xorshift to spread the clock's low bits
    let mut x = nanos ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x % bound.max(1) as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpegs_split_at_end_markers() {
        let bytes = [
            0xFF, 0xD8, 1, 0xFF, 0x00, 0xFF, 0xD9, 0xFF, 0xD8, 2, 0xFF, 0xD9,
        ];
        let jpegs = split_jpegs(&bytes);
        assert_eq!(jpegs.len(), 2);
        assert_eq!(jpegs[0], [0xFF, 0xD8, 1, 0xFF, 0x00, 0xFF, 0xD9]);
        assert_eq!(jpegs[1], [0xFF, 0xD8, 2, 0xFF, 0xD9]);
    }

    #[test]
    fn byte_ranges() {
        let bytes = [0, 1, 2, 3, 4];
        let part = ranged(&bytes, Some("bytes=1-2"), "audio/webm");
        assert_eq!(part.body, [1, 2]);
        assert_eq!(part.content_range.as_deref(), Some("bytes 1-2/5"));
        assert_eq!(ranged(&bytes, Some("bytes=3-"), "").body, [3, 4]);
        assert_eq!(ranged(&bytes, Some("bytes=2-99"), "").body, [2, 3, 4]);
        let whole = ranged(&bytes, None, "");
        assert_eq!((whole.body.len(), whole.content_range), (5, None));
        assert!(
            ranged(&bytes, Some("bytes=4-1"), "")
                .content_range
                .is_none()
        );
    }

    #[test]
    fn duration_tag() {
        assert_eq!(parse_duration("00:07:07.766000000"), Some(427.766));
        assert_eq!(parse_duration("01:00:00.5"), Some(3600.5));
        assert_eq!(parse_duration("bad"), None);
    }

    #[test]
    fn random_is_below_bound() {
        assert!((0..100).all(|_| random_below(7) < 7));
        assert_eq!(random_below(0), 0);
    }
}
