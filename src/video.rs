//! Video capture with ffmpeg: a live preview, plus recording to files
//!
//! One ffmpeg process owns the input, since a capture card can only be opened
//! once. It always writes a small MJPEG preview to stdout; while recording it
//! also encodes the same frames into a file. Starting or stopping a recording
//! restarts ffmpeg, so the preview blinks for a moment.
//!
//! Input timestamps are wall-clock (`-use_wallclock_as_timestamps`), and ffmpeg
//! reports the first one as `start:` in its log. That Unix time plus a frame's
//! timestamp in the file gives the frame's wall-clock time.

use crate::config::VideoConfig;
use crate::dump::unix_ms;
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
use serde::Serialize;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Instant;
use tokio::sync::watch;

/// How long a new ffmpeg may take to deliver its first frame
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

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

/// Snapshot of the video capture for the dashboard
#[derive(Debug, Serialize)]
pub struct VideoStatus {
    /// Selected input id, if any
    pub input: Option<String>,
    pub inputs: Vec<VideoInput>,
    /// File being recorded
    pub recording: Option<String>,
    /// Size of that file so far
    pub bytes: u64,
    /// Preview frames arrived within the last two seconds
    pub live: bool,
    /// Why ffmpeg is not running
    pub error: Option<String>,
}

struct Inner {
    config: VideoConfig,
    input: Option<String>,
    /// File the running ffmpeg records into
    recording: Option<PathBuf>,
    child: Option<Child>,
    /// Unix ms of the first frame of the running ffmpeg, from its log
    started_at_ms: Option<u64>,
    error: Option<String>,
}

/// Handle to the capture process; clones share it
#[derive(Clone)]
pub struct Video {
    inner: Arc<Mutex<Inner>>,
    /// Latest preview JPEG
    preview: watch::Sender<Arc<Vec<u8>>>,
    /// Bumped before every restart so threads of an old ffmpeg know they are stale.
    /// Kept outside the lock: an old ffmpeg's pipes must be drained without it,
    /// or it blocks on a full pipe and never finishes its files.
    generation: Arc<AtomicU64>,
    /// Unix ms of the latest preview frame
    last_preview_ms: Arc<AtomicU64>,
}

impl Video {
    /// Start previewing `input` (if any)
    pub fn new(config: VideoConfig, input: Option<String>) -> Self {
        let video = Self {
            inner: Arc::new(Mutex::new(Inner {
                config,
                input,
                recording: None,
                child: None,
                started_at_ms: None,
                error: None,
            })),
            preview: watch::Sender::default(),
            generation: Arc::default(),
            last_preview_ms: Arc::default(),
        };
        video.restart(&mut video.lock());
        video
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Receive preview JPEGs
    pub fn subscribe(&self) -> watch::Receiver<Arc<Vec<u8>>> {
        self.preview.subscribe()
    }

    /// Switch to another input, or none; not while recording
    pub fn set_input(&self, input: Option<String>) -> Result<()> {
        let mut inner = self.lock();
        ensure!(
            inner.recording.is_none(),
            "stop recording before changing the video input"
        );
        inner.input = input;
        self.restart(&mut inner);
        Ok(())
    }

    /// Selected input id
    pub fn input(&self) -> Option<String> {
        self.lock().input.clone()
    }

    /// File extension for recordings
    pub fn extension(&self) -> String {
        self.lock().config.extension.clone()
    }

    /// Start recording into `path`, restarting ffmpeg; false when there is no input
    pub fn start_recording(&self, path: &Path) -> bool {
        let mut inner = self.lock();
        if inner.input.is_none() {
            return false;
        }
        inner.recording = Some(path.to_path_buf());
        self.restart(&mut inner);
        true
    }

    /// Finish the recording and go back to preview only
    ///
    /// Returns the Unix ms of the recording's first frame when ffmpeg reported it.
    pub fn stop_recording(&self) -> Option<u64> {
        let mut inner = self.lock();
        inner.recording.as_ref()?;
        let started_at = inner.started_at_ms;
        inner.recording = None;
        self.restart(&mut inner);
        started_at
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> VideoStatus {
        let inner = self.lock();
        let recording = inner.recording.as_ref();
        VideoStatus {
            input: inner.input.clone(),
            inputs: list_inputs(),
            recording: recording.map(|p| p.display().to_string()),
            bytes: recording
                .and_then(|p| std::fs::metadata(p).ok())
                .map_or(0, |m| m.len()),
            live: unix_ms().saturating_sub(self.last_preview_ms.load(Ordering::Relaxed)) < 2000,
            error: inner.error.clone(),
        }
    }

    /// Stop the running ffmpeg and start one for the current input and recording
    fn restart(&self, inner: &mut Inner) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        if let Some(child) = inner.child.take() {
            // An ffmpeg that never produced a frame has nothing to save
            let graceful = self.last_preview_ms.load(Ordering::Relaxed) != 0;
            stop_child(child, graceful);
        }
        inner.started_at_ms = None;
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
        let args = ffmpeg_args(&inner.config, input, inner.recording.as_deref());
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

        let generation = self.generation.load(Ordering::SeqCst);
        let stdout = child.stdout.take().context("no ffmpeg stdout")?;
        let stderr = child.stderr.take().context("no ffmpeg stderr")?;
        let video = self.clone();
        thread::spawn(move || video.read_preview(stdout, generation));
        let video = self.clone();
        thread::spawn(move || video.read_log(stderr, generation));
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

    /// Publish preview frames; when ffmpeg dies on its own, retry after a pause
    fn read_preview(&self, stdout: ChildStdout, generation: u64) {
        let mut reader = BufReader::new(stdout);
        // Read to the end even when stale, so ffmpeg can finish
        while let Some(jpeg) = next_part(&mut reader) {
            if self.is_current(generation) {
                self.preview.send_replace(Arc::new(jpeg));
                self.last_preview_ms.store(unix_ms(), Ordering::Relaxed);
            }
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

    /// Pick the start time and errors out of the ffmpeg log
    fn read_log(&self, stderr: impl Read, generation: u64) {
        // Read to the end even when stale, so ffmpeg can finish
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if !self.is_current(generation) {
                continue;
            }
            let mut inner = self.lock();
            // "  Duration: N/A, start: 1790310939.087875, bitrate: ..."
            if inner.started_at_ms.is_none()
                && let Some(start) = line.split("start: ").nth(1)
                && let Ok(secs) = start
                    .split(',')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .parse::<f64>()
            {
                inner.started_at_ms = Some((secs * 1000.0) as u64);
            } else if line.contains("rror") || line.contains("busy") {
                log::warn!("ffmpeg: {}", line);
                inner.error = Some(line.trim().to_string());
            }
        }
    }
}

/// Ask ffmpeg to finish its files, then make sure it is gone
///
/// Without `graceful` it is killed right away.
fn stop_child(mut child: Child, graceful: bool) {
    if graceful {
        // SIGINT makes ffmpeg flush and close its outputs, like Ctrl+C in a terminal
        // SAFETY: plain signal to a child we own
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        log::warn!("ffmpeg did not quit, killing it");
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Command line for capturing `input`, previewing to stdout and optionally recording
fn ffmpeg_args(config: &VideoConfig, input: &str, recording: Option<&Path>) -> Vec<String> {
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

    let preview = format!(
        "scale=-2:{},fps={}",
        config.preview_height, config.preview_fps
    );
    match recording {
        Some(path) => {
            args.push("-filter_complex".to_string());
            args.push(format!(
                "[0:v]split=2[rec][pv];[rec]{}[r];[pv]{preview}[p]",
                config.record_filter
            ));
            args.extend(["-map", "[r]"].map(String::from));
            args.extend(config.encoder.iter().cloned());
            args.extend(["-y".to_string(), path.display().to_string()]);
            args.extend(["-map", "[p]"].map(String::from));
        }
        None => args.extend(["-vf".to_string(), preview]),
    }
    args.extend(["-c:v", "mjpeg", "-q:v", "7", "-f", "mpjpeg", "pipe:1"].map(String::from));
    args
}

/// Read one JPEG from an `mpjpeg` stream: header lines, a blank line, then the body
fn next_part(reader: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim();
        if let Some(value) = trimmed.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse::<usize>().ok();
        } else if trimmed.is_empty() && length.is_some() {
            break;
        }
    }
    let mut jpeg = vec![0; length?];
    reader.read_exact(&mut jpeg).ok()?;
    Some(jpeg)
}
