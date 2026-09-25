//! Video capture with ffmpeg: a live preview, plus recording to files
//!
//! A capture ffmpeg owns the input for as long as it is selected: a capture
//! card can only be opened once, and reopening it is slow (the Elgato 4K X
//! only streams on every other start). It writes two streams:
//!
//! - raw frames, already scaled to the recording size and rate, on fd 3
//! - a low-latency H.264 preview on stdout, as fragmented MP4 with one frame
//!   per fragment, which the dashboard plays with a `<video>` element
//!
//! Recording starts a separate encoder ffmpeg and feeds it those raw frames,
//! so a file begins with the first frame after Record, and pausing never
//! touches the device. Frames come at a constant rate: frame `n` of a file was
//! captured `n / fps` seconds after its first frame, whose Unix time is kept.

use crate::config::VideoConfig;
use crate::dump::unix_ms;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
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

/// How long a new ffmpeg may take to deliver its first frame
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

/// Seconds of frames queued for an encoder that is still starting up
const ENCODER_QUEUE_SECS: u32 = 3;

/// Recording heights the dashboard offers; widths follow 16:9
pub const RECORD_HEIGHTS: [u32; 4] = [1080, 720, 540, 360];

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

/// Width and height of recorded frames
fn record_size(config: &VideoConfig) -> (u32, u32) {
    size_16_9(config.record_height)
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

/// A file being encoded from the capture's raw frames
struct Recording {
    path: PathBuf,
    /// Frames for the encoder; dropping it ends the file
    frames: SyncSender<Vec<u8>>,
    /// Unix ms of the file's first frame, once it arrived
    first_frame_ms: Option<u64>,
    /// Frames lost because the encoder fell behind
    dropped: u64,
    encoder: Child,
    writer: JoinHandle<()>,
}

struct Inner {
    config: VideoConfig,
    input: Option<String>,
    /// The preview uses the recording size and rate instead of its own
    preview_matches_recording: bool,
    /// The capture ffmpeg
    child: Option<Child>,
    error: Option<String>,
}

/// Handle to the capture and recording processes; clones share them
#[derive(Clone)]
pub struct Video {
    inner: Arc<Mutex<Inner>>,
    /// The file being recorded; its own lock, as the frame reader takes it for every frame
    recording: Arc<Mutex<Option<Recording>>>,
    /// Preview stream for the browsers
    preview: broadcast::Sender<PreviewChunk>,
    /// The current init segment, for browsers that join later
    preview_init: Arc<Mutex<Option<PreviewChunk>>>,
    /// Bumped before every restart so threads of an old ffmpeg know they are stale.
    /// Kept outside the lock: an old ffmpeg's pipes must be drained without it,
    /// or it blocks on a full pipe and never exits.
    generation: Arc<AtomicU64>,
    /// Unix ms of the latest preview frame
    last_preview_ms: Arc<AtomicU64>,
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
                child: None,
                error: None,
            })),
            recording: Arc::default(),
            // A few seconds of frames; a browser further behind catches up at a keyframe
            preview: broadcast::channel(256).0,
            preview_init: Arc::default(),
            generation: Arc::default(),
            last_preview_ms: Arc::default(),
        };
        video.restart(&mut video.lock());
        video
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recording(&self) -> MutexGuard<'_, Option<Recording>> {
        self.recording.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Receive the preview: the current init segment, then the live stream
    pub fn subscribe(&self) -> (Option<PreviewChunk>, broadcast::Receiver<PreviewChunk>) {
        let init = self.preview_init.lock().unwrap_or_else(|e| e.into_inner());
        (init.clone(), self.preview.subscribe())
    }

    /// Make the preview follow the recording size and rate, or use its own; not while recording
    pub fn set_preview_matches_recording(&self, matches: bool) -> Result<()> {
        ensure!(
            self.recording().is_none(),
            "stop recording before changing the preview"
        );
        let mut inner = self.lock();
        if inner.preview_matches_recording != matches {
            inner.preview_matches_recording = matches;
            self.restart(&mut inner);
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
            self.recording().is_none(),
            "stop recording before changing the video input"
        );
        let mut inner = self.lock();
        inner.input = input;
        self.restart(&mut inner);
        Ok(())
    }

    /// Selected input id
    pub fn input(&self) -> Option<String> {
        self.lock().input.clone()
    }

    /// Size and rate of recordings; not while recording
    pub fn set_quality(&self, height: u32, fps: u32) -> Result<()> {
        ensure!(
            RECORD_HEIGHTS.contains(&height),
            "the height must be one of {:?}",
            RECORD_HEIGHTS
        );
        ensure!(fps > 0, "the frame rate must be above zero");
        ensure!(
            self.recording().is_none(),
            "stop recording before changing the video quality"
        );
        let mut inner = self.lock();
        if (inner.config.record_height, inner.config.record_fps) != (height, fps) {
            inner.config.record_height = height;
            inner.config.record_fps = fps;
            // The capture scales frames for recording, so it restarts with the new size
            self.restart(&mut inner);
        }
        Ok(())
    }

    /// Recorded height and frame rate
    pub fn quality(&self) -> (u32, u32) {
        let inner = self.lock();
        (record_size(&inner.config).1, inner.config.record_fps)
    }

    /// File extension for recordings
    pub fn extension(&self) -> String {
        self.lock().config.extension.clone()
    }

    /// Start encoding captured frames into `path`; false when there is no input
    pub fn start_recording(&self, path: &Path) -> bool {
        let inner = self.lock();
        if inner.input.is_none() {
            return false;
        }
        match spawn_encoder(&inner.config, path) {
            Ok(recording) => {
                log::info!("Recording video to {}", path.display());
                *self.recording() = Some(recording);
                true
            }
            Err(e) => {
                log::error!("Cannot start the video encoder: {:#}", e);
                drop(inner);
                self.lock().error = Some(format!("{e:#}"));
                false
            }
        }
    }

    /// Finish the file being recorded
    ///
    /// Returns the Unix ms of its first frame, when one arrived.
    pub fn stop_recording(&self) -> Option<u64> {
        let recording = self.recording().take()?;
        let Recording {
            path,
            frames,
            first_frame_ms,
            dropped,
            mut encoder,
            writer,
        } = recording;
        // Closing the queue lets the writer finish, which ends the encoder's input
        drop(frames);
        let _ = writer.join();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = encoder.try_wait() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if let Ok(None) = encoder.try_wait() {
            log::warn!("The video encoder did not finish, killing it");
            let _ = encoder.kill();
            let _ = encoder.wait();
        }
        if dropped > 0 {
            log::warn!("{} video frames dropped in {}", dropped, path.display());
        }
        first_frame_ms
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> VideoStatus {
        let recording = self
            .recording()
            .as_ref()
            .map(|r| r.path.display().to_string());
        let inner = self.lock();
        VideoStatus {
            input: inner.input.clone(),
            inputs: list_inputs(),
            recording,
            live: unix_ms().saturating_sub(self.last_preview_ms.load(Ordering::Relaxed)) < 2000,
            record_height: record_size(&inner.config).1,
            record_fps: inner.config.record_fps,
            preview_height: preview_size(&inner).1,
            preview_fps: preview_fps(&inner),
            preview_matches_recording: inner.preview_matches_recording,
            error: inner.error.clone(),
        }
    }

    /// Stop the capture ffmpeg and start one for the current input
    fn restart(&self, inner: &mut Inner) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(mut child) = inner.child.take() {
            // The capture writes no files, so there is nothing to finish
            let _ = child.kill();
            let _ = child.wait();
        }
        inner.error = None;
        self.last_preview_ms.store(0, Ordering::Relaxed);

        let Some(input) = inner.input.clone() else {
            return;
        };
        match self.spawn(inner, &input) {
            Ok(child) => {
                inner.child = Some(child);
                let video = self.clone();
                let generation = self.generation.load(Ordering::SeqCst);
                thread::spawn(move || video.watch_first_frame(generation, input));
            }
            Err(e) => {
                log::error!("Cannot start video capture: {:#}", e);
                inner.error = Some(format!("{e:#}"));
            }
        }
    }

    fn spawn(&self, inner: &Inner, input: &str) -> Result<Child> {
        // A pipe for the raw frames, handed to ffmpeg as fd 3
        let mut fds = [0; 2];
        // SAFETY: pipe2 fills both descriptors, which we then own
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            bail!("cannot create a pipe: {}", std::io::Error::last_os_error());
        }
        let (read_end, write_end) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // A bigger pipe means fewer wakeups for large frames; best effort
        unsafe { libc::fcntl(write_end.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 20) };

        let args = capture_args(inner, input);
        log::info!("Starting ffmpeg {}", args.join(" "));
        let mut command = Command::new("ffmpeg");
        command
            .args(&args)
            .stdin(Stdio::null())
            // Own process group: a terminal Ctrl+C reaches the studio, which then stops ffmpeg in order
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let raw_fd = write_end.as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec
        unsafe {
            command.pre_exec(move || {
                if raw_fd == 3 {
                    // Already fd 3: just let it survive exec
                    libc::fcntl(3, libc::F_SETFD, 0);
                } else if libc::dup2(raw_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("cannot run ffmpeg; is it installed?")?;
        // Only ffmpeg writes now, so the reader sees the end when it exits
        drop(write_end);

        let generation = self.generation.load(Ordering::SeqCst);
        let stdout = child.stdout.take().context("no ffmpeg stdout")?;
        let stderr = child.stderr.take().context("no ffmpeg stderr")?;
        let (width, height) = record_size(&inner.config);
        let frame_size = (width * height * 3 / 2) as usize;
        let video = self.clone();
        thread::spawn(move || video.read_preview(stdout, generation));
        let video = self.clone();
        thread::spawn(move || video.read_log(stderr, generation));
        let video = self.clone();
        let pipe = File::from(read_end);
        thread::spawn(move || video.read_frames(pipe, frame_size, generation));
        Ok(child)
    }

    /// Reopen the input when ffmpeg starts but no frame arrives
    ///
    /// The Elgato 4K X only delivers frames on every other stream start, and a
    /// capture card without HDMI signal delivers none; either way ffmpeg waits forever.
    fn watch_first_frame(&self, generation: u64, input: String) {
        thread::sleep(FIRST_FRAME_TIMEOUT);
        if !self.is_current(generation) || self.last_preview_ms.load(Ordering::Relaxed) != 0 {
            return;
        }
        let mut inner = self.lock();
        if self.is_current(generation) {
            log::warn!("No frames from {}, reopening it", input);
            self.restart(&mut inner);
            inner.error = Some(format!(
                "No frames from {input} yet; retrying. Is the console on?"
            ));
        }
    }

    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// Hand raw frames to the recording, if any; otherwise drop them
    fn read_frames(&self, mut pipe: File, frame_size: usize, generation: u64) {
        let mut frame = vec![0u8; frame_size];
        // Read to the end even when stale, so ffmpeg can exit
        while pipe.read_exact(&mut frame).is_ok() {
            if !self.is_current(generation) {
                continue;
            }
            let mut recording = self.recording();
            let Some(recording) = recording.as_mut() else {
                continue;
            };
            recording.first_frame_ms.get_or_insert_with(unix_ms);
            match recording.frames.try_send(frame.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => recording.dropped += 1,
                // The encoder is gone; stop_recording reports it
                Err(TrySendError::Disconnected(_)) => {}
            }
        }
    }

    /// Publish the preview stream; when ffmpeg dies on its own, retry after a pause
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
            if !self.is_current(generation) {
                continue;
            }
            if chunk.kind == ChunkKind::Init {
                *self.preview_init.lock().unwrap_or_else(|e| e.into_inner()) = Some(chunk.clone());
            } else {
                self.last_preview_ms.store(unix_ms(), Ordering::Relaxed);
            }
            // No browser watching is fine
            let _ = self.preview.send(chunk);
        }
        if !self.is_current(generation) {
            return;
        }

        // Give the log reader a moment to record why ffmpeg exited
        thread::sleep(Duration::from_secs(3));
        let mut inner = self.lock();
        if self.is_current(generation) {
            log::warn!("ffmpeg exited, restarting");
            let error = inner.error.take();
            self.restart(&mut inner);
            // Keep the reason visible until frames flow again
            inner.error = error.or(Some("ffmpeg exited".to_string()));
        }
    }

    /// Keep the capture's errors for the dashboard
    fn read_log(&self, stderr: impl Read, generation: u64) {
        // Read to the end even when stale, so ffmpeg can exit
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if self.is_current(generation) && (line.contains("rror") || line.contains("busy")) {
                log::warn!("ffmpeg: {}", line);
                self.lock().error = Some(line.trim().to_string());
            }
        }
    }
}

/// Start an encoder ffmpeg for `path`, fed raw frames through a queue
fn spawn_encoder(config: &VideoConfig, path: &Path) -> Result<Recording> {
    let (width, height) = record_size(config);
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "warning"]
        .map(String::from)
        .to_vec();
    args.extend(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size"].map(String::from));
    args.push(format!("{width}x{height}"));
    args.extend(["-framerate".to_string(), config.record_fps.to_string()]);
    args.extend(["-i", "pipe:0"].map(String::from));
    args.extend(config.encoder.iter().cloned());
    if matches!(config.extension.as_str(), "mkv" | "webm") {
        // Write a cluster every second: steady file growth, little lost on a crash
        args.extend(["-cluster_time_limit", "1000"].map(String::from));
    }
    // Hand every packet to the file right away, so its size shows the real rate
    args.extend(["-flush_packets", "1"].map(String::from));
    args.extend(["-y".to_string(), path.display().to_string()]);

    log::info!("Starting ffmpeg {}", args.join(" "));
    let mut encoder = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("cannot run ffmpeg; is it installed?")?;
    let stdin = encoder.stdin.take().context("no encoder stdin")?;
    if let Some(stderr) = encoder.stderr.take() {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::warn!("ffmpeg encoder: {}", line);
            }
        });
    }

    // Room for the encoder to start up without losing frames
    let (frames, queue) =
        sync_channel(config.record_fps.max(1) as usize * ENCODER_QUEUE_SECS as usize);
    let writer = thread::spawn(move || write_frames(stdin, queue));
    Ok(Recording {
        path: path.to_path_buf(),
        frames,
        first_frame_ms: None,
        dropped: 0,
        encoder,
        writer,
    })
}

/// Feed queued frames to the encoder until the queue closes
fn write_frames(mut stdin: impl Write, queue: Receiver<Vec<u8>>) {
    for frame in queue {
        if let Err(e) = stdin.write_all(&frame) {
            log::error!("Video encoder stopped taking frames: {}", e);
            return;
        }
    }
}

/// Width and height of the preview
fn preview_size(inner: &Inner) -> (u32, u32) {
    if inner.preview_matches_recording {
        record_size(&inner.config)
    } else {
        size_16_9(inner.config.preview_height)
    }
}

/// Frame rate of the preview
fn preview_fps(inner: &Inner) -> u32 {
    if inner.preview_matches_recording {
        inner.config.record_fps
    } else {
        inner.config.preview_fps
    }
}

/// Command line for capturing `input`: preview on stdout, raw recording frames on fd 3
fn capture_args(inner: &Inner, input: &str) -> Vec<String> {
    let config = &inner.config;
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

    // Both outputs fit the source into 16:9, padded if it has another shape,
    // at a constant rate
    let fit = |(width, height): (u32, u32), fps: u32| {
        format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,fps={fps},format=yuv420p"
        )
    };
    let preview_fps = preview_fps(inner);
    args.push("-filter_complex".to_string());
    args.push(format!(
        "[0:v]split=2[a][b];[a]{}[r];[b]{}[p]",
        fit(record_size(config), config.record_fps),
        fit(preview_size(inner), preview_fps),
    ));
    args.extend(["-map", "[r]", "-f", "rawvideo", "pipe:3"].map(String::from));

    // Preview: H.264 with a keyframe every second, so a browser can join
    // quickly, and one frame per MP4 fragment for low latency
    args.extend(["-map", "[p]"].map(String::from));
    args.extend(config.preview_encoder.iter().cloned());
    args.extend([
        "-bf".to_string(),
        "0".to_string(),
        "-g".to_string(),
        preview_fps.to_string(),
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
