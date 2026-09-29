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
//! - **the live capture**: the studio holds the capture card (it opens only
//!   once), and its grabber makes the policy's frames on an output of their
//!   own ([`video::PolicySink`]): 640 x 360 RGB at 30 fps, split off before
//!   the recording's constant rate. Each is written into shared memory
//!   ([`SharedFrames`]: a memfd `agentzero-play` gets as its fd 3, a ring of
//!   [`SLOTS`] slots) with a notice on its stdin, and the policy takes the
//!   newest whenever the model is free: no pipe of pictures, no ffmpeg and
//!   no reader thread on its side. Frames keep the studio's numbers, which
//!   `agentzero-play` reports back with its action.
//!
//! ```text
//! uv run agentzero-play --checkpoint <ckpt> --video <file> [--start-s S] [--end-s E] --realtime --dry-run --json [--cpu]
//! uv run agentzero-play --checkpoint <ckpt> --shared-frames --dry-run --json [--cpu]
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
//! **Every run is recorded** while "Record bot runs" is on (the default,
//! `record_bot_runs` in the studio's state file): letting it play starts a
//! session as the Studio's Record does (`<prefix>bot-<stamp>/`: the video
//! with sound, and in `controller.bin` what reached the Switch, the mix),
//! which stops when the play ends; a session the Studio is recording
//! already (`allow_recording`) takes the run instead. `session.json` marks
//! it with `bot` ([`crate::studio::BotRecord`]: each play's checkpoint,
//! limits, start, end and why, and every **takeover**), and
//! `agentzero.jsonl` keeps the policy's actions, one line each: what it
//! wanted (`send`, its `button_probs`), what was `sent` after the limits
//! (null while not playing), `t_ms` and the frame's `captured_ms`, so a
//! timeline can show the bot's wish, the line sent and the person's
//! corrections. A takeover ([`Takeover`]) is seen in the proxy's frames,
//! which the bot reads as a [`Dumper`]: a frame with buttons no line sent
//! lately pressed, or a stick or the gyro equal to no such line's (the
//! proxy writes a line's values as they are, so a different value is the
//! controller's: pushed past the deadzone, or turning), starts one at the
//! frame's time; frames without a person's input end it after
//! [`TAKEOVER_GAP`]. Times are host Unix ms like the session's markers.
//! A run whose recording cannot start is not played ([`Ended::Recording`]).
//!
//! The bot also measures a person's fastest tapping of ZR from the proxy's
//! frames ([`limits::Tapping`]), for the page to set the cap from.
//!
//! **The loop's latency** (live), every moment on `CLOCK_MONOTONIC`
//! ([`video::mono_ns`], Python's `time.monotonic` too): from the capture
//! card's timestamp of a frame to its action written to the replay port.
//! The frame's hand-off, until the policy took it: the grabber (until ffmpeg
//! wrote it: for the capture card the USB transfer, the grabber's decoding,
//! fitting and scaling), the pipe into the studio, the shared memory (in its
//! slot, announced) and the wait (the model busy with the frame before, the
//! policy waking up); then the upload onto the model's device, the model,
//! and the send (the line back to the studio and onto the socket; while not
//! sending, until the studio has it). The page shows each as median and
//! 99th percentile over [`WINDOW`] actions.
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
//! - `POST record` `{"enabled": bool}`: record the runs, kept there too;
//!   `status` also answers with `record` (whether, and the session)
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
use crate::dump::{Dumper, Frame, unix_ms};
use crate::objects::write_atomic;
use crate::recorder::RecorderState;
use crate::replay::Action;
use crate::stream::LinkStats;
use crate::studio::{BotStart, Studio};
use crate::v4l2::YUYV;
use crate::video::{self, POLICY_MAX_BYTES, PolicySink, PolicyTimes, mono_ns, mono_to_unix_us};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use core::time::Duration;
use gameplay_data::labels::{self, Label};
use limits::{Limiter, Limits, Tapping, TappingStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::watch;
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// The policy checkpoints' folder under AgentZero's `runs/` (one level down,
/// so the IDM's list does not show them)
const POLICY_RUNS: &str = "policy";

/// A policy's stored runs are named `policy-<checkpoint>`, beside the IDM's
const STORED_PREFIX: &str = "policy-";

/// Frames per second the policy works at (the grabber's policy frames)
pub const FPS: f64 = video::POLICY_FPS as f64;

/// Actions the timings are taken over (10 s at 30 fps)
const WINDOW: usize = 300;

/// The times of the frames published lately, kept to match the policy's
/// actions to them
const HANDED: usize = 256;

/// Slots in the ring of shared frames: a frame stays this many frames in
/// its slot (133 ms at 30 fps), long enough for the policy to copy it out
pub const SLOTS: usize = 4;

/// The first bytes of the shared frames; the layout (AgentZero's
/// `SharedFrames` reads it): magic, then little-endian `u32` version,
/// slots, the frames' fourcc (`YUYV`), 0, then `u64` bytes from one slot to
/// the next and where slot 0 starts ([`SHARED_DATA`]); at [`SLOT_NUMBERS`],
/// a `u64` per slot: the number of the frame in it, all ones while it is
/// written. Each frame's size comes with its notice.
const SHARED_MAGIC: &[u8; 8] = b"PCFRAME1";

/// The layout's version
const SHARED_VERSION: u32 = 2;

/// Where the slots' numbers start
const SLOT_NUMBERS: u64 = 64;

/// Where slot 0 starts: the header takes a page
const SHARED_DATA: u64 = 4096;

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

/// A frame may show any line sent this long before it reached the studio:
/// the proxy's frames come a little after it read them, in bursts, and a
/// line takes a moment to get there
const RECENT: Duration = Duration::from_millis(300);

/// A person's input seen again within this long (ms of the frames' clock)
/// continues a takeover rather than starting another: taps come 100 ms or
/// more apart
pub const TAKEOVER_GAP: u64 = 250;

/// Keys of an action line that stay in a stored prediction
const KEPT_EXTRA: [&str; 4] = ["button_probs", "camera_turn", "seen", "model_ms"];

/// Keys of an action line the page's `agent` messages and the recorded
/// `agentzero.jsonl` leave out
const TICK_DROPPED: [&str; 6] = [
    "event",
    "arrived_ms",
    "ready_ms",
    "taken_ns",
    "placed_ns",
    "ready_ns",
];

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
    /// The live capture is read by the studio itself (V4L2), not ffmpeg
    pub direct: bool,
    /// Latency over the last actions (see the module docs): `handoff` with
    /// its parts `grabber`, `pipe`, `shared` and `wait`, then `upload`,
    /// `model`, `send` and `total` (live), `age` (a video's frame from its
    /// time to its action), each `{median, p99}` ms, and `rate` (actions per
    /// second)
    pub timings: Value,
    /// The stored run's checkpoint name, once kept
    pub stored: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

/// One action's times in ms, for [`Status::timings`]
#[derive(Clone, Copy, Debug, Default, Serialize)]
struct Sample {
    /// When the studio had the action ([`mono_ns`] in ms)
    #[serde(skip)]
    at: f64,
    /// The frame's hand-off, until the policy took it, and its parts (see
    /// the module docs): until ffmpeg wrote it (`grabber`), the studio had
    /// it (`pipe`), it was in shared memory and announced (`shared`), the
    /// policy took it (`wait`)
    handoff: Option<f64>,
    grabber: Option<f64>,
    pipe: Option<f64>,
    shared: Option<f64>,
    wait: Option<f64>,
    /// Onto the model's device
    upload: Option<f64>,
    model: f64,
    send: Option<f64>,
    total: Option<f64>,
    /// A video's frame, from its time to its action
    age: Option<f64>,
}

impl Sample {
    /// A live action's stages, from its frame's times and the policy's
    /// moments (all [`mono_ns`]): it took the frame, had it on the model's
    /// device, had the action; the studio `sent` it (or had it)
    fn live(frame: &Published, taken: u64, placed: u64, ready: u64, sent: u64) -> Self {
        let ms = |from: u64, to: u64| Some((to as f64 - from as f64) / 1e6);
        let times = &frame.times;
        Self {
            at: sent as f64 / 1e6,
            handoff: ms(times.captured, taken),
            grabber: ms(times.captured, times.emitted),
            pipe: ms(times.emitted, times.read),
            shared: ms(times.read, frame.published),
            wait: ms(frame.published, taken),
            upload: ms(taken, placed),
            model: (ready as f64 - placed as f64) / 1e6,
            send: ms(ready, sent),
            total: ms(times.captured, sent),
            age: None,
        }
    }
}

/// Median and 99th percentile (nearest rank) of some ms
fn spread(mut values: Vec<f64>) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_by(f64::total_cmp);
    let rank = |share: f64| {
        values[((share * values.len() as f64).ceil() as usize).clamp(1, values.len()) - 1]
    };
    json!({ "median": rank(0.5), "p99": rank(0.99) })
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
        "grabber": pick(|s| s.grabber),
        "pipe": pick(|s| s.pipe),
        "shared": pick(|s| s.shared),
        "wait": pick(|s| s.wait),
        "upload": pick(|s| s.upload),
        "model": pick(|s| Some(s.model)),
        "send": pick(|s| s.send),
        "total": pick(|s| s.total),
        "age": pick(|s| s.age),
        "rate": rate,
    })
}

/// A frame published to the policy: its number, its times, and when it
/// was in shared memory and announced ([`mono_ns`])
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Published {
    number: u64,
    times: PolicyTimes,
    published: u64,
}

/// The live capture's frames for the policy in shared memory (see the
/// module docs and [`SHARED_MAGIC`] for the layout): a memfd the policy
/// gets as its fd 3, and a notice per frame on its stdin, 40 bytes: the
/// frame's number, its slot, when it was captured and when it was
/// published ([`mono_ns`]) as little-endian `u64`s, then its width and
/// height as `u32`s. Frames are YUYV, the capture card's own (AgentZero
/// scales them). Nothing waits: a frame goes into the next slot, and a
/// notice the pipe has no room for is dropped (the policy is not reading;
/// it takes the newest anyway).
pub struct SharedFrames {
    memory: File,
    /// The policy's stdin, once it is ready for frames
    notices: Mutex<Option<File>>,
    next: AtomicU64,
    /// The frames published lately, to match the policy's actions to
    published: Mutex<VecDeque<Published>>,
}

/// Bytes from one slot to the next: the largest frame, whole pages
const SLOT_BYTES: u64 = (POLICY_MAX_BYTES as u64).div_ceil(4096) * 4096;

/// Bytes of a notice
const NOTICE_BYTES: usize = 40;

impl SharedFrames {
    /// Room for [`SLOTS`] frames, with the header written
    pub fn new() -> Result<Self> {
        // SAFETY: memfd_create takes a NUL-terminated name and returns a
        // fresh descriptor we then own
        let fd = unsafe { libc::memfd_create(c"procon-policy-frames".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("cannot make shared memory");
        }
        // SAFETY: a fresh descriptor from memfd_create
        let memory = unsafe { File::from_raw_fd(fd) };
        memory.set_len(SHARED_DATA + SLOTS as u64 * SLOT_BYTES)?;
        let mut header = Vec::with_capacity(40);
        header.extend_from_slice(SHARED_MAGIC);
        for value in [SHARED_VERSION, SLOTS as u32, YUYV, 0] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        for value in [SLOT_BYTES, SHARED_DATA] {
            header.extend_from_slice(&value.to_le_bytes());
        }
        memory.write_all_at(&header, 0)?;
        // No slot holds a frame yet
        for slot in 0..SLOTS as u64 {
            memory.write_all_at(&u64::MAX.to_le_bytes(), SLOT_NUMBERS + 8 * slot)?;
        }
        Ok(Self {
            memory,
            notices: Mutex::default(),
            next: AtomicU64::new(0),
            published: Mutex::default(),
        })
    }

    /// Announce frames on `notices` (the policy's stdin) from now on
    fn start(&self, notices: impl Into<OwnedFd>) {
        let notices = File::from(notices.into());
        // A notice the pipe has no room for is dropped, never waited for
        // SAFETY: fcntl on a descriptor we own
        unsafe {
            let flags = libc::fcntl(notices.as_raw_fd(), libc::F_GETFL);
            libc::fcntl(notices.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        *self.notices.lock().unwrap() = Some(notices);
    }

    /// The frame numbered `number`, if published lately
    fn find(&self, number: u64) -> Option<Published> {
        let published = self.published.lock().unwrap();
        published.iter().rev().find(|p| p.number == number).copied()
    }
}

impl PolicySink for SharedFrames {
    fn frame(&self, frame: &[u8], (width, height): (u32, u32), times: PolicyTimes) {
        if frame.len() as u64 > SLOT_BYTES || frame.len() != (width * height * 2) as usize {
            return;
        }
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        let slot = number % SLOTS as u64;
        let slot_number = SLOT_NUMBERS + 8 * slot;
        // Marked as being written, then the frame, then its number: a
        // policy still copying the frame there before sees the change
        let written = self
            .memory
            .write_all_at(&u64::MAX.to_le_bytes(), slot_number)
            .and_then(|()| {
                self.memory
                    .write_all_at(frame, SHARED_DATA + slot * SLOT_BYTES)
            })
            .and_then(|()| self.memory.write_all_at(&number.to_le_bytes(), slot_number));
        if let Err(e) = written {
            log::warn!("Cannot write a frame for AgentZero: {e}");
            return;
        }
        let published = mono_ns();
        {
            let mut log = self.published.lock().unwrap();
            if log.len() >= HANDED {
                log.pop_front();
            }
            log.push_back(Published {
                number,
                times,
                published,
            });
        }
        let mut notice = [0u8; NOTICE_BYTES];
        for (i, value) in [number, slot, times.captured, published]
            .into_iter()
            .enumerate()
        {
            notice[8 * i..8 * i + 8].copy_from_slice(&value.to_le_bytes());
        }
        notice[32..36].copy_from_slice(&width.to_le_bytes());
        notice[36..40].copy_from_slice(&height.to_le_bytes());
        if let Some(stdin) = self.notices.lock().unwrap().as_mut() {
            // All of it or none (a pipe write under 4 KiB is atomic)
            let _ = stdin.write(&notice);
        }
    }
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
    /// `--shared-frames`: the live capture's frames from the studio
    /// (without it, only videos run)
    pub shared: bool,
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
        status.direct = status.live && self.studio.video.direct();
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
            Ok(output) if output.status.success() => {
                let help = String::from_utf8_lossy(&output.stdout);
                PlayCapabilities {
                    json: help_has(&help, "--json"),
                    shared: help_has(&help, "--shared-frames"),
                    error: None,
                }
            }
            Ok(output) => PlayCapabilities {
                json: false,
                shared: false,
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
                shared: false,
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
        let capabilities = self.capabilities(false);
        ensure!(
            capabilities.json,
            "this agentzero-play has no --json yet; update AgentZero, then Recheck"
        );
        ensure!(
            capabilities.shared || request.source.is_some(),
            "this agentzero-play cannot take the live capture's frames yet (--shared-frames); \
             update AgentZero, then Recheck"
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
                args.push("--shared-frames".to_string());
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
            direct: false,
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
                // No more frames for it, nothing for the proxy
                online.studio.video.set_policy_sink(None);
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
        // The live capture's frames reach it in shared memory, its fd 3
        let shared = if live {
            Some(Arc::new(SharedFrames::new()?))
        } else {
            None
        };
        let (stdin, stdout, stderr) = {
            let mut slot = self.child.lock().unwrap();
            // Under the lock: a stop before this point means nothing runs,
            // one after it finds the process
            ensure!(!self.stop.load(Ordering::Relaxed), "stopped");
            let mut command = Command::new("uv");
            if let Some(shared) = &shared {
                video::inherit_as_fd3(&mut command, &shared.memory);
            }
            let mut child = command
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
                    if let (Some(stdin), Some(shared)) = (stdin.take(), &shared)
                        && !self.stop.load(Ordering::Relaxed)
                    {
                        shared.start(stdin);
                        let sink: Arc<dyn PolicySink> = shared.clone();
                        self.studio.video.set_policy_sink(Some(sink));
                    }
                }
                Some("action") => {
                    if let Err(e) = self.on_action(value, shared.as_deref()) {
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

    /// One action line: timings, the bot, the page, the predictions; for
    /// the live capture, `shared` holds its frames' times
    fn on_action(&self, value: Value, shared: Option<&SharedFrames>) -> Result<()> {
        let got = mono_ns();
        let number = |key: &str| value[key].as_f64();
        let moment = |key: &str| value[key].as_u64();
        let seen = value["seen"].as_u64().context("no seen")?;
        let (id, live) = self
            .run
            .lock()
            .unwrap()
            .as_ref()
            .map_or((0, false), |run| (run.status.id, run.status.live));
        let mut sample = Sample {
            at: got as f64 / 1e6,
            model: number("model_ms").unwrap_or_default(),
            ..Default::default()
        };
        // What went to the Switch, after the bot's limits
        let mut sent = None;
        if live {
            let action = Action::parse(&value["send"].to_string()).context("bad send")?;
            sent = self.studio.bot.send(&action);
            let done = sent.as_ref().map_or(got, |(at, _)| *at);
            let frame = shared.and_then(|shared| shared.find(seen));
            if let (Some(frame), Some(taken), Some(placed), Some(ready)) = (
                frame,
                moment("taken_ns"),
                moment("placed_ns"),
                moment("ready_ns"),
            ) {
                sample = Sample::live(&frame, taken, placed, ready, done);
            }
            // The recorded run's log: what the policy wanted, what was sent
            self.studio.bot_action(|| {
                let mut line = value.clone();
                if let Some(fields) = line.as_object_mut() {
                    for key in TICK_DROPPED {
                        fields.remove(key);
                    }
                    fields.insert("t_ms".into(), json!(unix_ms()));
                    fields.insert(
                        "captured_ms".into(),
                        json!(frame.map(|f| mono_to_unix_us(f.times.captured) / 1000)),
                    );
                    fields.insert("sent".into(), json!(sent.as_ref().map(|(_, line)| line)));
                }
                line
            });
        } else if let (Some(arrived), Some(ready)) = (number("arrived_ms"), number("ready_ms")) {
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
            for key in TICK_DROPPED {
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
            // Each stage of this action's loop (live), in ms
            if live {
                fields.insert("stages".into(), json!(sample));
            }
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
        self.studio.video.set_policy_sink(None);
        self.studio.bot.release(Ended::Stopped);
        self.end_bot_play();
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
        self.studio.bot.play(seconds)?;
        if self.studio.record_bot_runs() {
            let start = BotStart {
                checkpoint: status.checkpoint,
                cpu: status.cpu,
                seconds,
                limits: self.studio.bot.limits(),
            };
            if let Err(e) = self.studio.bot_play_started(start) {
                self.studio.bot.release(Ended::Recording);
                return Err(e.context(
                    "the run could not be recorded (untick \"Record bot runs\" to play unrecorded)",
                ));
            }
        }
        Ok(())
    }

    /// The bot's play ended: its end, why, and the takeovers seen go to the
    /// session recording it, which stops when it was the bot's own
    fn end_bot_play(&self) {
        if !self.studio.bot_play_open() {
            return;
        }
        let status = self.studio.bot.status();
        let takeovers = self.studio.bot.take_takeovers();
        if let Err(e) = self
            .studio
            .bot_play_ended(status.ended, status.sent, takeovers)
        {
            log::warn!("Cannot finish the bot run's record: {:#}", e);
        }
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
            if !self.studio.bot.status().playing {
                self.end_bot_play();
            }
            // Its own recording of the run is no reason to stop
            let recording = self.studio.recorder.status().state != RecorderState::Idle
                && !self.studio.recording_bot_run();
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

// ------------------------------------------------------------------ the bot

/// The bot's hold on the proxy's replay port while AgentZero plays, with
/// what it may press ([`Limits`]); a [`Dumper`] of the frames the proxy
/// streams, for measuring a person's tapping. See the module docs.
#[derive(Clone)]
pub struct Bot(Arc<BotInner>);

struct BotInner {
    /// `host:port` of the proxy's replay port
    address: String,
    /// The proxy link: its clock offset puts the frames on the host's clock
    link: Arc<LinkStats>,
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
    /// The lines sent within [`RECENT`] (when, the line), which the proxy's
    /// frames are compared with for a person's input
    recent: VecDeque<(Instant, Action)>,
    /// A person's input over the bot's, being seen
    open: Option<Takeover>,
    /// The takeovers seen since it was let play, until taken
    takeovers: Vec<Takeover>,
}

/// A stretch of the proxy's frames in which a person's input showed over
/// the bot's while it played (see the module docs): buttons the bot did not
/// press, a stick pushed past the proxy's deadzone, the controller turned.
/// Times in host Unix ms, as the session's markers; frames without a
/// person's input end it after [`TAKEOVER_GAP`]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Takeover {
    /// The first frame with the person's input
    pub t_start_ms: u64,
    /// The last one
    pub t_end_ms: u64,
    /// `buttons`, `left_stick`, `right_stick`, `gyro`: what was the person's
    pub channels: BTreeSet<String>,
    /// The buttons the person pressed
    pub buttons: BTreeSet<String>,
}

/// What of `report` (a frame of the proxy) is a person's rather than any of
/// the `lines` sent lately, by the proxy's mix (see [`crate::replay`]):
/// buttons no line pressed, and a stick or the gyro equal to no line's,
/// since a line's values are written as they are (a line without a stick
/// or gyro leaves it to the controller: not a takeover). Nothing before the
/// first line
fn persons_input<'a>(
    report: &Action,
    lines: impl Iterator<Item = &'a Action>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut buttons: BTreeSet<String> = report.buttons.iter().flatten().cloned().collect();
    // Differs from every line's, so far
    let mut left = report.left_stick.is_some();
    let mut right = report.right_stick.is_some();
    let mut gyro = report.gyro.is_some();
    let mut any = false;
    for line in lines {
        any = true;
        for pressed in line.buttons.iter().flatten() {
            buttons.remove(pressed);
        }
        left &= line.left_stick.is_some() && line.left_stick != report.left_stick;
        right &= line.right_stick.is_some() && line.right_stick != report.right_stick;
        gyro &= line.gyro.is_some() && line.gyro != report.gyro;
    }
    if !any {
        return (BTreeSet::new(), BTreeSet::new());
    }
    let mut channels = BTreeSet::new();
    for (name, taken) in [
        ("buttons", !buttons.is_empty()),
        ("left_stick", left),
        ("right_stick", right),
        ("gyro", gyro),
    ] {
        if taken {
            channels.insert(name.to_string());
        }
    }
    if !channels.contains("buttons") {
        buttons.clear();
    }
    (channels, buttons)
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
    /// The recording of the run could not start
    Recording,
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
    /// Takeovers seen since it was let play
    pub takeovers: usize,
}

impl BotState {
    /// A frame of the proxy at `at_ms` (host clock) while it plays: a
    /// person's input over the lines sent lately opens or continues a
    /// takeover, a frame without one ends it after [`TAKEOVER_GAP`]
    fn note_frame(&mut self, report: &Action, at_ms: u64) {
        let now = Instant::now();
        while self.recent.len() > 1
            && self
                .recent
                .front()
                .is_some_and(|(sent, _)| now.duration_since(*sent) > RECENT)
        {
            self.recent.pop_front();
        }
        if self
            .open
            .as_ref()
            .is_some_and(|open| at_ms.saturating_sub(open.t_end_ms) > TAKEOVER_GAP)
        {
            self.close_takeover();
        }
        let (channels, buttons) = persons_input(report, self.recent.iter().map(|(_, line)| line));
        if channels.is_empty() {
            return;
        }
        match self.open.as_mut() {
            Some(open) => {
                open.t_end_ms = open.t_end_ms.max(at_ms);
                open.channels.extend(channels);
                open.buttons.extend(buttons);
            }
            None => {
                self.open = Some(Takeover {
                    t_start_ms: at_ms,
                    t_end_ms: at_ms,
                    channels,
                    buttons,
                })
            }
        }
    }

    fn close_takeover(&mut self) {
        if let Some(open) = self.open.take() {
            self.takeovers.push(open);
        }
    }

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
    /// may press what `limits` allow; `link` is the proxy's stream, whose
    /// clock offset dates the takeovers
    pub fn new(address: String, limits: Limits, link: Arc<LinkStats>) -> Self {
        Self(Arc::new(BotInner {
            address,
            link,
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
        state.recent.clear();
        state.open = None;
        state.takeovers.clear();
        log::info!("AgentZero plays through {address} for {seconds} s");
        Ok(())
    }

    /// Send one action, its buttons held to the limits, unless not playing;
    /// answers with when it was written ([`mono_ns`]) and the line written
    pub fn send(&self, action: &Action) -> Option<(u64, Action)> {
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
        state.recent.push_back((now, line.clone()));
        Some((mono_ns(), line))
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
        state.recent.clear();
        state.close_takeover();
        log::info!("AgentZero stopped playing ({reason:?})");
    }

    /// The takeovers seen since it was let play, for the session's record;
    /// one still open (while it plays) stays
    pub fn take_takeovers(&self) -> Vec<Takeover> {
        core::mem::take(&mut self.lock().takeovers)
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
            takeovers: state.takeovers.len() + usize::from(state.open.is_some()),
        }
    }
}

/// The proxy's frames: a person's input over the bot's while it plays, and
/// their presses while their tapping is measured
impl Dumper for Bot {
    fn dump(&mut self, frame: &Frame) -> Result<()> {
        let mut state = self.lock();
        if state.stream.is_none() && !state.measuring() {
            return Ok(());
        }
        let Some(report) = Action::from_report(frame.payload()) else {
            return Ok(());
        };
        if state.stream.is_some() {
            let offset = self.0.link.clock_offset_ms.load(Ordering::Relaxed);
            let at_ms = frame.timestamp_ms.saturating_add_signed(offset);
            state.note_frame(&report, at_ms);
        }
        let Some(tapping) = state.tapping.as_mut().filter(|t| t.active()) else {
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
                "record": self.studio.bot_record_status(),
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
            "record" => {
                let enabled = serde_json::from_slice::<Value>(body)
                    .ok()
                    .and_then(|v| v["enabled"].as_bool())
                    .context("give enabled")?;
                self.studio.set_record_bot_runs(enabled)?;
                Ok(self.studio.bot_record_status())
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
        let s = spread((1..=200).map(f64::from).collect());
        assert_eq!(s["median"], 100.0);
        assert_eq!(s["p99"], 198.0);
        let one = spread(vec![7.0]);
        assert_eq!(
            (one["median"].as_f64(), one["p99"].as_f64()),
            (Some(7.0), Some(7.0))
        );
    }

    /// A little-endian `u64` of the shared memory
    fn word(memory: &File, at: u64) -> u64 {
        let mut bytes = [0u8; 8];
        memory.read_exact_at(&mut bytes, at).unwrap();
        u64::from_le_bytes(bytes)
    }

    #[test]
    fn frames_go_into_shared_memory_with_a_notice_each() {
        use std::io::Read;
        let shared = SharedFrames::new().unwrap();
        let memory = &shared.memory;
        // The header AgentZero reads
        let mut magic = [0u8; 8];
        memory.read_exact_at(&mut magic, 0).unwrap();
        assert_eq!(&magic, SHARED_MAGIC);
        assert_eq!(word(memory, 8), 2 | (SLOTS as u64) << 32);
        assert_eq!(word(memory, 16), u64::from(u32::from_le_bytes(*b"YUYV")));
        assert_eq!([word(memory, 24), word(memory, 32)], [4_149_248, 4096]);
        assert_eq!(word(memory, SLOT_NUMBERS), u64::MAX);
        // Frames before the policy is ready go in, unannounced
        let times = |n: u64| PolicyTimes {
            captured: 1000 + n,
            emitted: 2000 + n,
            read: 3000 + n,
        };
        let size = (1280, 720);
        let bytes = 1280 * 720 * 2;
        shared.frame(&vec![9u8; bytes], size, times(0));
        let (mut notices, writer) = std::io::pipe().unwrap();
        shared.start(writer);
        for n in 1..6u64 {
            shared.frame(&vec![n as u8; bytes], size, times(n));
        }
        // A frame of another size than it says, or too large, is not handed on
        shared.frame(&vec![0u8; bytes - 2], size, times(6));
        shared.frame(&vec![0u8; 2560 * 1440 * 2], (2560, 1440), times(7));
        let mut notice = [0u8; NOTICE_BYTES * 5];
        notices.read_exact(&mut notice).unwrap();
        for (i, n) in (1..6u64).enumerate() {
            let notice = &notice[NOTICE_BYTES * i..NOTICE_BYTES * (i + 1)];
            let field = |k: usize| u64::from_le_bytes(notice[8 * k..8 * k + 8].try_into().unwrap());
            assert_eq!(
                (field(0), field(1), field(2)),
                (n, n % SLOTS as u64, 1000 + n)
            );
            assert_eq!(shared.find(n).unwrap().published, field(3));
            assert_eq!(field(4), 1280 | 720 << 32);
        }
        assert_eq!(shared.next.load(Ordering::Relaxed), 6);
        // Each slot holds the newest frame of its turn, numbered
        for (slot, n) in [(0, 4u64), (1, 5), (2, 2), (3, 3)] {
            assert_eq!(word(memory, SLOT_NUMBERS + 8 * slot), n);
            let mut pixel = [0u8; 1];
            memory
                .read_exact_at(&mut pixel, SHARED_DATA + slot * SLOT_BYTES + 1234)
                .unwrap();
            assert_eq!(pixel[0], n as u8);
        }
        let frame = shared.find(3).unwrap();
        assert_eq!(frame.times, times(3));
        assert!(frame.published >= frame.times.read);
    }

    #[test]
    fn a_policy_not_reading_never_holds_the_frames_up() {
        let shared = SharedFrames::new().unwrap();
        let (_notices, writer) = std::io::pipe().unwrap();
        // The smallest pipe: room for 102 notices
        // SAFETY: fcntl on a descriptor we own
        unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
        shared.start(writer);
        let frame = vec![0u8; 640 * 360 * 2];
        for _ in 0..200 {
            shared.frame(&frame, (640, 360), PolicyTimes::default());
        }
        assert_eq!(shared.next.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn a_live_sample_splits_the_loop() {
        let frame = Published {
            number: 7,
            times: PolicyTimes {
                captured: 1_000_000,
                emitted: 3_000_000,
                read: 3_500_000,
            },
            published: 3_600_000,
        };
        let s = Sample::live(&frame, 4_600_000, 4_800_000, 9_800_000, 10_300_000);
        let parts = [s.grabber, s.pipe, s.shared, s.wait].map(Option::unwrap);
        assert_eq!(parts, [2.0, 0.5, 0.1, 1.0]);
        assert!((parts.iter().sum::<f64>() - s.handoff.unwrap()).abs() < 1e-9);
        assert_eq!((s.upload, s.model, s.send), (Some(0.2), 5.0, Some(0.5)));
        assert_eq!(s.total, Some(9.3));
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
        let mut bot = Bot::new(address, Limits::default(), Arc::default());
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

    /// A frame of the proxy: `state` as the mix left it, read at `at_ms`
    fn mixed(state: &str, at_ms: u64) -> Frame {
        let mut report = [0u8; 64];
        report[0] = 0x30;
        action(state).apply(&mut report);
        Frame::new(at_ms, 0, &report)
    }

    #[test]
    fn a_persons_input_over_the_bots_is_a_takeover() {
        let (address, _proxy) = replay_port();
        let link = Arc::new(LinkStats::default());
        link.clock_offset_ms.store(1_000_000, Ordering::Relaxed);
        let mut bot = Bot::new(address, Limits::default(), Arc::clone(&link));
        let wants = |buttons: &str, x: u16| {
            action(&format!(
                r#"{{"buttons": {buttons}, "left_stick": [{x}, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}}"#
            ))
        };
        // Before it plays, frames are the person's alone: not takeovers
        bot.dump(&mixed(r#"{"buttons": ["a"]}"#, 1_000)).unwrap();
        bot.play(30.0).unwrap();
        // Nothing sent yet: nothing to compare with
        bot.dump(&mixed(r#"{"buttons": ["a"]}"#, 1_010)).unwrap();
        bot.send(&wants(r#"["y"]"#, 2100)).unwrap();
        bot.send(&wants(r#"["y"]"#, 2120)).unwrap();
        // The bot's own line as the proxy applied it (the frame may show
        // the line before the last): not a takeover
        for (n, x) in [(0, 2120), (1, 2100), (2, 2120)] {
            bot.dump(
                &mixed(
                    &format!(
                        r#"{{"buttons": ["y"], "left_stick": [{x}, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}}"#
                    ),
                    2_000 + n,
                ),
            )
            .unwrap();
        }
        assert_eq!(bot.status().takeovers, 0);
        // The person presses A over Y, then pushes the left stick; a lull
        // under TAKEOVER_GAP joins the two; the right stick and gyro left
        // as the line's are the bot's
        for (state, at) in [
            (
                r#"{"buttons": ["y", "a"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#,
                2_100,
            ),
            (
                r#"{"buttons": ["y", "a"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#,
                2_116,
            ),
            (
                r#"{"buttons": ["y"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#,
                2_132,
            ),
            (
                r#"{"buttons": ["y"], "left_stick": [3900, 1000], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#,
                2_300,
            ),
            (
                r#"{"buttons": ["y"], "left_stick": [3900, 1000], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#,
                2_316,
            ),
        ] {
            bot.dump(&mixed(state, at)).unwrap();
        }
        assert_eq!(bot.status().takeovers, 1);
        // Turning the controller, TAKEOVER_GAP after the last: another
        bot.dump(&mixed(r#"{"buttons": ["y"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#, 2_400)).unwrap();
        bot.dump(&mixed(r#"{"buttons": ["y"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [900, -90, 31]}"#, 2_600)).unwrap();
        bot.dump(&mixed(r#"{"buttons": ["y"], "left_stick": [2120, 1847], "right_stick": [2100, 2100], "gyro": [19, -90, 31]}"#, 2_616)).unwrap();
        assert_eq!(bot.status().takeovers, 2);
        // The stop closes the open one; times on the host's clock
        bot.release(Ended::You);
        let takeovers = bot.take_takeovers();
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(
            takeovers,
            vec![
                Takeover {
                    t_start_ms: 1_002_100,
                    t_end_ms: 1_002_316,
                    channels: set(&["buttons", "left_stick"]),
                    buttons: set(&["a"]),
                },
                Takeover {
                    t_start_ms: 1_002_600,
                    t_end_ms: 1_002_600,
                    channels: set(&["gyro"]),
                    buttons: BTreeSet::new(),
                },
            ]
        );
        assert!(bot.take_takeovers().is_empty());
        // Frames after the stop are the person's alone again
        bot.dump(&mixed(r#"{"buttons": ["a"]}"#, 3_000)).unwrap();
        assert_eq!(bot.status().takeovers, 0);
    }

    #[test]
    fn a_persons_tapping_is_measured_from_the_proxys_frames() {
        let mut bot = Bot::new("127.0.0.1:9".to_string(), Limits::default(), Arc::default());
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
