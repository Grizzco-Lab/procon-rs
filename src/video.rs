//! Video capture with ffmpeg: a live preview, plus recording to files
//!
//! Three kinds of ffmpeg share the work, joined by the studio:
//!
//! - The grabber owns the input for as long as it is selected and turns it
//!   into raw 1080p frames at the capture rate. Reopening a capture card is
//!   slow (the Elgato 4K X only streams on every other start), so nothing but
//!   choosing another input restarts it.
//! - The preview encoder gets those frames thinned to the preview rate,
//!   scales them to the preview size and encodes low-latency H.264 as
//!   fragmented MP4, one frame per fragment, which the dashboard plays with a
//!   `<video>` element. Every frame sent comes out as one fragment, which is
//!   how its encoding time is measured. Changing the preview restarts only
//!   this process.
//! - A recording encoder per video file, fed the same frames from Record on,
//!   so a file begins with the next frame and pausing never touches the input.
//!
//! Frames reach recordings at a constant rate: frame `n` of a file was captured
//! `n / fps` seconds after its first frame, whose Unix time is kept. That time
//! is when the capture card delivered the frame to the kernel, which ffmpeg
//! reports for every frame, not when it reached the studio: if the grabber
//! ever falls behind, frames queue in the driver and arrive late for good, so
//! arrival times would shift a whole recording by however long that queue is.
//! A queue that builds up while nothing records is cleared by restarting the
//! grabber.
//!
//! With sound on, a recording also gets the capture card's sound
//! ([`Audio`]) through a second pipe, starting at the sample that arrived
//! with its first frame, as a second track of the same file.

use crate::audio::{self, Audio};
use crate::config::VideoConfig;
use crate::dump::unix_ms;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Instant;
use tokio::sync::broadcast;

/// How long a new grabber may take to deliver its first frame
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

/// Frames queued for a recording encoder that is still starting up
const RECORDING_QUEUE_SECS: u32 = 1;

/// Frames queued for the preview encoder; a busy preview skips frames instead
const PREVIEW_QUEUE: usize = 4;

/// Frames arriving this late after capture mean a queue has built up
const BACKLOG_MS: f64 = 120.0;

/// How long a queue may last before the grabber restarts to clear it
const BACKLOG_PATIENCE: Duration = Duration::from_secs(3);

/// Recording and preview heights the dashboard offers; widths follow 16:9
pub const RECORD_HEIGHTS: [u32; 4] = [1080, 720, 540, 360];

/// Size of the grabber's raw frames, the largest the dashboard offers
const WORK_SIZE: (u32, u32) = (1920, 1080);

/// Bytes of one raw 4:2:0 frame at [`WORK_SIZE`]
const FRAME_BYTES: usize = (WORK_SIZE.0 * WORK_SIZE.1 * 3 / 2) as usize;

/// Input id for capturing the X11 screen
pub const SCREEN: &str = "screen";

/// A capture source the dashboard can pick
#[derive(Debug, Clone, Serialize)]
pub struct VideoInput {
    /// `screen` or a V4L2 device path
    pub id: String,
    pub name: String,
}

/// List the screen (when an X11 display is set) and V4L2 capture devices
pub fn list_inputs() -> Vec<VideoInput> {
    let mut inputs = Vec::new();
    if let Ok(display) = std::env::var("DISPLAY") {
        inputs.push(VideoInput {
            id: SCREEN.to_string(),
            name: format!("Screen ({display})"),
        });
    }

    let mut devices: Vec<VideoInput> = std::fs::read_dir("/sys/class/video4linux")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            // Index 0 is the capture node; others are metadata nodes of the same device
            let index = std::fs::read_to_string(dir.join("index")).ok()?;
            if index.trim() != "0" {
                return None;
            }
            let name = std::fs::read_to_string(dir.join("name")).ok()?;
            let id = format!("/dev/{}", entry.file_name().to_string_lossy());
            Some(VideoInput {
                name: format!("{} ({id})", name.trim()),
                id,
            })
        })
        .collect();
    devices.sort_by(|a, b| a.id.cmp(&b.id));
    inputs.extend(devices);
    inputs
}

/// Width and height of 16:9 frames at one of the offered heights
fn size_16_9(height: u32) -> (u32, u32) {
    let height = if RECORD_HEIGHTS.contains(&height) {
        height
    } else {
        RECORD_HEIGHTS[0]
    };
    // Even sizes, as 4:2:0 frames need
    ((height * 16 / 9 + 1) & !1, height)
}

/// One piece of the preview stream for the browser's media player
#[derive(Clone)]
pub struct PreviewChunk {
    /// MP4 bytes: `ftyp` + `moov` for an init segment, `moof` + `mdat` otherwise
    pub data: Arc<Vec<u8>>,
    pub kind: ChunkKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// Codec setup; a player starts over with it
    Init,
    /// A frame that decodes on its own; a player can join here
    Key,
    /// A frame that needs the ones before it
    Delta,
}

/// Snapshot of the video capture for the dashboard
#[derive(Debug, Serialize)]
pub struct VideoStatus {
    /// Selected input id, if any
    pub input: Option<String>,
    pub inputs: Vec<VideoInput>,
    /// File being recorded
    pub recording: Option<String>,
    /// Preview frames arrived within the last two seconds
    pub live: bool,
    /// Recorded height
    pub record_height: u32,
    /// Recorded frame rate
    pub record_fps: u32,
    /// Preview height and frame rate in use
    pub preview_height: u32,
    pub preview_fps: u32,
    /// Time from a frame reaching the preview encoder to its fragment coming
    /// out, smoothed; `None` while the preview is not live
    pub preview_encode_ms: Option<f64>,
    /// Time from the capture card delivering a frame to the studio reading
    /// it, smoothed; `None` while no frames arrive
    pub capture_ms: Option<f64>,
    /// The preview follows the recording size and rate
    pub preview_matches_recording: bool,
    /// Recordings get the sound track, when there is a sound source
    pub record_audio: bool,
    /// Sound source configured, and whether its sound is arriving
    pub audio_input: Option<String>,
    pub audio_live: bool,
    /// Why ffmpeg is not running
    pub error: Option<String>,
}

/// A raw frame shared by the preview and the recording
type SharedFrame = Arc<Vec<u8>>;

/// The preview encoder's input, and how to thin the capture rate to its rate
#[derive(Clone)]
struct PreviewFeed {
    frames: SyncSender<SharedFrame>,
    /// Keep `fps` frames of every `capture_fps`
    fps: u32,
    capture_fps: u32,
}

/// An ffmpeg fed raw frames on stdin through a queue
struct Worker {
    child: Child,
    /// Frames for the process; dropping it ends its input
    frames: SyncSender<SharedFrame>,
    writer: JoinHandle<()>,
}

/// A file being encoded from the grabbed frames
struct Recording {
    path: PathBuf,
    worker: Worker,
    /// Unix ms of the file's first frame, once it arrived
    first_frame_ms: Option<u64>,
    /// Sound input of the encoder, until the first frame starts the sound
    audio_input: Option<SyncSender<SharedFrame>>,
    /// Writes the sound into the encoder's second pipe
    audio_writer: Option<JoinHandle<()>>,
    /// Unix ms of the first sample in the file
    audio_start_ms: Option<u64>,
    /// Frames lost because the encoder fell behind
    dropped: u64,
}

struct Inner {
    config: VideoConfig,
    input: Option<String>,
    /// The preview uses the recording size and rate instead of its own
    preview_matches_recording: bool,
    record_audio: bool,
    grabber: Option<Child>,
    preview: Option<Worker>,
    error: Option<String>,
}

/// Handle to the capture, preview and recording processes; clones share them
#[derive(Clone)]
pub struct Video {
    inner: Arc<Mutex<Inner>>,
    /// Where grabbed frames go; separate locks, as the grabber's reader takes
    /// them for every frame
    preview_feed: Arc<Mutex<Option<PreviewFeed>>>,
    /// When each frame still inside the preview encoder was sent to it
    preview_sent: Arc<Mutex<VecDeque<Instant>>>,
    /// Smoothed preview encoding time in µs
    preview_encode_us: Arc<AtomicU64>,
    recording: Arc<Mutex<Option<Recording>>>,
    /// Preview stream for the browsers
    preview: broadcast::Sender<PreviewChunk>,
    /// The current init segment, for browsers that join later
    preview_init: Arc<Mutex<Option<PreviewChunk>>>,
    /// Bumped before every grabber or preview restart so threads of an old
    /// ffmpeg know they are stale. Kept outside the lock: an old ffmpeg's pipes
    /// must be drained without it, or it blocks on a full pipe and never exits.
    grabber_generation: Arc<AtomicU64>,
    preview_generation: Arc<AtomicU64>,
    /// Unix ms of the latest grabbed frame and preview fragment
    last_frame_ms: Arc<AtomicU64>,
    last_preview_ms: Arc<AtomicU64>,
    /// Capture times (Unix µs) the grabber logged for frames not read yet
    capture_times: Arc<Mutex<VecDeque<u64>>>,
    /// Smoothed time from capture to reading a frame, in µs
    capture_us: Arc<AtomicU64>,
    /// The sound source, when one is configured
    audio: Option<Audio>,
}

/// When a finished video file starts
pub struct FileTimes {
    /// Unix ms of its first frame
    pub first_frame_ms: Option<u64>,
    /// Unix ms of its first sound sample, if it has sound
    pub audio_start_ms: Option<u64>,
}

/// Lock a mutex even if a holder panicked; the data stays usable
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Video {
    /// Start capturing `input` (if any), and the sound source (if configured)
    pub fn new(
        config: VideoConfig,
        input: Option<String>,
        preview_matches_recording: bool,
        record_audio: bool,
    ) -> Self {
        let audio = Some(config.audio_input.clone())
            .filter(|source| !source.is_empty())
            .map(Audio::start);
        let video = Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                input,
                preview_matches_recording,
                record_audio,
                grabber: None,
                preview: None,
                error: None,
            })),
            preview_feed: Arc::default(),
            preview_sent: Arc::default(),
            preview_encode_us: Arc::default(),
            recording: Arc::default(),
            // A few seconds of frames; a browser further behind catches up at a keyframe
            preview: broadcast::channel(256).0,
            preview_init: Arc::default(),
            grabber_generation: Arc::default(),
            preview_generation: Arc::default(),
            last_frame_ms: Arc::default(),
            last_preview_ms: Arc::default(),
            capture_times: Arc::default(),
            capture_us: Arc::default(),
            audio,
        };
        let mut inner = video.lock();
        video.restart_grabber(&mut inner);
        video.restart_preview(&mut inner);
        drop(inner);
        video
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    /// Receive the preview: the current init segment, then the live stream
    pub fn subscribe(&self) -> (Option<PreviewChunk>, broadcast::Receiver<PreviewChunk>) {
        let init = lock(&self.preview_init);
        (init.clone(), self.preview.subscribe())
    }

    /// Make the preview follow the recording size and rate, or use its own
    pub fn set_preview_matches_recording(&self, matches: bool) -> Result<()> {
        let mut inner = self.lock();
        if inner.preview_matches_recording != matches {
            inner.preview_matches_recording = matches;
            self.restart_preview(&mut inner);
        }
        Ok(())
    }

    /// Whether the preview follows the recording size and rate
    pub fn preview_matches_recording(&self) -> bool {
        self.lock().preview_matches_recording
    }

    /// Give recordings from the next file on the sound track, or not
    pub fn set_record_audio(&self, enabled: bool) {
        self.lock().record_audio = enabled;
    }

    pub fn record_audio(&self) -> bool {
        self.lock().record_audio
    }

    /// Switch to another input, or none; not while recording
    pub fn set_input(&self, input: Option<String>) -> Result<()> {
        ensure!(
            lock(&self.recording).is_none(),
            "stop recording before changing the video input"
        );
        let mut inner = self.lock();
        inner.input = input;
        self.restart_grabber(&mut inner);
        // The preview only runs while there is an input
        if inner.input.is_none() || inner.preview.is_none() {
            self.restart_preview(&mut inner);
        }
        Ok(())
    }

    /// Selected input id
    pub fn input(&self) -> Option<String> {
        self.lock().input.clone()
    }

    /// Size and rate of recordings from the next file on; not while recording
    pub fn set_quality(&self, height: u32, fps: u32) -> Result<()> {
        ensure!(
            RECORD_HEIGHTS.contains(&height),
            "the height must be one of {:?}",
            RECORD_HEIGHTS
        );
        ensure!(fps > 0, "the frame rate must be above zero");
        ensure!(
            lock(&self.recording).is_none(),
            "stop recording before changing the video quality"
        );
        let mut inner = self.lock();
        if (inner.config.record_height, inner.config.record_fps) != (height, fps) {
            inner.config.record_height = height;
            inner.config.record_fps = fps;
            // Recordings pick it up on their own; only a matching preview changes now
            if inner.preview_matches_recording {
                self.restart_preview(&mut inner);
            }
        }
        Ok(())
    }

    /// Recorded height and frame rate
    pub fn quality(&self) -> (u32, u32) {
        let inner = self.lock();
        (
            size_16_9(inner.config.record_height).1,
            inner.config.record_fps,
        )
    }

    /// File extension for recordings
    pub fn extension(&self) -> String {
        self.lock().config.extension.clone()
    }

    /// Start encoding grabbed frames into `path`; false when there is no input
    pub fn start_recording(&self, path: &Path) -> bool {
        let inner = self.lock();
        if inner.input.is_none() {
            return false;
        }
        // Sound only when it is arriving: an input that never sends would stall the file
        let with_audio = inner.record_audio && self.audio.as_ref().is_some_and(Audio::live);
        let args = recording_args(&inner.config, path, with_audio);
        // Room for the encoder to start up without losing frames
        let queue = (inner.config.fps * RECORDING_QUEUE_SECS) as usize;
        drop(inner);
        let audio_pipe = if with_audio { audio_pipe() } else { Ok(None) };
        let spawned = audio_pipe.and_then(|pipe| {
            let (read_end, write_end) = pipe.unzip();
            let (worker, _) = spawn_worker(&args, queue, false, read_end)?;
            Ok((worker, write_end))
        });
        match spawned {
            Ok((worker, write_end)) => {
                log::info!(
                    "Recording video{} to {}",
                    if with_audio { " and sound" } else { "" },
                    path.display()
                );
                // A second of sound queued, like the frames
                let (audio_input, audio_writer) = match write_end {
                    Some(pipe) => {
                        let (input, queued) =
                            sync_channel((1000 / 10 * RECORDING_QUEUE_SECS) as usize);
                        let writer = thread::spawn(move || write_frames(pipe, queued));
                        (Some(input), Some(writer))
                    }
                    None => (None, None),
                };
                *lock(&self.recording) = Some(Recording {
                    path: path.to_path_buf(),
                    worker,
                    first_frame_ms: None,
                    audio_input,
                    audio_writer,
                    audio_start_ms: None,
                    dropped: 0,
                });
                true
            }
            Err(e) => {
                log::error!("Cannot start the video encoder: {:#}", e);
                self.lock().error = Some(format!("{e:#}"));
                false
            }
        }
    }

    /// Finish the file being recorded, and say when its video and sound start
    pub fn stop_recording(&self) -> Option<FileTimes> {
        let mut recording = lock(&self.recording).take()?;
        // End the sound first: ffmpeg finishes only when both inputs have
        if let Some(audio) = &self.audio {
            audio.end();
        }
        drop(recording.audio_input.take());
        if let Some(writer) = recording.audio_writer.take() {
            let _ = writer.join();
        }
        finish_worker(recording.worker, Duration::from_secs(10));
        if recording.dropped > 0 {
            log::warn!(
                "{} video frames dropped in {}",
                recording.dropped,
                recording.path.display()
            );
        }
        Some(FileTimes {
            first_frame_ms: recording.first_frame_ms,
            audio_start_ms: recording.audio_start_ms,
        })
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> VideoStatus {
        let recording = lock(&self.recording)
            .as_ref()
            .map(|r| r.path.display().to_string());
        let inner = self.lock();
        let (preview_height, preview_fps) = preview_quality(&inner);
        let live = unix_ms().saturating_sub(self.last_preview_ms.load(Ordering::Relaxed)) < 2000;
        let encode_us = self.preview_encode_us.load(Ordering::Relaxed);
        VideoStatus {
            input: inner.input.clone(),
            inputs: list_inputs(),
            recording,
            live,
            record_height: size_16_9(inner.config.record_height).1,
            record_fps: inner.config.record_fps,
            preview_height,
            preview_fps,
            preview_encode_ms: (live && encode_us > 0).then(|| encode_us as f64 / 1000.0),
            capture_ms: {
                let fresh =
                    unix_ms().saturating_sub(self.last_frame_ms.load(Ordering::Relaxed)) < 2000;
                let us = self.capture_us.load(Ordering::Relaxed);
                (fresh && us > 0).then(|| us as f64 / 1000.0)
            },
            preview_matches_recording: inner.preview_matches_recording,
            record_audio: inner.record_audio,
            audio_input: Some(inner.config.audio_input.clone()).filter(|s| !s.is_empty()),
            audio_live: self.audio.as_ref().is_some_and(Audio::live),
            error: inner.error.clone(),
        }
    }

    /// Stop the grabber and start one for the current input
    fn restart_grabber(&self, inner: &mut Inner) {
        self.grabber_generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut child) = inner.grabber.take() {
            // The grabber writes no files, so there is nothing to finish
            let _ = child.kill();
            let _ = child.wait();
        }
        inner.error = None;
        self.last_frame_ms.store(0, Ordering::Relaxed);
        lock(&self.capture_times).clear();
        self.capture_us.store(0, Ordering::Relaxed);

        let Some(input) = inner.input.clone() else {
            return;
        };
        match self.spawn_grabber(inner, &input) {
            Ok(child) => {
                inner.grabber = Some(child);
                let video = self.clone();
                let generation = self.grabber_generation.load(Ordering::SeqCst);
                thread::spawn(move || video.watch_first_frame(generation, input));
            }
            Err(e) => {
                log::error!("Cannot start video capture: {:#}", e);
                inner.error = Some(format!("{e:#}"));
            }
        }
    }

    fn spawn_grabber(&self, inner: &Inner, input: &str) -> Result<Child> {
        let args = grabber_args(&inner.config, input);
        log::info!("Starting ffmpeg {}", args.join(" "));
        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            // Own process group: a terminal Ctrl+C reaches the studio, which then stops ffmpeg in order
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run ffmpeg; is it installed?")?;

        let stdout = child.stdout.take().context("no ffmpeg stdout")?;
        let stderr = child.stderr.take().context("no ffmpeg stderr")?;
        // Frames are megabytes; a bigger pipe means fewer wakeups. Best effort.
        // SAFETY: fcntl on a descriptor we own
        unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 22) };
        let generation = self.grabber_generation.load(Ordering::SeqCst);
        let video = self.clone();
        thread::spawn(move || video.read_frames(stdout, generation));
        let video = self.clone();
        thread::spawn(move || video.read_log(stderr, generation));
        Ok(child)
    }

    /// Stop the preview encoder and start one with the current settings
    fn restart_preview(&self, inner: &mut Inner) {
        self.preview_generation.fetch_add(1, Ordering::SeqCst);
        *lock(&self.preview_feed) = None;
        lock(&self.preview_sent).clear();
        self.preview_encode_us.store(0, Ordering::Relaxed);
        if let Some(mut worker) = inner.preview.take() {
            // A preview has nothing worth finishing
            let _ = worker.child.kill();
            finish_worker(worker, Duration::from_secs(2));
        }
        self.last_preview_ms.store(0, Ordering::Relaxed);
        if inner.input.is_none() {
            return;
        }

        let args = preview_args(inner);
        match spawn_worker(&args, PREVIEW_QUEUE, true, None) {
            Ok((worker, Some(stdout))) => {
                *lock(&self.preview_feed) = Some(PreviewFeed {
                    frames: worker.frames.clone(),
                    fps: preview_quality(inner).1,
                    capture_fps: inner.config.fps,
                });
                inner.preview = Some(worker);
                let generation = self.preview_generation.load(Ordering::SeqCst);
                let video = self.clone();
                thread::spawn(move || video.read_preview(stdout, generation));
            }
            Ok((_, None)) => {}
            Err(e) => {
                log::error!("Cannot start the preview encoder: {:#}", e);
                inner.error = Some(format!("{e:#}"));
            }
        }
    }

    /// Reopen the input when the grabber starts but no frame arrives
    ///
    /// The Elgato 4K X only delivers frames on every other stream start, and a
    /// capture card without HDMI signal delivers none; either way ffmpeg waits forever.
    fn watch_first_frame(&self, generation: u64, input: String) {
        thread::sleep(FIRST_FRAME_TIMEOUT);
        if !self.grabber_is_current(generation) || self.last_frame_ms.load(Ordering::Relaxed) != 0 {
            return;
        }
        let mut inner = self.lock();
        if self.grabber_is_current(generation) {
            log::warn!("No frames from {}, reopening it", input);
            self.restart_grabber(&mut inner);
            inner.error = Some(format!(
                "No frames from {input} yet; retrying. Is the console on?"
            ));
        }
    }

    fn grabber_is_current(&self, generation: u64) -> bool {
        self.grabber_generation.load(Ordering::SeqCst) == generation
    }

    fn preview_is_current(&self, generation: u64) -> bool {
        self.preview_generation.load(Ordering::SeqCst) == generation
    }

    /// Hand grabbed frames to the preview and the recording; when the grabber
    /// dies on its own, retry after a pause
    fn read_frames(&self, mut stdout: ChildStdout, generation: u64) {
        let mut frame = vec![0u8; FRAME_BYTES];
        // Counts toward the next frame kept for the preview
        let mut preview_phase = 0;
        // Since when frames have been arriving too long after capture
        let mut backlog_since: Option<Instant> = None;
        // Read to the end even when stale, so ffmpeg can exit
        while stdout.read_exact(&mut frame).is_ok() {
            if !self.grabber_is_current(generation) {
                continue;
            }
            let now = unix_ms();
            self.last_frame_ms.store(now, Ordering::Relaxed);
            let captured_ms = self.capture_time(now);
            let behind_ms = now.saturating_sub(captured_ms);
            let smoothed = match self.capture_us.load(Ordering::Relaxed) {
                0 => behind_ms * 1000,
                before => (before * 15 + behind_ms * 1000) / 16,
            };
            self.capture_us.store(smoothed, Ordering::Relaxed);
            if smoothed as f64 / 1000.0 < BACKLOG_MS || lock(&self.recording).is_some() {
                backlog_since = None;
            } else if backlog_since.get_or_insert_with(Instant::now).elapsed() > BACKLOG_PATIENCE {
                log::warn!(
                    "Frames arrive {:.0} ms after capture; restarting the grabber to clear the queue",
                    smoothed as f64 / 1000.0
                );
                let mut inner = self.lock();
                if self.grabber_is_current(generation) {
                    self.restart_grabber(&mut inner);
                }
                continue;
            }
            // Thin the capture rate to the preview rate
            let preview = lock(&self.preview_feed).clone().filter(|feed| {
                preview_phase += feed.fps;
                let keep = preview_phase >= feed.capture_fps;
                if keep {
                    preview_phase -= feed.capture_fps;
                }
                keep
            });
            let mut recording = lock(&self.recording);
            if preview.is_none() && recording.is_none() {
                continue;
            }
            // Consumers share the frame; the next one gets a fresh buffer
            let shared = Arc::new(core::mem::replace(&mut frame, vec![0u8; FRAME_BYTES]));
            if let Some(preview) = preview {
                // A busy preview skips a frame
                if preview.frames.try_send(Arc::clone(&shared)).is_ok() {
                    let mut sent = lock(&self.preview_sent);
                    // Out of step with the fragments somehow: start counting afresh
                    if sent.len() > 30 {
                        sent.clear();
                    }
                    sent.push_back(Instant::now());
                }
            }
            if let Some(recording) = recording.as_mut() {
                if recording.first_frame_ms.is_none() {
                    // When the card delivered it, not when it got here
                    let now = captured_ms;
                    recording.first_frame_ms = Some(now);
                    // The sound starts with the sample that arrived with this frame
                    if let (Some(audio), Some(input)) = (&self.audio, recording.audio_input.take())
                    {
                        let offset = lock(&self.inner).config.audio_offset_ms;
                        let start = now.saturating_add_signed(offset);
                        recording.audio_start_ms = audio.begin(input, start);
                    }
                }
                match recording.worker.frames.try_send(shared) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => recording.dropped += 1,
                    // The encoder is gone; stop_recording reports it
                    Err(TrySendError::Disconnected(_)) => {}
                }
            }
        }
        if !self.grabber_is_current(generation) {
            return;
        }

        // Give the log reader a moment to record why ffmpeg exited
        thread::sleep(Duration::from_secs(3));
        let mut inner = self.lock();
        if self.grabber_is_current(generation) {
            log::warn!("ffmpeg exited, restarting");
            let error = inner.error.take();
            self.restart_grabber(&mut inner);
            // Keep the reason visible until frames flow again
            inner.error = error.or(Some("ffmpeg exited".to_string()));
        }
    }

    /// Publish the preview stream; when the encoder dies on its own, restart it
    fn read_preview(&self, stdout: ChildStdout, generation: u64) {
        let mut reader = BufReader::new(stdout);
        // `ftyp` or `moof` waiting for the box that completes it
        let mut pending = Vec::new();
        // Read to the end even when stale, so ffmpeg can exit
        while let Some((kind, bytes)) = next_box(&mut reader) {
            let chunk = match &kind {
                b"ftyp" | b"moof" => {
                    pending = bytes;
                    continue;
                }
                b"moov" => ChunkKind::Init,
                b"mdat" if has_keyframe(&bytes[8..]) => ChunkKind::Key,
                b"mdat" => ChunkKind::Delta,
                _ => continue,
            };
            pending.extend_from_slice(&bytes);
            let chunk = PreviewChunk {
                data: Arc::new(core::mem::take(&mut pending)),
                kind: chunk,
            };
            if !self.preview_is_current(generation) {
                continue;
            }
            if chunk.kind == ChunkKind::Init {
                *lock(&self.preview_init) = Some(chunk.clone());
            } else {
                self.last_preview_ms.store(unix_ms(), Ordering::Relaxed);
                if let Some(sent) = lock(&self.preview_sent).pop_front() {
                    let took = sent.elapsed().as_micros() as u64;
                    let smoothed = match self.preview_encode_us.load(Ordering::Relaxed) {
                        0 => took,
                        before => (before * 7 + took) / 8,
                    };
                    self.preview_encode_us.store(smoothed, Ordering::Relaxed);
                }
            }
            // No browser watching is fine
            let _ = self.preview.send(chunk);
        }

        thread::sleep(Duration::from_secs(1));
        let mut inner = self.lock();
        if self.preview_is_current(generation) {
            log::warn!("The preview encoder exited, restarting it");
            self.restart_preview(&mut inner);
        }
    }

    /// Capture time (Unix ms) of the frame just read, `now` if unknown
    ///
    /// The grabber logs each frame's time just before writing the frame, so it
    /// is normally waiting; give the log reader a moment if not.
    fn capture_time(&self, now: u64) -> u64 {
        for _ in 0..20 {
            if let Some(us) = lock(&self.capture_times).pop_front() {
                return us / 1000;
            }
            thread::sleep(Duration::from_millis(1));
        }
        now
    }

    /// Collect each frame's capture time, and keep the grabber's errors for the dashboard
    fn read_log(&self, stderr: impl Read, generation: u64) {
        // Read to the end even when stale, so ffmpeg can exit
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            // showinfo: "... pts_time:1790371234.516667 ..." in Unix seconds
            if let Some(time) = line.split("pts_time:").nth(1) {
                let seconds = time
                    .split_whitespace()
                    .next()
                    .and_then(|t| t.parse::<f64>().ok());
                if let Some(seconds) = seconds
                    && self.grabber_is_current(generation)
                {
                    let mut times = lock(&self.capture_times);
                    // Out of step with the frames somehow: start afresh
                    if times.len() > 120 {
                        times.clear();
                    }
                    times.push_back((seconds * 1e6) as u64);
                }
                continue;
            }
            if self.grabber_is_current(generation)
                && (line.contains("rror") || line.contains("busy"))
            {
                log::warn!("ffmpeg: {}", line);
                self.lock().error = Some(line.trim().to_string());
            }
        }
    }
}

/// Start an ffmpeg that reads raw frames from a queue of `queue` frames;
/// with `piped_stdout`, also return its stdout
fn spawn_worker(
    args: &[String],
    queue: usize,
    piped_stdout: bool,
    fd3: Option<OwnedFd>,
) -> Result<(Worker, Option<ChildStdout>)> {
    log::info!("Starting ffmpeg {}", args.join(" "));
    let mut command = Command::new("ffmpeg");
    if let Some(fd) = &fd3 {
        let raw = fd.as_raw_fd();
        // SAFETY: dup2 is async-signal-safe; the copy at 3 is inherited, as
        // copies do not keep the close-on-exec flag
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(raw, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(if piped_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("cannot run ffmpeg; is it installed?")?;
    // The child has its copy; ours would keep the pipe open
    drop(fd3);
    let stdin = child.stdin.take().context("no ffmpeg stdin")?;
    // SAFETY: fcntl on a descriptor we own; bigger pipes suit megabyte frames
    unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 22) };
    let stdout = child.stdout.take();
    if let Some(stderr) = child.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::warn!("ffmpeg: {}", line);
            }
        });
    }
    let (frames, queued) = sync_channel(queue.max(1));
    let writer = thread::spawn(move || write_frames(stdin, queued));
    Ok((
        Worker {
            child,
            frames,
            writer,
        },
        stdout,
    ))
}

/// A pipe for sound into an encoder: the end ffmpeg reads (as fd 3), and ours
fn audio_pipe() -> Result<Option<(OwnedFd, File)>> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills both descriptors, which we then own
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot make a pipe for sound");
    }
    // SAFETY: fresh descriptors from pipe2, owned by nothing else
    let (read_end, write_end) =
        unsafe { (OwnedFd::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    Ok(Some((read_end, write_end)))
}

/// Feed queued frames to an ffmpeg until the queue closes
fn write_frames(mut stdin: impl Write, queue: Receiver<SharedFrame>) {
    for frame in queue {
        if stdin.write_all(&frame).is_err() {
            return;
        }
    }
}

/// Close a worker's input and wait for it to finish, killing it after `patience`
fn finish_worker(worker: Worker, patience: Duration) {
    let Worker {
        mut child,
        frames,
        writer,
    } = worker;
    // Closing the queue lets the writer finish, which ends ffmpeg's input
    drop(frames);
    let _ = writer.join();
    let deadline = Instant::now() + patience;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    log::warn!("ffmpeg did not finish, killing it");
    let _ = child.kill();
    let _ = child.wait();
}

/// Preview height and frame rate in use
fn preview_quality(inner: &Inner) -> (u32, u32) {
    if inner.preview_matches_recording {
        (
            size_16_9(inner.config.record_height).1,
            inner.config.record_fps,
        )
    } else {
        (
            size_16_9(inner.config.preview_height).1,
            inner.config.preview_fps,
        )
    }
}

/// Input options for raw frames from the grabber, arriving at `fps`
fn raw_input(fps: u32) -> Vec<String> {
    let mut args: Vec<String> = ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size"]
        .map(String::from)
        .to_vec();
    args.push(format!("{}x{}", WORK_SIZE.0, WORK_SIZE.1));
    args.extend(["-framerate".to_string(), fps.to_string()]);
    args.extend(["-i", "pipe:0"].map(String::from));
    args
}

/// Command line for the grabber: `input` as raw 1080p frames on stdout
fn grabber_args(config: &VideoConfig, input: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "info"]
        .map(String::from)
        .to_vec();
    // Frame times: X11 stamps frames with the wall clock itself; for V4L2,
    // the kernel's capture time, converted to Unix time
    if input == SCREEN {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
        args.extend(["-f", "x11grab", "-framerate"].map(String::from));
        args.push(config.fps.to_string());
        args.extend(["-i".to_string(), display]);
    } else {
        args.extend(["-f", "v4l2", "-ts", "mono2abs"].map(String::from));
        args.extend(config.v4l2_args.iter().cloned());
        args.extend(["-framerate".to_string(), config.fps.to_string()]);
        args.extend(["-i".to_string(), input.to_string()]);
    }

    // Fit the source into 16:9, padded if it has another shape, at a constant
    // rate; showinfo logs each frame's capture time (kept absolute by -copyts)
    let (width, height) = WORK_SIZE;
    args.push("-copyts".to_string());
    args.push("-vf".to_string());
    args.push(format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,fps={},format=yuv420p,showinfo=checksum=0",
        config.fps
    ));
    args.extend(["-f", "rawvideo", "pipe:1"].map(String::from));
    args
}

/// Command line for the preview: low-latency H.264 as fragmented MP4 on stdout
fn preview_args(inner: &Inner) -> Vec<String> {
    let config = &inner.config;
    let (height, fps) = preview_quality(inner);
    let (width, height) = size_16_9(height);
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "warning"]
        .map(String::from)
        .to_vec();
    // Frames come already thinned to the preview rate; encode each one. A
    // skipped frame only makes the player fall behind a little, which it catches up
    args.extend(raw_input(fps));
    args.push("-vf".to_string());
    args.push(format!("scale={width}:{height}"));
    args.extend(["-fps_mode", "passthrough"].map(String::from));
    args.extend(config.preview_encoder.iter().cloned());
    // A keyframe every second, so a browser can join quickly, and one frame
    // per MP4 fragment for low latency
    args.extend([
        "-bf".to_string(),
        "0".to_string(),
        "-g".to_string(),
        fps.to_string(),
    ]);
    args.extend(
        [
            "-f",
            "mp4",
            "-movflags",
            "empty_moov+default_base_moof+frag_every_frame",
            "-flush_packets",
            "1",
            "pipe:1",
        ]
        .map(String::from),
    );
    args
}

/// Command line for recording raw frames into `path` at the recording size and rate
fn recording_args(config: &VideoConfig, path: &Path, with_audio: bool) -> Vec<String> {
    let (width, height) = size_16_9(config.record_height);
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "warning"]
        .map(String::from)
        .to_vec();
    args.extend(raw_input(config.fps));
    if with_audio {
        // Raw sound on fd 3, starting with the first frame
        args.extend(["-thread_queue_size", "256", "-f", "s16le", "-ar"].map(String::from));
        args.push(audio::SAMPLE_RATE.to_string());
        args.extend(["-ch_layout", "stereo"].map(String::from));
        args.extend(["-i", "pipe:3", "-map", "0:v", "-map", "1:a"].map(String::from));
        args.extend(["-c:a", "libopus", "-b:a", "128k"].map(String::from));
    }
    args.push("-vf".to_string());
    args.push(format!("scale={width}:{height},fps={}", config.record_fps));
    args.extend(config.encoder.iter().cloned());
    // A keyframe every second, so a training clip can start anywhere without
    // decoding seconds of frames it does not use
    args.extend(["-g".to_string(), config.record_fps.to_string()]);
    if matches!(config.extension.as_str(), "mkv" | "webm") {
        // Write a cluster every second: steady file growth, little lost on a crash
        args.extend(["-cluster_time_limit", "1000"].map(String::from));
    }
    // Hand every packet to the file right away, so its size shows the real rate
    args.extend(["-flush_packets", "1"].map(String::from));
    args.extend(["-y".to_string(), path.display().to_string()]);
    args
}

/// Read one top-level MP4 box, header included: its type and bytes
fn next_box(reader: &mut impl Read) -> Option<([u8; 4], Vec<u8>)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let size = u32::from_be_bytes(header[..4].try_into().ok()?) as usize;
    // Fragments never need 64-bit sizes; anything else means the stream is broken
    if size < 8 {
        return None;
    }
    let mut bytes = vec![0; size];
    bytes[..8].copy_from_slice(&header);
    reader.read_exact(&mut bytes[8..]).ok()?;
    Some((header[4..].try_into().ok()?, bytes))
}

/// Whether an `mdat` payload of length-prefixed H.264 units holds a keyframe (IDR)
fn has_keyframe(mut payload: &[u8]) -> bool {
    while payload.len() > 4 {
        let length = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload[4] & 0x1f == 5 {
            return true;
        }
        payload = payload.get(4 + length..).unwrap_or_default();
    }
    false
}
