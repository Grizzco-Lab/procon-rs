//! Video capture with ffmpeg: a live preview, plus recording to files
//!
//! Three kinds of ffmpeg share the work, joined by the studio:
//!
//! - The grabber owns the input for as long as it is selected and turns it
//!   into raw 1080p frames at the capture rate. Reopening a capture card is
//!   slow (the Elgato 4K X only streams on every other start), so nothing but
//!   choosing another input restarts it.
//! - The preview encoder scales those frames to the preview size and rate and
//!   encodes low-latency H.264 as fragmented MP4, one frame per fragment,
//!   which the dashboard plays with a `<video>` element. Changing the preview
//!   restarts only this process.
//! - A recording encoder per video file, fed the same frames from Record on,
//!   so a file begins with the next frame and pausing never touches the input.
//!
//! Frames reach recordings at a constant rate: frame `n` of a file was captured
//! `n / fps` seconds after its first frame, whose Unix time is kept.

use crate::config::VideoConfig;
use crate::dump::unix_ms;
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
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
    /// The preview follows the recording size and rate
    pub preview_matches_recording: bool,
    /// Why ffmpeg is not running
    pub error: Option<String>,
}

/// A raw frame shared by the preview and the recording
type SharedFrame = Arc<Vec<u8>>;

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
    /// Frames lost because the encoder fell behind
    dropped: u64,
}

struct Inner {
    config: VideoConfig,
    input: Option<String>,
    /// The preview uses the recording size and rate instead of its own
    preview_matches_recording: bool,
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
    preview_feed: Arc<Mutex<Option<SyncSender<SharedFrame>>>>,
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
}

/// Lock a mutex even if a holder panicked; the data stays usable
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Video {
    /// Start capturing `input` (if any)
    pub fn new(
        config: VideoConfig,
        input: Option<String>,
        preview_matches_recording: bool,
    ) -> Self {
        let video = Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                input,
                preview_matches_recording,
                grabber: None,
                preview: None,
                error: None,
            })),
            preview_feed: Arc::default(),
            recording: Arc::default(),
            // A few seconds of frames; a browser further behind catches up at a keyframe
            preview: broadcast::channel(256).0,
            preview_init: Arc::default(),
            grabber_generation: Arc::default(),
            preview_generation: Arc::default(),
            last_frame_ms: Arc::default(),
            last_preview_ms: Arc::default(),
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
        let args = recording_args(&inner.config, path);
        // Room for the encoder to start up without losing frames
        let queue = (inner.config.fps * RECORDING_QUEUE_SECS) as usize;
        drop(inner);
        match spawn_worker(&args, queue, false) {
            Ok((worker, _)) => {
                log::info!("Recording video to {}", path.display());
                *lock(&self.recording) = Some(Recording {
                    path: path.to_path_buf(),
                    worker,
                    first_frame_ms: None,
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

    /// Finish the file being recorded
    ///
    /// Returns the Unix ms of its first frame, when one arrived.
    pub fn stop_recording(&self) -> Option<u64> {
        let recording = lock(&self.recording).take()?;
        finish_worker(recording.worker, Duration::from_secs(10));
        if recording.dropped > 0 {
            log::warn!(
                "{} video frames dropped in {}",
                recording.dropped,
                recording.path.display()
            );
        }
        recording.first_frame_ms
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> VideoStatus {
        let recording = lock(&self.recording)
            .as_ref()
            .map(|r| r.path.display().to_string());
        let inner = self.lock();
        let (preview_height, preview_fps) = preview_quality(&inner);
        VideoStatus {
            input: inner.input.clone(),
            inputs: list_inputs(),
            recording,
            live: unix_ms().saturating_sub(self.last_preview_ms.load(Ordering::Relaxed)) < 2000,
            record_height: size_16_9(inner.config.record_height).1,
            record_fps: inner.config.record_fps,
            preview_height,
            preview_fps,
            preview_matches_recording: inner.preview_matches_recording,
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
        match spawn_worker(&args, PREVIEW_QUEUE, true) {
            Ok((worker, Some(stdout))) => {
                *lock(&self.preview_feed) = Some(worker.frames.clone());
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
        // Read to the end even when stale, so ffmpeg can exit
        while stdout.read_exact(&mut frame).is_ok() {
            if !self.grabber_is_current(generation) {
                continue;
            }
            self.last_frame_ms.store(unix_ms(), Ordering::Relaxed);
            let preview = lock(&self.preview_feed).clone();
            let mut recording = lock(&self.recording);
            if preview.is_none() && recording.is_none() {
                continue;
            }
            // Consumers share the frame; the next one gets a fresh buffer
            let shared = Arc::new(core::mem::replace(&mut frame, vec![0u8; FRAME_BYTES]));
            if let Some(preview) = preview {
                // A busy preview skips a frame
                let _ = preview.try_send(Arc::clone(&shared));
            }
            if let Some(recording) = recording.as_mut() {
                recording.first_frame_ms.get_or_insert_with(unix_ms);
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

    /// Keep the grabber's errors for the dashboard
    fn read_log(&self, stderr: impl Read, generation: u64) {
        // Read to the end even when stale, so ffmpeg can exit
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
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
) -> Result<(Worker, Option<ChildStdout>)> {
    log::info!("Starting ffmpeg {}", args.join(" "));
    let mut child = Command::new("ffmpeg")
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

/// Input options for raw frames from the grabber, arriving at the capture rate
fn raw_input(config: &VideoConfig) -> Vec<String> {
    let mut args: Vec<String> = ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size"]
        .map(String::from)
        .to_vec();
    args.push(format!("{}x{}", WORK_SIZE.0, WORK_SIZE.1));
    args.extend(["-framerate".to_string(), config.fps.to_string()]);
    args.extend(["-i", "pipe:0"].map(String::from));
    args
}

/// Command line for the grabber: `input` as raw 1080p frames on stdout
fn grabber_args(config: &VideoConfig, input: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "info"]
        .map(String::from)
        .to_vec();
    args.extend(["-use_wallclock_as_timestamps", "1"].map(String::from));

    if input == SCREEN {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
        args.extend(["-f", "x11grab", "-framerate"].map(String::from));
        args.push(config.fps.to_string());
        args.extend(["-i".to_string(), display]);
    } else {
        args.extend(["-f", "v4l2"].map(String::from));
        args.extend(config.v4l2_args.iter().cloned());
        args.extend(["-framerate".to_string(), config.fps.to_string()]);
        args.extend(["-i".to_string(), input.to_string()]);
    }

    // Fit the source into 16:9, padded if it has another shape, at a constant rate
    let (width, height) = WORK_SIZE;
    args.push("-vf".to_string());
    args.push(format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,fps={},format=yuv420p",
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
    // Stamp frames as they arrive, so a skipped frame never slows the clock
    args.extend(["-use_wallclock_as_timestamps", "1"].map(String::from));
    args.extend(raw_input(config));
    args.push("-vf".to_string());
    args.push(format!("scale={width}:{height},fps={fps}"));
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
fn recording_args(config: &VideoConfig, path: &Path) -> Vec<String> {
    let (width, height) = size_16_9(config.record_height);
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "warning"]
        .map(String::from)
        .to_vec();
    args.extend(raw_input(config));
    args.push("-vf".to_string());
    args.push(format!("scale={width}:{height},fps={}", config.record_fps));
    args.extend(config.encoder.iter().cloned());
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
