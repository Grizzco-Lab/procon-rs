//! Video capture with ffmpeg: a live preview, plus recording to files
//!
//! Three kinds of ffmpeg share the work, joined by the lab:
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
//! A V4L2 capture card in YUYV (the Elgato 4K X's uncompressed mode) is read
//! by the lab itself instead ([`crate::studio::v4l2`]): memory-mapped, a few
//! buffers, each frame taken as soon as the kernel has it, with the kernel's
//! timestamp. The reader puts the frames on the constant rate ([`ConstantRate`],
//! the fps filter's job otherwise) and writes them into a converter ffmpeg,
//! whose raw 1080p frames go on as the grabber's would. ffmpeg's own v4l2
//! input asks for 256 buffers, so frames it falls behind on queue in the
//! kernel for good, and it passes each through four threads before a pipe.
//! When reading the device ourselves fails ([`DIRECT_TRIES`] starts without
//! a frame), or its format is another, ffmpeg reads it (`v4l2_direct`).
//!
//! Our reader counts what the card delivers ([`CaptureCounts`]): frames,
//! the corrupted or short ones it skips, and the ones the driver dropped
//! (gaps in its sequence numbers); either way the frame before stands in
//! for them on the constant rate, so no recording gets a broken frame or
//! loses its timing. The status has the counts since the reader started
//! and since the file being recorded started, each file keeps its own
//! ([`FileTimes::capture`]), and the log has the first loss and, while
//! losses go on, at most a line a [`LOSS_LOG_EVERY`] with their rate.
//!
//! Either way the Predictor's live policy gets its own frames ([`PolicySink`]):
//! YUYV, the newest ready, [`POLICY_FPS`] of them a second, before the
//! constant rate (which holds each frame until the next one arrives), each
//! with its capture time; AgentZero scales them. Our reader hands on the
//! device's own frames before anything else; ffmpeg's grabber makes them on
//! a second output. The capture card opens only once, so the policy never
//! grabs it itself.
//!
//! Every raw output is written with `-threads 1`: ffmpeg's rawvideo encoder
//! is frame threaded, which holds a frame or two back. And every pipe of
//! frames is grown as far as the system lets a user ([`grow_pipe`]): at
//! the default 64 KiB, a 1080p frame takes 48 hand-offs.
//!
//! Besides the screen and V4L2 devices, the input can be a video file, played
//! in a loop at its own pace as if it were live (for trying the lab
//! without a console); its frames are stamped with the time they were read.
//!
//! Frames reach recordings at a constant rate: frame `n` of a file was captured
//! `n / fps` seconds after its first frame, whose Unix time is kept. That time
//! is when the capture card delivered the frame to the kernel, which ffmpeg
//! reports for every frame (and our reader reads), not when it reached the
//! lab: if the grabber
//! ever falls behind, frames queue in the driver and arrive late for good, so
//! arrival times would shift a whole recording by however long that queue is.
//! A queue that builds up while nothing records is cleared by restarting the
//! grabber.
//!
//! With sound on, a recording also gets the capture card's sound
//! ([`Audio`]) through a second pipe, starting at the sample that arrived
//! with its first frame, as a second track of the same file.

use crate::config::VideoConfig;
use crate::studio::audio::{self, Audio};
use crate::studio::v4l2::{Capture, Dequeued};
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::ops::Range;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use core::time::Duration;
use procon_core::dump::unix_ms;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
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

/// The largest policy frame: YUYV at [`WORK_SIZE`], as ffmpeg's grabber
/// makes them (our reader's are the device's size; larger ones are not
/// handed on)
pub const POLICY_MAX_BYTES: usize = (WORK_SIZE.0 * WORK_SIZE.1 * 2) as usize;

/// Frames per second the policy gets: AgentZero's rate
pub const POLICY_FPS: u32 = 30;

/// A frame goes to the policy once its frame time, less half a frame of a
/// source at `fps`, passed since the last one: every other frame at 60 fps,
/// every fourth at 120, every one at 30 even when they jitter
fn policy_gap_ns(fps: u32) -> u64 {
    (1_000_000_000 / u64::from(POLICY_FPS)).saturating_sub(500_000_000 / u64::from(fps.max(1)))
}

/// How long our V4L2 reader waits for frames before looking whether it
/// should stop
const DEVICE_POLL: Duration = Duration::from_millis(100);

/// Starts of our own V4L2 reader without a frame before ffmpeg reads the
/// device instead (the Elgato 4K X streams only on every other start)
pub const DIRECT_TRIES: u32 = 3;

/// While the capture card keeps losing frames, the log sums them up at most
/// this often
const LOSS_LOG_EVERY: Duration = Duration::from_secs(60);

/// Capture times queued for frames not read yet; more means the log and the
/// frames are out of step, and the queue starts afresh
const TIMES_QUEUED: usize = 120;

/// Pipes of frames are grown to this, or as far as the system lets a user
/// (1 MiB unless /proc/sys/fs/pipe-max-size says otherwise)
const PIPE_BYTES: [i32; 2] = [1 << 22, 1 << 20];

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
    /// Time from the capture card delivering a frame to the lab reading
    /// it, smoothed; `None` while no frames arrive
    pub capture_ms: Option<f64>,
    /// The capture card is read by the lab itself, not ffmpeg
    pub direct: bool,
    /// What our reader counted of the card's frames; `None` while ffmpeg
    /// reads the input
    pub capture: Option<CaptureStatus>,
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

/// The capture card's frames as our V4L2 reader counts them: every frame
/// its driver handed over, the corrupted or short ones among them (skipped),
/// and the frames the driver dropped (gaps in its sequence numbers, never
/// handed over). The frame before stands in for a skipped or dropped one on
/// the constant rate, so recordings keep their timing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct CaptureCounts {
    pub frames: u64,
    pub corrupted: u64,
    pub dropped: u64,
}

impl CaptureCounts {
    /// What was counted since `start`, an earlier reading of the same
    /// counters
    pub fn since(self, start: Self) -> Self {
        Self {
            frames: self.frames.saturating_sub(start.frames),
            corrupted: self.corrupted.saturating_sub(start.corrupted),
            dropped: self.dropped.saturating_sub(start.dropped),
        }
    }

    /// Frames lost: skipped or dropped
    pub fn lost(self) -> u64 {
        self.corrupted + self.dropped
    }
}

/// [`CaptureCounts`] over the lab's life, which our reader adds to, and
/// when it last lost a frame
#[derive(Debug, Default)]
pub struct CaptureCounters {
    frames: AtomicU64,
    corrupted: AtomicU64,
    dropped: AtomicU64,
    /// Unix ms of the last frame skipped or dropped, 0 before any
    last_loss_ms: AtomicU64,
}

impl CaptureCounters {
    /// The counts now
    pub fn get(&self) -> CaptureCounts {
        CaptureCounts {
            frames: self.frames.load(Ordering::Relaxed),
            corrupted: self.corrupted.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }

    /// Count frames lost: `corrupted` skipped, `dropped` by the driver
    fn lose(&self, corrupted: u64, dropped: u64) {
        self.corrupted.fetch_add(corrupted, Ordering::Relaxed);
        self.dropped.fetch_add(dropped, Ordering::Relaxed);
        self.last_loss_ms.store(unix_ms(), Ordering::Relaxed);
    }
}

/// The capture card as our reader counted it, for the dashboard
#[derive(Debug, Serialize)]
pub struct CaptureStatus {
    /// When the reader started, Unix ms
    pub started_ms: u64,
    /// Since the reader started
    pub since_start: CaptureCounts,
    /// When it last lost a frame, Unix ms, if it did
    pub last_loss_ms: Option<u64>,
    /// Since the file being recorded started, while one is
    pub recording: Option<CaptureCounts>,
}

/// A raw frame shared by the preview and the recording: 4:2:0 at 1920 x
/// 1080 (see [`raw_input`])
pub type SharedFrame = Arc<Vec<u8>>;

/// The preview encoder's input, and how to thin the capture rate to its rate
#[derive(Clone)]
struct PreviewFeed {
    frames: SyncSender<SharedFrame>,
    /// Keep `fps` frames of every `capture_fps`
    fps: u32,
    capture_fps: u32,
}

/// When a policy frame went through each hand-off, on `CLOCK_MONOTONIC` in
/// ns ([`mono_ns`]), the clock the policy's process reads too
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicyTimes {
    /// The capture card's timestamp of the frame (a file's frames: when
    /// ffmpeg decoded them)
    pub captured: u64,
    /// Its first bytes arrived from the grabber: ffmpeg wrote it (our own
    /// V4L2 reader: the kernel handed it over)
    pub emitted: u64,
    /// The whole frame was read
    pub read: u64,
}

/// Where the policy's frames go: YUYV (4:2:2, Y0 U Y1 V) of `size`, at
/// [`POLICY_FPS`]; called on the grabber's thread for each, which must not
/// wait (a device's buffer, or ffmpeg, waits for it)
pub trait PolicySink: Send + Sync {
    fn frame(&self, frame: &[u8], size: (u32, u32), times: PolicyTimes);
}

/// Places frames on the recordings' constant rate by their capture times,
/// as ffmpeg's fps filter does, without holding a frame until the next one:
/// slot `n` is `n / fps` after the first frame's time; a frame takes the
/// slot nearest its time, a slot the source skipped repeats the frame
/// before, and a frame for a slot already filled is left out.
#[derive(Debug)]
pub struct ConstantRate {
    fps: u32,
    first: Option<u64>,
    /// The next slot to fill
    next: u64,
}

impl ConstantRate {
    pub fn new(fps: u32) -> Self {
        Self {
            fps,
            first: None,
            next: 0,
        }
    }

    /// The slots a frame captured at `captured` (ns) fills: none when it is
    /// left out, else its own last, the frame before repeating in the ones
    /// before it
    pub fn place(&mut self, captured: u64) -> Range<u64> {
        let first = *self.first.get_or_insert(captured);
        let slot =
            ((captured.saturating_sub(first)) as f64 * f64::from(self.fps) / 1e9).round() as u64;
        if slot < self.next {
            return self.next..self.next;
        }
        let filled = self.next..slot + 1;
        self.next = slot + 1;
        filled
    }

    /// The time (ns) of slot `slot`
    pub fn slot_time(&self, slot: u64) -> u64 {
        self.first.unwrap_or(0) + slot * 1_000_000_000 / u64::from(self.fps)
    }
}

/// Now on `CLOCK_MONOTONIC`, in ns: the clock the live policy's loop is
/// timed on, the same in every process (Python's `time.monotonic`)
pub fn mono_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec it is given
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

/// Now as Unix time, in ns
fn unix_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// A Unix time in seconds (ffmpeg's capture times) on `CLOCK_MONOTONIC`, ns
fn unix_to_mono(seconds: f64) -> u64 {
    let age = unix_ns().saturating_sub((seconds * 1e9) as u64);
    mono_ns().saturating_sub(age)
}

/// A `CLOCK_MONOTONIC` time (ns) as Unix time in µs, the recordings' clock
pub fn mono_to_unix_us(ns: u64) -> u64 {
    unix_ns().saturating_sub(mono_ns().saturating_sub(ns)) / 1000
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
    /// The capture card's counts when the file started
    capture_from: CaptureCounts,
}

struct Inner {
    config: VideoConfig,
    input: Option<String>,
    /// The preview uses the recording size and rate instead of its own
    preview_matches_recording: bool,
    record_audio: bool,
    grabber: Option<Grabber>,
    preview: Option<Worker>,
    error: Option<String>,
}

/// What turns the input into the grabber's raw 1080p frames
struct Grabber {
    /// The grabber's ffmpeg, or the converter behind our own V4L2 reader
    child: Child,
    /// Our own V4L2 reader, which holds the device until it ends
    reader: Option<JoinHandle<()>>,
    /// When our reader started (Unix ms) and the capture card's counts then
    counted_from: Option<(u64, CaptureCounts)>,
}

impl Grabber {
    /// Stop it (it writes no files, so there is nothing to finish); the
    /// device is free again once this returns. The grabber generation must
    /// have moved on, which ends our reader.
    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Handle to the capture, preview and recording processes; clones share them
#[derive(Clone)]
pub struct Video {
    inner: Arc<Mutex<Inner>>,
    /// Where grabbed frames go; separate locks, as the grabber's readers take
    /// them for every frame
    preview_feed: Arc<Mutex<Option<PreviewFeed>>>,
    policy_sink: Arc<Mutex<Option<Arc<dyn PolicySink>>>>,
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
    /// Capture times (Unix µs) the grabber logged (or our V4L2 reader
    /// wrote) for frames not read yet
    capture_times: Arc<Mutex<VecDeque<u64>>>,
    /// The same for the policy's frames, on `CLOCK_MONOTONIC` (ns)
    policy_times: Arc<Mutex<VecDeque<u64>>>,
    /// Smoothed time from capture to reading a frame, in µs
    capture_us: Arc<AtomicU64>,
    /// Starts of our own V4L2 reader in a row without a frame (see
    /// [`DIRECT_TRIES`])
    direct_failures: Arc<AtomicU32>,
    /// The current grabber is our own V4L2 reader
    direct: Arc<AtomicBool>,
    /// What our reader counted of the capture card's frames
    capture: Arc<CaptureCounters>,
    /// The sound source, when one is configured
    audio: Option<Audio>,
}

/// When a finished video file starts, and what the capture card lost
/// meanwhile
pub struct FileTimes {
    /// Unix ms of its first frame
    pub first_frame_ms: Option<u64>,
    /// Unix ms of its first sound sample, if it has sound
    pub audio_start_ms: Option<u64>,
    /// The capture card's frames while it was recorded, when our reader
    /// read the card
    pub capture: Option<CaptureCounts>,
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
            policy_sink: Arc::default(),
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
            policy_times: Arc::default(),
            capture_us: Arc::default(),
            direct_failures: Arc::default(),
            direct: Arc::default(),
            capture: Arc::default(),
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

    /// Stop the sound capture before exiting
    pub fn stop_audio(&self) {
        if let Some(audio) = &self.audio {
            audio.stop();
        }
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
        // Another input gets our own reader again
        self.direct_failures.store(0, Ordering::Relaxed);
        self.restart_grabber(&mut inner);
        // The preview only runs while there is an input
        if inner.input.is_none() || inner.preview.is_none() {
            self.restart_preview(&mut inner);
        }
        Ok(())
    }

    /// Whether the lab reads the capture card itself (see the module docs)
    pub fn direct(&self) -> bool {
        self.direct.load(Ordering::Relaxed)
    }

    /// Selected input id
    pub fn input(&self) -> Option<String> {
        self.lock().input.clone()
    }

    /// Hand the grabber's policy frames to the Predictor's live policy from
    /// the next frame on, or stop (`None`)
    pub fn set_policy_sink(&self, sink: Option<Arc<dyn PolicySink>>) {
        *lock(&self.policy_sink) = sink;
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
        // Sound goes to the encoder through a pipe of its own, its fd 3
        let audio_pipe = if with_audio {
            pipe().map(Some)
        } else {
            Ok(None)
        };
        let spawned = audio_pipe.and_then(|pipe| {
            let (read_end, write_end) = pipe.unzip();
            let (worker, _) = spawn_worker(&args, queue, false, read_end.map(OwnedFd::from))?;
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
                    capture_from: self.capture.get(),
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
        // Counted only while our reader read the card
        let capture = Some(self.capture.get().since(recording.capture_from))
            .filter(|counts| counts.frames > 0);
        if let Some(counts) = capture.filter(|counts| counts.lost() > 0) {
            log::warn!(
                "The capture card lost {} while {} was recorded; skipped, the frame before \
                 standing in for each",
                lost_frames(counts),
                recording.path.display()
            );
        }
        Some(FileTimes {
            first_frame_ms: recording.first_frame_ms,
            audio_start_ms: recording.audio_start_ms,
            capture,
        })
    }

    /// Take a snapshot for the dashboard
    pub fn status(&self) -> VideoStatus {
        let (recording, recording_from) = lock(&self.recording)
            .as_ref()
            .map(|r| (r.path.display().to_string(), r.capture_from))
            .unzip();
        let inner = self.lock();
        let counts = self.capture.get();
        let capture = inner
            .grabber
            .as_ref()
            .and_then(|grabber| grabber.counted_from)
            .map(|(started_ms, from)| CaptureStatus {
                started_ms,
                since_start: counts.since(from),
                last_loss_ms: Some(self.capture.last_loss_ms.load(Ordering::Relaxed))
                    .filter(|&ms| ms >= started_ms),
                recording: recording_from.map(|from| counts.since(from)),
            });
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
            direct: self.direct(),
            capture,
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
        if let Some(grabber) = inner.grabber.take() {
            grabber.stop();
        }
        self.direct.store(false, Ordering::Relaxed);
        inner.error = None;
        self.last_frame_ms.store(0, Ordering::Relaxed);
        lock(&self.capture_times).clear();
        lock(&self.policy_times).clear();
        self.capture_us.store(0, Ordering::Relaxed);

        let Some(input) = inner.input.clone() else {
            return;
        };
        match self.spawn_grabber(inner, &input) {
            Ok(grabber) => {
                inner.grabber = Some(grabber);
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

    /// Our own V4L2 reader when the input allows it (see the module docs),
    /// else ffmpeg's grabber
    fn spawn_grabber(&self, inner: &Inner, input: &str) -> Result<Grabber> {
        if let Some(size) = direct_size(&inner.config, input)
            && self.direct_failures.load(Ordering::Relaxed) < DIRECT_TRIES
        {
            match self.spawn_direct(inner, input, size) {
                Ok(grabber) => {
                    self.direct_failures.store(0, Ordering::Relaxed);
                    return Ok(grabber);
                }
                Err(e) => {
                    log::warn!("Cannot read {input} ourselves ({e:#}); ffmpeg reads it");
                    self.direct_failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.direct.store(false, Ordering::Relaxed);
        self.spawn_ffmpeg(inner, input)
    }

    /// Read the device `input` ourselves, `size` YUYV at the capture rate,
    /// into the policy and a converter ffmpeg for the rest
    fn spawn_direct(&self, inner: &Inner, input: &str, size: (u32, u32)) -> Result<Grabber> {
        let fps = inner.config.fps;
        let capture = Capture::open(Path::new(input), size.0, size.1, fps)?;
        let args = converter_args(size, fps);
        log::info!(
            "Reading {input} ourselves ({}x{} YUYV at {fps} fps); starting ffmpeg {}",
            size.0,
            size.1,
            args.join(" ")
        );
        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .context("cannot run ffmpeg; is it installed?")?;
        let stdin = child.stdin.take().context("no ffmpeg stdin")?;
        let stdout = child.stdout.take().context("no ffmpeg stdout")?;
        let stderr = child.stderr.take().context("no ffmpeg stderr")?;
        grow_pipe(&stdin);
        grow_pipe(&stdout);
        let generation = self.grabber_generation.load(Ordering::SeqCst);
        let video = self.clone();
        thread::spawn(move || video.read_frames(stdout, generation));
        let video = self.clone();
        thread::spawn(move || video.read_log(stderr, generation));
        let video = self.clone();
        let reader = thread::Builder::new()
            .name("capture".to_string())
            .spawn(move || video.read_device(capture, stdin, generation, fps))
            .context("cannot start a thread")?;
        self.direct.store(true, Ordering::Relaxed);
        Ok(Grabber {
            child,
            reader: Some(reader),
            counted_from: Some((unix_ms(), self.capture.get())),
        })
    }

    /// ffmpeg's grabber: the input as raw 1080p frames on stdout, the
    /// policy's on fd 3
    fn spawn_ffmpeg(&self, inner: &Inner, input: &str) -> Result<Grabber> {
        let args = grabber_args(&inner.config, input);
        log::info!("Starting ffmpeg {}", args.join(" "));
        // The policy's frames come on the grabber's fd 3
        let (policy_frames, policy_end) = pipe()?;
        let mut command = Command::new("ffmpeg");
        inherit_as_fd3(&mut command, &policy_end);
        let mut child = command
            .args(&args)
            .stdin(Stdio::null())
            // Own process group: a terminal Ctrl+C reaches the lab, which then stops ffmpeg in order
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot run ffmpeg; is it installed?")?;
        // The child has its copy; ours would keep the pipe open
        drop(policy_end);

        let stdout = child.stdout.take().context("no ffmpeg stdout")?;
        let stderr = child.stderr.take().context("no ffmpeg stderr")?;
        grow_pipe(&stdout);
        grow_pipe(&policy_frames);
        let generation = self.grabber_generation.load(Ordering::SeqCst);
        let video = self.clone();
        thread::spawn(move || video.read_frames(stdout, generation));
        let video = self.clone();
        thread::spawn(move || video.read_policy_frames(policy_frames, generation));
        let video = self.clone();
        thread::spawn(move || video.read_log(stderr, generation));
        Ok(Grabber {
            child,
            reader: None,
            counted_from: None,
        })
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
            log::warn!(
                "No frames from {input} in {} ms, reopening it (the Elgato 4K X streams only on \
                 every other start; without a picture from the console none come either)",
                FIRST_FRAME_TIMEOUT.as_millis()
            );
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

    /// Hand grabbed frames to the preview and the recording; when the
    /// grabber dies on its own, retry after a pause
    fn read_frames(&self, mut stdout: ChildStdout, generation: u64) {
        let mut frame = vec![0u8; FRAME_BYTES];
        // Count toward the next frame kept for the preview
        let mut preview_phase = 0;
        // Since when frames have been arriving too long after capture
        let mut backlog_since: Option<Instant> = None;
        // Read to the end even when stale, so ffmpeg can exit
        while stdout.read_exact(&mut frame).is_ok() {
            if !self.grabber_is_current(generation) {
                continue;
            }
            let now = unix_ms();
            // Frames flow again: why the grabber was retried is past
            if self.last_frame_ms.swap(now, Ordering::Relaxed) == 0 {
                let mut inner = self.lock();
                if self.grabber_is_current(generation) {
                    inner.error = None;
                }
            }
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

    /// Hand ffmpeg's policy frames to the live policy while one wants them;
    /// read them all the same, as ffmpeg waits for every output
    fn read_policy_frames(&self, mut pipe: File, generation: u64) {
        let mut frame = vec![0u8; POLICY_MAX_BYTES];
        // Read to the end even when stale, so ffmpeg can exit
        while let Some(emitted) = read_frame(&mut pipe, &mut frame) {
            let read = mono_ns();
            if !self.grabber_is_current(generation) {
                continue;
            }
            let captured = self.policy_capture_time().unwrap_or(read);
            let sink = lock(&self.policy_sink).clone();
            if let Some(sink) = sink {
                let times = PolicyTimes {
                    captured,
                    emitted,
                    read,
                };
                sink.frame(&frame, WORK_SIZE, times);
            }
        }
    }

    /// Our own V4L2 reader's thread (see [`pump`]) until the grabber moves
    /// on or the device fails; then the capture closes, and the converter
    /// ends with its input, which restarts the grabber
    fn read_device(&self, mut capture: Capture, converter: ChildStdin, generation: u64, fps: u32) {
        let running = || self.grabber_is_current(generation);
        if let Err(e) = pump(
            &mut capture,
            converter,
            fps,
            &self.capture_times,
            &self.policy_sink,
            &self.capture,
            running,
        ) && running()
        {
            log::warn!("Reading the capture card: {e:#}");
            // Never waits: a restart holds the lock while it waits for us
            if let Ok(mut inner) = self.inner.try_lock() {
                inner.error = Some(format!("{e:#}"));
            }
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
    fn capture_time(&self, now: u64) -> u64 {
        next_time(&self.capture_times).map_or(now, |us| us / 1000)
    }

    /// Capture time (`CLOCK_MONOTONIC` ns) of the policy frame just read
    fn policy_capture_time(&self) -> Option<u64> {
        next_time(&self.policy_times)
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
                    // The policy's output, or the main one
                    if line.starts_with(POLICY_LOG) {
                        push_time(&self.policy_times, unix_to_mono(seconds));
                    } else {
                        push_time(&self.capture_times, (seconds * 1e6) as u64);
                    }
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
        inherit_as_fd3(&mut command, fd);
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
    grow_pipe(&stdin);
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

/// A pipe: the end to read and the end to write, both closed on exec (a
/// child gets its end through [`inherit_as_fd3`])
fn pipe() -> Result<(File, File)> {
    let mut fds = [0; 2];
    // SAFETY: pipe2 fills both descriptors, which we then own
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error()).context("cannot make a pipe");
    }
    // SAFETY: fresh descriptors from pipe2, owned by nothing else
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

/// Give the child `command` starts `fd` as its descriptor 3 (ffmpeg's
/// `pipe:3`, AgentZero's shared frames)
pub fn inherit_as_fd3(command: &mut Command, fd: &impl AsRawFd) {
    let raw = fd.as_raw_fd();
    // SAFETY: dup2 and fcntl are async-signal-safe. The copy at 3 is
    // inherited, as copies do not keep the close-on-exec flag; a descriptor
    // that is 3 already only loses the flag.
    unsafe {
        command.pre_exec(move || {
            let done = if raw == 3 {
                libc::fcntl(3, libc::F_SETFD, 0)
            } else {
                libc::dup2(raw, 3)
            };
            if done < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Grow a pipe of frames as far as the system lets a user (see
/// [`PIPE_BYTES`]): a frame then takes a few hand-offs instead of dozens.
/// Best effort.
pub fn grow_pipe(pipe: &impl AsRawFd) {
    for bytes in PIPE_BYTES {
        // SAFETY: fcntl on a descriptor the caller owns
        if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETPIPE_SZ, bytes) } >= 0 {
            return;
        }
    }
}

/// Read one whole frame into `frame`, answering when its first bytes came
/// ([`mono_ns`]); `None` at the end of the stream
fn read_frame(reader: &mut impl Read, frame: &mut [u8]) -> Option<u64> {
    let first = loop {
        match reader.read(frame) {
            Ok(0) => return None,
            Ok(n) => break n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    };
    let came = mono_ns();
    reader.read_exact(&mut frame[first..]).ok()?;
    Some(came)
}

/// Queue a capture time for the frame to come; a queue out of step with
/// the frames starts afresh
fn push_time(times: &Mutex<VecDeque<u64>>, time: u64) {
    let mut times = lock(times);
    if times.len() > TIMES_QUEUED {
        times.clear();
    }
    times.push_back(time);
}

/// What our V4L2 reader reads: a [`Capture`], or a stand-in in tests
trait FrameSource {
    /// Width and height of its YUYV frames
    fn size(&self) -> (u32, u32);
    /// Every frame ready, oldest first, after waiting up to `timeout`
    fn ready(&mut self, timeout: Duration) -> Result<Vec<Dequeued>>;
    /// A dequeued frame's bytes
    fn frame(&self, index: u32) -> &[u8];
    /// Give a dequeued frame's buffer back
    fn requeue(&mut self, index: u32) -> Result<()>;
}

impl FrameSource for Capture {
    fn size(&self) -> (u32, u32) {
        Capture::size(self)
    }
    fn ready(&mut self, timeout: Duration) -> Result<Vec<Dequeued>> {
        Capture::ready(self, timeout)
    }
    fn frame(&self, index: u32) -> &[u8] {
        Capture::frame(self, index)
    }
    fn requeue(&mut self, index: u32) -> Result<()> {
        Capture::requeue(self, index)
    }
}

/// Our V4L2 reader, until `running` says stop: of the frames ready, the
/// newest goes to the policy first (once [`policy_gap_ns`] passed since the
/// last), then each into `converter` on the constant rate (the frame before
/// repeated for the slots the source skipped), its capture time into
/// `capture_times` just before; corrupted or short frames are left out.
/// The frame last written keeps its buffer until the next one, for repeats.
/// Every frame, and every one skipped or dropped, goes into `counters`
/// ([`CaptureCounts`]), whose losses the log sums up ([`LossLog`]).
fn pump(
    source: &mut impl FrameSource,
    mut converter: impl Write,
    fps: u32,
    capture_times: &Mutex<VecDeque<u64>>,
    policy_sink: &Mutex<Option<Arc<dyn PolicySink>>>,
    counters: &CaptureCounters,
    running: impl Fn() -> bool,
) -> Result<()> {
    let size = source.size();
    let frame_bytes = (size.0 * size.1 * 2) as usize;
    let mut rate = ConstantRate::new(fps);
    // The buffer of the frame last written
    let mut held: Option<u32> = None;
    let mut policy_last: Option<u64> = None;
    let policy_gap = policy_gap_ns(fps);
    let mut losses = LossLog::new(counters.get());
    // Frames the driver dropped, for want of a free buffer (the reader fell
    // behind) or on the bus: gaps in its count
    let mut sequence: Option<u32> = None;
    while running() {
        let ready = source.ready(DEVICE_POLL)?;
        let dequeued = mono_ns();
        let whole = |frame: &Dequeued| !frame.error && frame.bytes as usize == frame_bytes;
        let newest = ready.iter().rev().find(|f| whole(f)).map(|f| f.index);
        for frame in &ready {
            counters.frames.fetch_add(1, Ordering::Relaxed);
            if let Some(last) = sequence.replace(frame.sequence) {
                let gap = u64::from(frame.sequence.wrapping_sub(last).saturating_sub(1));
                if gap > 0 && gap < 1 << 16 {
                    counters.lose(0, gap);
                }
            }
            if !whole(frame) {
                source.requeue(frame.index)?;
                counters.lose(1, 0);
                continue;
            }
            let captured = frame.captured.unwrap_or(dequeued);
            if Some(frame.index) == newest
                && policy_last.is_none_or(|last| captured >= last + policy_gap)
            {
                let sink = lock(policy_sink).clone();
                if let Some(sink) = sink {
                    let times = PolicyTimes {
                        captured,
                        emitted: dequeued,
                        read: dequeued,
                    };
                    sink.frame(source.frame(frame.index), size, times);
                    policy_last = Some(captured);
                }
            }
            let slots = rate.place(captured);
            if slots.is_empty() {
                source.requeue(frame.index)?;
                continue;
            }
            // The frame before, again in the slots the source skipped
            if let Some(before) = held.take() {
                for slot in slots.start..slots.end - 1 {
                    push_time(capture_times, mono_to_unix_us(rate.slot_time(slot)));
                    converter.write_all(source.frame(before))?;
                }
                source.requeue(before)?;
            }
            push_time(capture_times, mono_to_unix_us(captured));
            converter.write_all(source.frame(frame.index))?;
            held = Some(frame.index);
        }
        losses.note(counters.get());
    }
    Ok(())
}

/// The log's account of the capture card's losses ([`CaptureCounts`]): the
/// first at once, then, while more come, at most a line a
/// [`LOSS_LOG_EVERY`] with how many since the line before and their rate,
/// so a burst at the start reads apart from a steady loss
struct LossLog {
    /// The counters when the reader started, and when it did
    start: (Instant, CaptureCounts),
    /// When the last line was written, and the counters then
    last: Option<(Instant, CaptureCounts)>,
}

impl LossLog {
    fn new(start: CaptureCounts) -> Self {
        Self {
            start: (Instant::now(), start),
            last: None,
        }
    }

    /// The counters now: write a line when it is time
    fn note(&mut self, now: CaptureCounts) {
        if let Some(line) = self.line(now, Instant::now()) {
            log::warn!("{line}");
        }
    }

    /// The line to write at `at` with the counters `now`, if one is due
    fn line(&mut self, now: CaptureCounts, at: Instant) -> Option<String> {
        let (started, start) = self.start;
        let total = now.since(start);
        let line = match self.last {
            None if total.lost() > 0 => format!(
                "The capture card lost {}; skipped (the frame before stands in for each, so \
                 recordings keep their timing). While it goes on, a line a minute sums it up",
                lost_frames(total)
            ),
            Some((last, before))
                if at.duration_since(last) >= LOSS_LOG_EVERY && now.since(before).lost() > 0 =>
            {
                let new = now.since(before);
                // Of the frames the card sent meanwhile: handed over or dropped
                let sent = (new.frames + new.dropped).max(1);
                format!(
                    "The capture card lost {} in the last {:.0} s ({:.1} per 1000 frames), all \
                     skipped; {} since the reader started {:.0} min ago",
                    lost_frames(new),
                    at.duration_since(last).as_secs_f64(),
                    new.lost() as f64 * 1000.0 / sent as f64,
                    lost_frames(total),
                    at.duration_since(started).as_secs_f64() / 60.0
                )
            }
            _ => return None,
        };
        self.last = Some((at, now));
        Some(line)
    }
}

/// "3 frames (2 corrupted or short, 1 dropped by its driver)"
fn lost_frames(counts: CaptureCounts) -> String {
    let frames = |n: u64| {
        if n == 1 {
            "1 frame".into()
        } else {
            format!("{n} frames")
        }
    };
    format!(
        "{} ({} corrupted or short, {} dropped by its driver)",
        frames(counts.lost()),
        counts.corrupted,
        counts.dropped
    )
}

/// The size to read a V4L2 device `input` at ourselves: when the config
/// allows it and its `v4l2_args` ask for no more than YUYV (`-input_format
/// yuyv422`) at a size (`-video_size WxH`, else [`WORK_SIZE`])
fn direct_size(config: &VideoConfig, input: &str) -> Option<(u32, u32)> {
    if !config.v4l2_direct || input == SCREEN || Path::new(input).is_file() {
        return None;
    }
    let mut size = WORK_SIZE;
    for pair in config.v4l2_args.chunks(2) {
        match pair {
            [key, value] if key == "-input_format" && value == "yuyv422" => {}
            [key, value] if key == "-video_size" => {
                let (width, height) = value.split_once('x')?;
                size = (width.parse().ok()?, height.parse().ok()?);
            }
            _ => return None,
        }
    }
    (size.0 * size.1 * 2 <= POLICY_MAX_BYTES as u32 && size.0.is_multiple_of(2)).then_some(size)
}

/// The next queued capture time; the grabber logs a frame's time just
/// before writing the frame, so it is normally there: give the log reader a
/// moment if not
fn next_time(times: &Mutex<VecDeque<u64>>) -> Option<u64> {
    for _ in 0..20 {
        if let Some(time) = lock(times).pop_front() {
            return Some(time);
        }
        thread::sleep(Duration::from_millis(1));
    }
    None
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
pub fn raw_input(fps: u32) -> Vec<String> {
    let mut args: Vec<String> = ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size"]
        .map(String::from)
        .to_vec();
    args.push(format!("{}x{}", WORK_SIZE.0, WORK_SIZE.1));
    args.extend(["-framerate".to_string(), fps.to_string()]);
    args.extend(["-i", "pipe:0"].map(String::from));
    args
}

/// Command line for the grabber: `input` as raw 1080p frames on stdout, and
/// the policy's frames on fd 3
fn grabber_args(config: &VideoConfig, input: &str) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-nostats", "-loglevel", "info"]
        .map(String::from)
        .to_vec();
    // Frame times: X11 stamps frames with the wall clock itself; for V4L2,
    // the kernel's capture time, converted to Unix time; a file's frames
    // get the wall clock when they are read
    let mut stamp = "";
    if input == SCREEN {
        let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
        args.extend(["-f", "x11grab", "-framerate"].map(String::from));
        args.push(config.fps.to_string());
        args.extend(["-i".to_string(), display]);
    } else if Path::new(input).is_file() {
        // At its own pace, over and over, as if live
        args.extend(["-re", "-stream_loop", "-1", "-i"].map(String::from));
        args.push(input.to_string());
        stamp = "setpts=RTCTIME/(1000000*TB),";
    } else {
        args.extend(["-f", "v4l2", "-ts", "mono2abs"].map(String::from));
        args.extend(config.v4l2_args.iter().cloned());
        args.extend(["-framerate".to_string(), config.fps.to_string()]);
        args.extend(["-i".to_string(), input.to_string()]);
    }

    // Fit the source into 16:9, padded if it has another shape, then split:
    // at a constant rate on stdout, and for the policy on fd 3 before that
    // (the constant rate holds each frame until the next one comes), a
    // frame once 3/4 of the policy's frame time passed since the last (every
    // other one at 60 fps, every one at 30 even when they jitter), as YUYV
    // like a capture card's. showinfo logs each frame's capture time (kept
    // absolute by -copyts); the policy's timestamps in µs never collide,
    // since the muxer drops a frame whose time repeats.
    args.push("-copyts".to_string());
    args.push("-filter_complex".to_string());
    args.push(format!(
        "[0:v]{stamp}{fit},format=yuv420p,split[main][policy];\
         [main]fps={fps},showinfo=checksum=0[out];\
         [policy]select='isnan(prev_selected_t)+gte(t-prev_selected_t,{gap})',\
         format=yuyv422,settb=AVTB,showinfo@policy=checksum=0[small]",
        fit = fit(),
        fps = config.fps,
        // A file's or the screen's own rate is not known here: as at 60 fps
        gap = policy_gap_ns(60) as f64 / 1e9,
    ));
    args.extend(["-map", "[out]", "-threads", "1", "-f", "rawvideo", "pipe:1"].map(String::from));
    args.extend(
        [
            "-map",
            "[small]",
            "-fps_mode",
            "passthrough",
            "-threads",
            "1",
            "-f",
            "rawvideo",
            "pipe:3",
        ]
        .map(String::from),
    );
    args
}

/// How the grabber's log lines about the policy's frames begin
const POLICY_LOG: &str = "[showinfo@policy ";

/// The filter fitting a source into [`WORK_SIZE`], padded if it has
/// another shape than 16:9
fn fit() -> String {
    let (width, height) = WORK_SIZE;
    format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
    )
}

/// Command line for the converter behind our own V4L2 reader: its YUYV
/// frames of `size`, on the constant rate already, as the grabber's raw
/// 1080p frames on stdout
fn converter_args(size: (u32, u32), fps: u32) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "warning"]
        .map(String::from)
        .to_vec();
    args.extend(["-f", "rawvideo", "-pix_fmt", "yuyv422", "-video_size"].map(String::from));
    args.push(format!("{}x{}", size.0, size.1));
    args.extend(["-framerate".to_string(), fps.to_string()]);
    args.extend(["-i", "pipe:0", "-vf"].map(String::from));
    args.push(format!("{},format=yuv420p", fit()));
    args.extend(
        [
            "-fps_mode",
            "passthrough",
            "-threads",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ]
        .map(String::from),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> VideoConfig {
        VideoConfig {
            input: String::new(),
            fps: 60,
            v4l2_args: ["-input_format", "yuyv422", "-video_size", "1920x1080"]
                .map(String::from)
                .to_vec(),
            v4l2_direct: true,
            record_height: 720,
            record_fps: 30,
            encoder: Vec::new(),
            extension: "mkv".to_string(),
            preview_height: 1080,
            preview_fps: 60,
            preview_encoder: Vec::new(),
            audio_input: String::new(),
            audio_offset_ms: 0,
        }
    }

    /// The options of `output` in `args`, from its `-map` on
    fn output_options<'a>(args: &'a [String], output: &str) -> &'a [String] {
        let end = args.iter().position(|a| a == output).unwrap();
        let start = args[..end].iter().rposition(|a| a == "-map").unwrap();
        &args[start..end]
    }

    #[test]
    fn the_grabber_splits_off_the_policys_frames_before_the_constant_rate() {
        let args = grabber_args(&config(), "/dev/video9");
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        let (main, policy) = graph.split_once("[main]fps=60").unwrap();
        // One fit for both, then the constant rate on the main output only
        assert!(main.ends_with("format=yuv420p,split[main][policy];"));
        assert!(!policy.contains("fps="));
        assert!(policy.contains("format=yuyv422,settb=AVTB"));
        assert!(policy.contains("gte(t-prev_selected_t,0.025)"));
        assert!(policy.contains(&POLICY_LOG[1..POLICY_LOG.len() - 1]));
        // Both raw outputs without the frame-threaded encoder's delay
        let out = output_options(&args, "pipe:1");
        assert_eq!(out, ["-map", "[out]", "-threads", "1", "-f", "rawvideo"]);
        let small = output_options(&args, "pipe:3");
        assert!(small.windows(2).any(|w| w == ["-threads", "1"]));
        assert!(small.windows(2).any(|w| w == ["-fps_mode", "passthrough"]));
        // The capture card's own timestamps; a file is stamped as read
        assert!(args.windows(2).any(|w| w == ["-ts", "mono2abs"]));
        let file = std::env::current_exe().unwrap();
        let args = grabber_args(&config(), &file.display().to_string());
        assert!(args.iter().any(|a| a.contains("[0:v]setpts=RTCTIME")));
    }

    /// Hands out its bytes a few at a time, as a pipe does
    struct Trickle(Vec<u8>);

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.0.len()).min(7);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0.drain(..n);
            Ok(n)
        }
    }

    #[test]
    fn frames_are_read_whole() {
        let mut pipe = Trickle((0..50).collect());
        let mut frame = [0u8; 20];
        let before = mono_ns();
        let came = read_frame(&mut pipe, &mut frame).unwrap();
        assert!(came >= before && came <= mono_ns());
        assert_eq!(frame[19], 19);
        read_frame(&mut pipe, &mut frame).unwrap();
        assert_eq!(frame[0], 20);
        // Ten bytes left: not a frame
        assert_eq!(read_frame(&mut pipe, &mut frame), None);
    }

    #[test]
    fn frames_keep_a_constant_rate_without_waiting_for_the_next() {
        let ms = 1_000_000;
        let mut rate = ConstantRate::new(60);
        // Steady with jitter: a slot each
        for (t, slot) in [(1000, 0), (1017, 1), (1032, 2), (1051, 3)] {
            assert_eq!(rate.place(t * ms), slot..slot + 1);
        }
        // Two frames the source skipped: the frame before fills slots 4 and 5
        assert_eq!(rate.place(1100 * ms), 4..7);
        // A second frame for a slot already filled is left out
        assert!(rate.place(1104 * ms).is_empty());
        assert_eq!(rate.place(1117 * ms), 7..8);
        assert_eq!(rate.slot_time(6), 1100 * ms);
    }

    #[test]
    fn only_yuyv_at_a_size_is_read_ourselves() {
        let mut config = config();
        assert_eq!(direct_size(&config, "/dev/video9"), Some((1920, 1080)));
        let exe = std::env::current_exe().unwrap().display().to_string();
        assert_eq!(direct_size(&config, SCREEN), None);
        assert_eq!(direct_size(&config, &exe), None);
        config.v4l2_args = ["-video_size", "1280x720"].map(String::from).to_vec();
        assert_eq!(direct_size(&config, "/dev/video9"), Some((1280, 720)));
        for args in [
            &["-input_format", "mjpeg"][..],
            &["-video_size", "2560x1440"],
            &["-standard", "PAL"],
            &["-video_size"],
        ] {
            config.v4l2_args = args.iter().map(|a| a.to_string()).collect();
            assert_eq!(direct_size(&config, "/dev/video9"), None, "{args:?}");
        }
        config.v4l2_args.clear();
        assert_eq!(direct_size(&config, "/dev/video9"), Some(WORK_SIZE));
        config.v4l2_direct = false;
        assert_eq!(direct_size(&config, "/dev/video9"), None);
        let args = converter_args((1280, 720), 60).join(" ");
        assert!(args.contains("-pix_fmt yuyv422 -video_size 1280x720 -framerate 60 -i pipe:0"));
        assert!(
            args.contains("format=yuv420p -fps_mode passthrough -threads 1 -f rawvideo pipe:1")
        );
    }

    /// A device stand-in: frames of 4 x 2 YUYV (16 bytes) filled with their
    /// number, handed out a batch per wait, each with its capture time,
    /// whether it is whole, and the driver's sequence number
    struct FakeDevice {
        batches: VecDeque<Vec<(u64, bool, u32)>>,
        buffers: Vec<Option<u8>>,
        next: u8,
        /// The frames whose buffers went back
        requeued: Vec<u8>,
    }

    impl FrameSource for FakeDevice {
        fn size(&self) -> (u32, u32) {
            (4, 2)
        }
        fn ready(&mut self, _: Duration) -> Result<Vec<Dequeued>> {
            let batch = self.batches.pop_front().unwrap_or_default();
            Ok(batch
                .into_iter()
                .map(|(captured, whole, sequence)| {
                    let index = self.buffers.iter().position(Option::is_none).unwrap() as u32;
                    self.buffers[index as usize] = Some(self.next);
                    self.next += 1;
                    Dequeued {
                        index,
                        captured: Some(captured),
                        sequence,
                        bytes: if whole { 16 } else { 8 },
                        error: false,
                    }
                })
                .collect())
        }
        fn frame(&self, index: u32) -> &[u8] {
            const FRAMES: [[u8; 16]; 16] = {
                let mut frames = [[0u8; 16]; 16];
                let mut n = 0;
                while n < 16 {
                    frames[n] = [n as u8; 16];
                    n += 1;
                }
                frames
            };
            &FRAMES[self.buffers[index as usize].unwrap() as usize]
        }
        fn requeue(&mut self, index: u32) -> Result<()> {
            let frame = self.buffers[index as usize].take();
            self.requeued.push(frame.expect("requeued twice"));
            Ok(())
        }
    }

    /// What reaches the policy: frame numbers and capture times
    #[derive(Default)]
    struct Policy(Mutex<Vec<(u8, u64)>>);

    impl PolicySink for Policy {
        fn frame(&self, frame: &[u8], size: (u32, u32), times: PolicyTimes) {
            assert_eq!(size, (4, 2));
            self.0.lock().unwrap().push((frame[0], times.captured));
        }
    }

    #[test]
    fn our_reader_feeds_the_policy_the_newest_and_the_rest_every_frame() {
        let ms = 1_000_000;
        let base = mono_ns() - 10_000 * ms;
        let at = |t: u64| base + t * ms;
        let mut device = FakeDevice {
            batches: VecDeque::from([
                vec![(at(0), true, 10)],
                vec![(at(17), true, 11)],
                // Behind: two ready at once, the second one short
                vec![(at(33), true, 12), (at(50), false, 13)],
                // The driver dropped a frame
                vec![(at(83), true, 15)],
                vec![(at(100), true, 16), (at(117), true, 17)],
            ]),
            buffers: vec![None; 4],
            next: 0,
            requeued: Vec::new(),
        };
        let times = Mutex::default();
        let policy = Arc::new(Policy::default());
        let sink: Arc<dyn PolicySink> = policy.clone();
        let sinks = Mutex::new(Some(sink));
        let mut converter = Vec::new();
        let waits = core::cell::Cell::new(0);
        let counters = CaptureCounters::default();
        pump(
            &mut device,
            &mut converter,
            60,
            &times,
            &sinks,
            &counters,
            || {
                waits.set(waits.get() + 1);
                waits.get() <= 5
            },
        )
        .unwrap();
        // Every slot from 0 ms to 117 ms: frame 2 again for the short one
        // (50 ms) and the one skipped (67 ms); 83 ms is slot 5
        let written: Vec<u8> = converter.chunks(16).map(|f| f[0]).collect();
        assert_eq!(written, [0, 1, 2, 2, 2, 4, 5, 6]);
        assert!(converter.chunks(16).all(|f| f.iter().all(|&b| b == f[0])));
        let times: Vec<u64> = times.into_inner().unwrap().into();
        assert_eq!(times.len(), 8);
        assert!(times.windows(2).all(|w| w[1] > w[0]));
        // The policy: the newest whole frame, 25 ms or more apart
        let seen = policy.0.lock().unwrap().clone();
        assert_eq!(seen, [(0, at(0)), (2, at(33)), (4, at(83)), (6, at(117))]);
        // The short frame went back at once; the last one written is held
        assert_eq!(device.requeued, [0, 1, 3, 2, 4, 5]);
        assert_eq!(device.buffers.iter().flatten().collect::<Vec<_>>(), [&6]);
        // Seven frames handed over, the short one skipped, one dropped
        let counts = CaptureCounts {
            frames: 7,
            corrupted: 1,
            dropped: 1,
        };
        assert_eq!(counters.get(), counts);
        assert_eq!(counts.lost(), 2);
        assert_eq!(
            counts.since(CaptureCounts {
                frames: 3,
                corrupted: 1,
                dropped: 0
            }),
            CaptureCounts {
                frames: 4,
                corrupted: 0,
                dropped: 1
            }
        );
    }

    #[test]
    fn losses_are_logged_first_then_summed_up_a_minute_apart() {
        let counts = |frames, corrupted, dropped| CaptureCounts {
            frames,
            corrupted,
            dropped,
        };
        let mut log = LossLog::new(counts(1000, 3, 0));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        // Nothing lost since the reader started: nothing to say
        assert_eq!(log.line(counts(1100, 3, 0), at(1)), None);
        // The first loss at once
        let first = log.line(counts(1200, 4, 0), at(2)).unwrap();
        assert!(first.contains("lost 1 frame (1 corrupted or short, 0 dropped"));
        assert!(first.contains("skipped"));
        // More within the minute wait for the next line
        assert_eq!(log.line(counts(2000, 6, 1), at(30)), None);
        let next = log.line(counts(4800, 7, 1), at(62)).unwrap();
        // 4 lost of the 3601 frames the card sent in those 60 s
        assert!(next.contains("lost 4 frames (3 corrupted or short, 1 dropped"));
        assert!(
            next.contains("in the last 60 s (1.1 per 1000 frames)"),
            "{next}"
        );
        assert!(next.contains("5 frames (4 corrupted or short, 1 dropped by its driver) since"));
        // A burst that stopped says nothing more
        assert_eq!(log.line(counts(9000, 7, 1), at(200)), None);
    }

    #[test]
    fn capture_times_move_to_the_monotonic_clock() {
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let now = mono_ns();
        let mono = unix_to_mono(unix - 0.25);
        let age_ms = (now as f64 - mono as f64) / 1e6;
        assert!((249.0..260.0).contains(&age_ms), "{age_ms}");
    }
}
