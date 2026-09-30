//! Pipeline app backend: the GPU and the experiment queue, live
//!
//! This machine trains AgentZero's models while agents (and the user) plan
//! what to run next. The plan is not in code, since it changes all the
//! time: it is a small JSON file the agents keep, `[pipeline] queue`, by
//! default `runs/queue.json` in the AgentZero folder (`[predictor]
//! agentzero`), written with AgentZero's `agentzero-queue` (its README has
//! the format). Each entry is one step of an experiment ([`Entry`]): what
//! it is, why (the question it answers), its status (`queued`, `running`,
//! `done`, `failed`, `paused`), its `priority` (queued entries run highest
//! first, ties in file order: [`run_order`]), the entries it waits for
//! (`after`), where it can run (`device`), its processes (`pgid`, `pid`, or
//! `match`, a piece of its command line), its `log` and `run_dir`, its
//! times, and once it is over a one-line `result_summary` and the
//! `next_step`. Relative paths start at the queue file's folder.
//!
//! Two GPUs take the entries: this host's (`device: gpu:linux`) and the
//! win11 VM's (`gpu:win11`; `gpu` is either, `cpu` none). AgentZero's win11
//! runner (`agentzero-win11 run`) feeds the VM: it marks the entries it
//! takes `host: win11`, writes the VM's GPU to `win11/gpu.json` beside the
//! queue file every 10 s ([`Remote`]), and copies each job's log and
//! metrics back into the same paths here every minute. An entry on the VM
//! has no process here: whether it runs is what that file says, while it
//! is fresh ([`REMOTE_STALE`]). Each runner takes the first queued entry
//! that fits its device and waits for nothing ([`next_for`]).
//!
//! The page reorders the waiting entries by dragging them: `POST order`
//! writes their priorities into the file, holding the lock the helper takes
//! (`<queue>.lock`, an exclusive `flock`) and writing atomically, with
//! every other field kept as it was ([`write_order`]). Nothing else here
//! writes.
//!
//! A thread samples the machine every [`SAMPLE_EVERY`], read-only, and
//! keeps [`KEEP`] of it in memory for the timeline ([`Sample`]): this
//! host's GPU through `nvidia-smi` (utilization, memory, temperature,
//! power, and each compute process's memory), the VM's GPU from its file
//! (unknown, never 0, while the file is stale), CPU, load and memory from
//! `/proc`, the processes of each entry here with the CPU they took, and
//! each live entry's progress from its run folder (`metrics.jsonl` rows
//! with `step` and `split`, `args.json` with `steps`) or else its log (the
//! last `N/M` in it), with the rate and ETA it saw. A run whose trainer
//! wrote its closing rows ([`END_SPLITS`]; for a run known only by its
//! log, printed that it stops, [`END_LINES`]) has ended, early when before
//! its last step; a live entry whose step has not moved for [`STALL`] does
//! something else now (an evaluation after training, say). Neither has an
//! ETA.
//!
//! Each sample is also appended to a log on this machine,
//! `pipeline-gpu.jsonl` in the local cache (`cuttlefish::store::cache_dir`,
//! never the synced knowledge folder), with the entries seen running then
//! and the CPU each took ([`Sample::line`]), so a restart keeps the
//! timeline: the sampler reads back the last [`KEEP`] when it starts. The
//! log is rewritten at start and every hour ([`compact`]): samples older
//! than [`KEEP`] thinned to one a minute, those older than
//! [`HISTORY_KEEP`] dropped, a torn last line skipped.
//!
//! Endpoints under `/api/pipeline/`:
//!
//! - `GET state[?since=<ms>]`: the machine now, both GPUs and their
//!   processes, the queue with each entry's processes, progress and
//!   runner, and with `since` the samples taken after it
//! - `GET timeline?minutes=<n>`: the samples of the last `n` minutes (at
//!   most [`KEEP`]), averaged down to [`MAX_POINTS`], and when each entry
//!   was seen running
//! - `GET run?id=<entry>`: the loss curve and validation rows of an
//!   entry's run folder, and its `args.json`
//! - `GET log?id=<entry>`: the last lines of an entry's log
//! - `POST order` with `{"order": [ids]}`: the waiting entries in that
//!   order, top first (see [`write_order`]); answers with `state`
//!
//! Only files the queue names are read for `run` and `log`. Errors are
//! `{"error": "..."}` with status 400 (404 for an unknown endpoint or
//! entry, 409 when the queue file stays locked).

use crate::inspect::objects::write_atomic;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail};
use core::time::Duration;
use procon::dump::unix_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};

/// How often the machine is sampled
pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);

/// How much of the samples the timeline keeps
pub const KEEP: Duration = Duration::from_secs(12 * 3600);

/// How much of the samples the log on disk keeps
pub const HISTORY_KEEP: Duration = Duration::from_secs(7 * 24 * 3600);

/// Width of the buckets samples older than [`KEEP`] are averaged into on disk
const HISTORY_BUCKET_MS: u64 = 60_000;

/// How often the log on disk is compacted
const COMPACT_EVERY: Duration = Duration::from_secs(3600);

/// Most points in a timeline answer; longer windows are averaged down
pub const MAX_POINTS: usize = 720;

/// Most points of a loss curve sent to the page
const MAX_CURVE: usize = 500;

/// The numbers of a validation row the page shows: its loss, its scores,
/// and the copycat scores of AgentZero's policy (what it gets right beyond
/// repeating the action the frame seen shows: `keyframe_*` over the button
/// changes, `anticipation_*` as partial correlations given the present;
/// with the press, tolerant and half-second scores of its evaluation)
const VAL_KEYS: [&str; 20] = [
    "loss_total",
    "button_f1",
    "onset_f1",
    "turn_corr_x",
    "turn_corr_y",
    "gyro_corr",
    "stick_bin_accuracy",
    "keyframe_button_acc",
    "keyframe_onset_recall",
    "keyframe_release_recall",
    "anticipation_left_x",
    "anticipation_left_y",
    "anticipation_turn_x",
    "anticipation_turn_y",
    "press_f1",
    "frame_f1_tolerant",
    "onset_f1_wide",
    "hold_iou",
    "turn_corr_x_500ms",
    "turn_corr_y_500ms",
];

/// Splits of the rows a trainer writes once its loop is over, stopped
/// early or not: the policy's tuned `thresholds`, the IDM's `val-tuned`
pub const END_SPLITS: [&str; 2] = ["thresholds", "val-tuned"];

/// What the trainers print once their loop is over: why they stop early
/// (the policy's, the IDM's), and the line both print last
const END_LINES: [&str; 3] = [
    "no better loss in",
    "stopping: no new best in",
    "done; best validation loss",
];

/// A live entry whose step has not moved for this long does something else
/// now (an evaluation after training, a stuck loader): it has no ETA
pub const STALL: Duration = Duration::from_secs(5 * 60);

/// This host's name in the queue's devices (`gpu:linux`)
pub const LOCAL: &str = "linux";

/// The machine of the second GPU, the win11 VM: `host` of the entries its
/// runner takes, `gpu:win11` their device, `win11/gpu.json` beside the
/// queue file its GPU
pub const REMOTE_HOST: &str = "win11";

/// A remote GPU file older than this says nothing of now: its runner
/// writes it every 10 s, but not while it copies a job's data over
pub const REMOTE_STALE: Duration = Duration::from_secs(60);

/// Largest queue file read
const MAX_QUEUE_BYTES: u64 = 1 << 20;

/// Lines of a log sent to the page
const LOG_LINES: usize = 60;

/// Bytes read from the end of a log for its last lines and its progress
const LOG_TAIL_BYTES: u64 = 64 << 10;

/// How far back the rate of progress is measured
const RATE_WINDOW_MS: u64 = 10 * 60 * 1000;

/// A gap in an entry's activity longer than this starts a new span
const SPAN_GAP_MS: u64 = 4 * 5000;

/// How long `POST order` waits for the queue's lock
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// Largest request body
const BODY_LIMIT: u64 = 64 << 10;

/// The Pipeline's settings, from `[pipeline]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The queue file agents keep
    pub queue: PathBuf,
    /// The samples' log on disk, if any
    pub history: Option<PathBuf>,
    /// The file the win11 runner writes the VM's GPU to: `win11/gpu.json`
    /// beside the queue file, as AgentZero's helper reads it
    pub remote: PathBuf,
}

impl Settings {
    /// Settings from `[pipeline]`; relative paths start at `config_dir`,
    /// and the queue defaults to `runs/queue.json` in the AgentZero folder
    pub fn from_config(
        config: crate::config::PipelineConfig,
        config_dir: &Path,
        agentzero: &Path,
    ) -> Self {
        let queue = config.queue.map_or_else(
            || agentzero.join("runs").join("queue.json"),
            |queue| config_dir.join(queue),
        );
        let remote = queue
            .parent()
            .unwrap_or(Path::new("."))
            .join(REMOTE_HOST)
            .join("gpu.json");
        Self {
            queue,
            history: Some(cuttlefish::store::cache_dir().join("pipeline-gpu.jsonl")),
            remote,
        }
    }
}

// ------------------------------------------------------------------ queue

/// A timestamped remark on an entry
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Note {
    /// When, RFC 3339
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub text: String,
}

/// One step of an experiment in the queue file. The fields are written in
/// this order (the helper writes the same), and fields the lab does not
/// know are kept after them.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Entry {
    /// Unique, short (`d-f8-no`)
    pub id: String,
    /// What runs, in a few words
    #[serde(default)]
    pub title: String,
    /// The question it answers
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// `queued`, `running`, `done`, `failed` or `paused`
    #[serde(default = "queued")]
    pub status: String,
    /// Queued entries run highest first, ties in file order; none is 0
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    /// The entries it waits for: no runner takes it until each is done (a
    /// list of ids, or one id; kept as written)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Value>,
    /// The experiment it belongs to (`D`), shared by its steps
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Who runs it (an agent's name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// Where it can run: `cpu`, `gpu:linux` (this host's GPU), `gpu:win11`
    /// (the VM's) or `gpu` (either)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The machine it runs on (`win11`); none is this one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The command line a runner starts it with
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Its process, with its children
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Its process group (a job started with `setsid`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<u32>,
    /// Its process on `host`, never looked up here
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pid: Option<u64>,
    /// A piece of its command line, to find its processes without a pid
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Its run folder, with `metrics.jsonl` and `args.json`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_dir: Option<String>,
    /// Its log file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// When it was queued, started and ended, RFC 3339
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    /// When it should end, as its owner guesses, RFC 3339
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eta: Option<String>,
    /// What came out, in one line
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    /// What to do about it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// Fields this version does not know, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn queued() -> String {
    String::from("queued")
}

impl Entry {
    /// Its status in lower case
    pub fn status(&self) -> String {
        self.status.trim().to_ascii_lowercase()
    }

    /// Whether it waits its turn: queued or paused, which the page orders
    pub fn waiting(&self) -> bool {
        matches!(self.status().as_str(), "queued" | "paused")
    }

    /// Whether it is still to run or running
    fn active(&self) -> bool {
        matches!(self.status().as_str(), "queued" | "paused" | "running")
    }

    /// The machine it runs on when that is not this one: its `host`, else
    /// the one its `device` names (`gpu:win11`)
    pub fn machine(&self) -> Option<&str> {
        let here = |name: &str| name.is_empty() || name == LOCAL;
        match self.host.as_deref().map(str::trim) {
            Some(host) => (!here(host)).then_some(host),
            None => self
                .device
                .as_deref()?
                .trim()
                .strip_prefix("gpu:")
                .filter(|host| !here(host)),
        }
    }
}

/// Whether an entry of `device` (none: `gpu`, either GPU) can run on
/// `runner`, a runner's device: `cpu` on `cpu`, a machine's GPU on that
/// machine, either's on any GPU (AgentZero's `queue.fits`)
pub fn fits(device: Option<&str>, runner: &str) -> bool {
    let have = device.map(str::trim).filter(|d| !d.is_empty());
    let have = have.unwrap_or("gpu");
    if have == "cpu" || runner == "cpu" {
        return have == runner;
    }
    have == "gpu" || have == runner
}

/// The ids in `entry`'s `after` still in the queue and not done: what holds
/// it back (AgentZero's `queue.waits_for`)
pub fn waits_for(entries: &[Entry], entry: &Entry) -> Vec<String> {
    let ids: Vec<&str> = match &entry.after {
        Some(Value::String(id)) => vec![id.as_str()],
        Some(Value::Array(ids)) => ids.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    ids.into_iter()
        .filter(|id| {
            entries
                .iter()
                .any(|other| other.id == *id && other.status() != "done")
        })
        .map(String::from)
        .collect()
}

/// The entry a runner of `device` (`cpu`, `gpu:linux`, `gpu:win11`) takes
/// next, as an index into `entries`: the first queued one in the run order
/// that fits it and waits for nothing; another machine's GPU takes only
/// entries with a `command` (AgentZero's `queue.next_entry`)
pub fn next_for(entries: &[Entry], device: &str) -> Option<usize> {
    let remote = device
        .strip_prefix("gpu:")
        .is_some_and(|host| host != LOCAL);
    run_order(entries).into_iter().find(|&i| {
        let entry = &entries[i];
        let command = entry
            .command
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty());
        entry.status() == "queued"
            && fits(entry.device.as_deref(), device)
            && (command || !remote)
            && waits_for(entries, entry).is_empty()
    })
}

/// The queue file: `entries` and whatever else it holds
#[derive(Debug, Deserialize)]
struct QueueDoc {
    #[serde(default)]
    version: Option<Value>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    entries: Vec<Value>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

/// An entry as written back: read into an [`Entry`] (written in its field
/// order), or kept as it was when it did not read
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Slot {
    Entry(Box<Entry>),
    Raw(Value),
}

/// The queue as read: the entries that read, and why the others did not
#[derive(Clone, Debug, Default)]
struct Queue {
    entries: Vec<Entry>,
    /// `entry <n>: <why>` for entries that did not read
    problems: Vec<String>,
    /// The file's `updated`
    updated: Option<String>,
}

/// Read a queue file's text
fn parse_queue(text: &str) -> Result<Queue> {
    let doc: QueueDoc = serde_json::from_str(text).context("not a queue file")?;
    let mut queue = Queue {
        updated: doc.updated,
        ..Queue::default()
    };
    let mut ids = HashSet::new();
    for (i, value) in doc.entries.into_iter().enumerate() {
        match serde_json::from_value::<Entry>(value) {
            Ok(entry) if entry.id.trim().is_empty() => {
                queue.problems.push(format!("entry {}: no id", i + 1));
            }
            Ok(entry) if !ids.insert(entry.id.clone()) => {
                queue
                    .problems
                    .push(format!("entry {}: id {} again", i + 1, entry.id));
            }
            Ok(entry) => queue.entries.push(entry),
            Err(e) => queue.problems.push(format!("entry {}: {e}", i + 1)),
        }
    }
    Ok(queue)
}

/// The waiting entries (queued or paused) in the order they run: highest
/// priority first, ties in file order, as indexes into `entries`
pub fn run_order(entries: &[Entry]) -> Vec<usize> {
    let mut waiting: Vec<usize> = (0..entries.len())
        .filter(|&i| entries[i].waiting())
        .collect();
    // A stable sort keeps the file order among equal priorities
    waiting.sort_by_key(|&i| core::cmp::Reverse(entries[i].priority.unwrap_or(0)));
    waiting
}

/// The waiting entries of the queue file `text` in `order` (ids, top
/// first): those it names in its order, then the others as they ran
/// before, with priorities from their count down to 1. Everything else in
/// the file is kept (entries that do not read as they were). `None` when
/// nothing changes.
pub fn reorder(text: &str, order: &[String], now: &str) -> Result<Option<String>> {
    let doc: QueueDoc = serde_json::from_str(text).context("not a queue file")?;
    let mut slots: Vec<Slot> = doc
        .entries
        .into_iter()
        .map(
            |value| match serde_json::from_value::<Entry>(value.clone()) {
                Ok(entry) => Slot::Entry(Box::new(entry)),
                Err(_) => Slot::Raw(value),
            },
        )
        .collect();
    // The entries that read, with where they sit among the slots
    let (places, entries): (Vec<usize>, Vec<Entry>) = slots
        .iter()
        .enumerate()
        .filter_map(|(i, slot)| match slot {
            Slot::Entry(entry) => Some((i, (**entry).clone())),
            Slot::Raw(_) => None,
        })
        .unzip();
    let before = run_order(&entries);
    let mut after: Vec<usize> = Vec::with_capacity(before.len());
    for id in order {
        if let Some(&i) = before.iter().find(|&&i| entries[i].id == *id)
            && !after.contains(&i)
        {
            after.push(i);
        }
    }
    for &i in &before {
        if !after.contains(&i) {
            after.push(i);
        }
    }
    let count = after.len() as i64;
    let mut changed = false;
    for (rank, &i) in after.iter().enumerate() {
        let priority = Some(count - rank as i64);
        if let Slot::Entry(entry) = &mut slots[places[i]]
            && entry.priority != priority
        {
            entry.priority = priority;
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    #[derive(Serialize)]
    struct Out<'a> {
        #[serde(skip_serializing_if = "Option::is_none")]
        version: Option<Value>,
        updated: &'a str,
        entries: Vec<Slot>,
        #[serde(flatten)]
        extra: Map<String, Value>,
    }
    let out = Out {
        version: doc.version,
        updated: now,
        entries: slots,
        extra: doc.extra,
    };
    Ok(Some(serde_json::to_string_pretty(&out)? + "\n"))
}

/// The lock the helper and the lab hold while writing `queue`
pub fn lock_path(queue: &Path) -> PathBuf {
    let mut name = queue.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    queue.with_file_name(name)
}

/// Put the waiting entries of the queue file at `path` in `order`, under
/// its lock (see [`reorder`]); whether the file changed
pub fn write_order(path: &Path, order: &[String]) -> Result<bool, Status> {
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))
        .with_context(|| format!("cannot open the lock of {}", path.display()))?;
    let waited = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if waited.elapsed() < LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(Status(
                    StatusCode::CONFLICT,
                    anyhow::anyhow!("the queue is being written; try again"),
                ));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(anyhow::Error::from(e)
                    .context("cannot lock the queue")
                    .into());
            }
        }
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let now = chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    let Some(text) = reorder(&text, order, &now)? else {
        return Ok(false);
    };
    write_atomic(path, text.as_bytes())?;
    // The lock goes with the file handle
    drop(lock);
    Ok(true)
}

/// Unix ms of an RFC 3339 time, or of a local time without an offset
/// (`2026-09-29T13:05:00`, `2026-09-29 13:05`)
pub fn parse_time(text: &str) -> Option<i64> {
    use chrono::TimeZone;
    let text = text.trim();
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(time.timestamp_millis());
    }
    let local = [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(text, format).ok())?;
    chrono::Local
        .from_local_datetime(&local)
        .earliest()
        .map(|time| time.timestamp_millis())
}

// ---------------------------------------------------------------- machine

/// One reading of the (first) GPU, from `nvidia-smi`; readings it does not
/// support are `None`
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Gpu {
    pub name: String,
    /// Busy, %
    pub util: Option<f64>,
    pub mem_used_mib: Option<f64>,
    pub mem_total_mib: Option<f64>,
    pub temp_c: Option<f64>,
    pub power_w: Option<f64>,
    pub power_limit_w: Option<f64>,
    /// Fan, %
    pub fan: Option<f64>,
    pub sm_mhz: Option<f64>,
    pub pstate: String,
}

/// The fields asked of `nvidia-smi`, the name last since it may hold commas
const GPU_QUERY: &str = "utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw,power.limit,fan.speed,clocks.sm,pstate,name";

/// Parse one line of [`GPU_QUERY`] (`csv,noheader,nounits`)
pub fn parse_gpu(line: &str) -> Option<Gpu> {
    let fields: Vec<&str> = line.splitn(10, ',').map(str::trim).collect();
    if fields.len() < 10 {
        return None;
    }
    let number = |i: usize| fields[i].parse::<f64>().ok().filter(|v| v.is_finite());
    Some(Gpu {
        name: fields[9].to_string(),
        util: number(0),
        mem_used_mib: number(1),
        mem_total_mib: number(2),
        temp_c: number(3),
        power_w: number(4),
        power_limit_w: number(5),
        fan: number(6),
        sm_mhz: number(7),
        pstate: fields[8].to_string(),
    })
}

/// Pid and MiB of each compute process on the GPU, from `nvidia-smi`
pub fn parse_apps(text: &str) -> Vec<(u32, f64)> {
    text.lines()
        .filter_map(|line| {
            let (pid, mib) = line.split_once(',')?;
            Some((pid.trim().parse().ok()?, mib.trim().parse().ok()?))
        })
        .collect()
}

/// Run `nvidia-smi` with `args`: its output, or why not
fn nvidia_smi(args: &[&str]) -> Result<String> {
    let output = Command::new("nvidia-smi")
        .args(args)
        .output()
        .context("nvidia-smi not found")?;
    if !output.status.success() {
        bail!(
            "nvidia-smi: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A remote GPU's file as its runner writes it (AgentZero's
/// `win11.Runner.status_file`): when it read the GPU, whether it could,
/// the GPU, its compute processes, the job it runs, and the runner
#[derive(Debug, Default, Deserialize)]
struct RemoteFile {
    #[serde(default)]
    at: String,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    gpu: Option<RemoteGpu>,
    #[serde(default)]
    processes: Vec<RemoteProc>,
    #[serde(default)]
    job: Option<RemoteJob>,
    #[serde(default)]
    runner: Option<RemoteRunner>,
}

#[derive(Debug, Default, Deserialize)]
struct RemoteGpu {
    #[serde(default)]
    name: String,
    utilization_percent: Option<f64>,
    memory_used_mib: Option<f64>,
    memory_total_mib: Option<f64>,
    temperature_c: Option<f64>,
    power_w: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct RemoteJob {
    #[serde(default)]
    id: String,
    host_pid: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct RemoteRunner {
    pid: Option<u32>,
    #[serde(default)]
    hold: bool,
}

/// A compute process on a remote GPU (Windows does not tell its memory)
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct RemoteProc {
    #[serde(default)]
    pub pid: Option<u64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub memory_mib: Option<f64>,
}

/// A GPU on another machine as its runner last wrote it; what it says of
/// the GPU and the job holds only while it is `fresh`
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Remote {
    /// The machine's name in the queue (`host`, `gpu:<host>`)
    pub host: String,
    /// When its runner read the GPU, Unix ms
    pub at_ms: Option<u64>,
    /// Written within [`REMOTE_STALE`]
    pub fresh: bool,
    /// The reading, when the runner could take one (`name`, `util`,
    /// `mem_used_mib`, `mem_total_mib`, `temp_c`, `power_w`)
    pub gpu: Option<Gpu>,
    /// Why there is no reading: the runner could not reach the machine, or
    /// the file does not read
    pub error: Option<String>,
    pub processes: Vec<RemoteProc>,
    /// The entry its runner runs there, and that job's process there
    pub job: Option<String>,
    pub job_pid: Option<u64>,
    /// Whether its runner still runs here (its pid is alive), and whether
    /// it holds new entries back (`HOLD`)
    pub runner: bool,
    pub hold: bool,
}

impl Remote {
    /// The entry its runner runs now, while the file is fresh
    pub fn running(&self) -> Option<&str> {
        self.job.as_deref().filter(|_| self.fresh)
    }
}

/// Read a remote GPU file's `text` for `host`, as of `now` (Unix ms);
/// `alive` tells whether a pid of this host runs
pub fn parse_remote(host: &str, text: &str, now: u64, alive: impl Fn(u32) -> bool) -> Remote {
    let file = match serde_json::from_str::<RemoteFile>(text) {
        Ok(file) => file,
        Err(e) => {
            return Remote {
                host: host.to_string(),
                error: Some(format!("gpu.json does not read: {e}")),
                ..Remote::default()
            };
        }
    };
    let at_ms = parse_time(&file.at).and_then(|t| u64::try_from(t).ok());
    let stale = REMOTE_STALE.as_millis() as u64;
    let gpu = file.gpu.filter(|_| file.ok).map(|gpu| Gpu {
        name: gpu.name,
        util: gpu.utilization_percent,
        mem_used_mib: gpu.memory_used_mib,
        mem_total_mib: gpu.memory_total_mib,
        temp_c: gpu.temperature_c,
        power_w: gpu.power_w,
        ..Gpu::default()
    });
    let runner = file.runner.unwrap_or_default();
    Remote {
        host: host.to_string(),
        at_ms,
        fresh: at_ms.is_some_and(|at| now.saturating_sub(at) <= stale),
        error: if file.ok {
            None
        } else {
            Some(file.error.unwrap_or_else(|| String::from("unreachable")))
        },
        gpu,
        processes: file.processes,
        job: file
            .job
            .as_ref()
            .map(|job| job.id.clone())
            .filter(|id| !id.is_empty()),
        job_pid: file.job.and_then(|job| job.host_pid),
        runner: runner.pid.is_some_and(alive),
        hold: runner.hold,
    }
}

/// The remote GPU file at `path` now, `None` without one
fn read_remote(path: &Path, now: u64) -> Option<Remote> {
    let text = std::fs::read_to_string(path).ok()?;
    let alive = |pid: u32| Path::new(&format!("/proc/{pid}")).exists();
    Some(parse_remote(REMOTE_HOST, &text, now, alive))
}

/// What `/proc/<pid>/stat` says of a process
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcStat {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    pub comm: String,
    /// User and system time, in clock ticks
    pub ticks: u64,
    /// Start after boot, in clock ticks
    pub start_ticks: u64,
    pub rss_pages: u64,
}

/// Parse `/proc/<pid>/stat`
pub fn parse_stat(text: &str) -> Option<ProcStat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let comm = text.get(open + 1..close)?.to_string();
    // Fields 3 onwards, after the command's name
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let field = |n: usize| rest.get(n - 3)?.parse::<u64>().ok();
    Some(ProcStat {
        pid,
        ppid: field(4)? as u32,
        pgid: field(5)? as u32,
        comm,
        ticks: field(14)? + field(15)?,
        start_ticks: field(22)?,
        rss_pages: field(24)?,
    })
}

/// A process's command line, its words
fn cmdline(pid: u32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| {
            bytes
                .split(|&b| b == 0)
                .filter(|word| !word.is_empty())
                .map(|word| String::from_utf8_lossy(word).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// A short name for a process: the module or script Python, uv, node or a
/// shell runs, else the program
pub fn short_name(words: &[String], comm: &str) -> String {
    let base = |word: &str| word.rsplit('/').next().unwrap_or(word).to_string();
    let Some(first) = words.first() else {
        return comm.to_string();
    };
    let program = base(first);
    let args = &words[1..];
    let first_plain = || {
        args.iter()
            .find(|word| !word.starts_with('-'))
            .map(|w| base(w))
    };
    if program.starts_with("python") {
        if let Some(i) = args.iter().position(|word| word == "-m") {
            return args.get(i + 1).cloned().unwrap_or(program);
        }
        if args.iter().any(|word| word == "-c") {
            return format!("{program} -c");
        }
        return first_plain().unwrap_or(program);
    }
    if program == "uv" {
        if let Some(i) = args.iter().position(|word| word == "run") {
            return args[i + 1..]
                .iter()
                .find(|word| !word.starts_with('-'))
                .map_or(program, |word| base(word));
        }
        return program;
    }
    if matches!(program.as_str(), "node" | "bash" | "sh" | "zsh" | "dash") {
        return first_plain().unwrap_or(program);
    }
    program
}

/// Clock ticks per second and bytes per page
fn clock() -> (u64, u64) {
    // SAFETY: sysconf only reads system constants
    let (ticks, page) = unsafe {
        (
            libc::sysconf(libc::_SC_CLK_TCK),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    (
        u64::try_from(ticks).unwrap_or(100).max(1),
        u64::try_from(page).unwrap_or(4096),
    )
}

/// `/proc/stat`'s busy and total CPU ticks, and the boot time in Unix s
fn cpu_ticks() -> Option<(u64, u64, u64)> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    let mut lines = text.lines();
    let values: Vec<u64> = lines
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal
    let total: u64 = values.iter().take(8).sum();
    let idle = values.get(3)? + values.get(4).unwrap_or(&0);
    let boot = text
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())?;
    Some((total - idle, total, boot))
}

/// Memory in bytes: total, available, swap total, swap free
fn meminfo() -> Option<[u64; 4]> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let kib = line.strip_prefix(name)?.trim().strip_suffix("kB")?;
            Some(kib.trim().parse::<u64>().ok()? * 1024)
        })
    };
    Some([
        field("MemTotal:")?,
        field("MemAvailable:")?,
        field("SwapTotal:").unwrap_or(0),
        field("SwapFree:").unwrap_or(0),
    ])
}

/// The load averages over 1, 5 and 15 minutes
fn loadavg() -> Option<[f64; 3]> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut values = text.split_whitespace().map(|v| v.parse().ok());
    Some([values.next()??, values.next()??, values.next()??])
}

// ------------------------------------------------------------ run folders

/// A row of `metrics.jsonl` with a validation split (`val`, `val_azu`,
/// `val-tuned`): its step and numbers
#[derive(Clone, Debug, PartialEq)]
pub struct ValRow {
    pub step: f64,
    pub split: String,
    pub values: BTreeMap<String, f64>,
}

/// A run folder's `metrics.jsonl`, read as it grows, and its `args.json`
#[derive(Clone, Debug, Default)]
pub struct RunSeries {
    /// Bytes of `metrics.jsonl` read, whole lines only
    offset: u64,
    /// Train rows: step, total loss, learning rate, s per step (NaN when
    /// missing)
    pub train: Vec<[f64; 4]>,
    pub val: Vec<ValRow>,
    /// The step of the first closing row ([`END_SPLITS`]) after the last
    /// train row: the run has ended, and kept its checkpoint of that step
    pub end: Option<f64>,
    pub args: Option<Value>,
    args_modified: Option<SystemTime>,
}

impl RunSeries {
    /// Read what `dir` gained since the last time
    pub fn refresh(&mut self, dir: &Path) {
        let metrics = dir.join("metrics.jsonl");
        match std::fs::metadata(&metrics) {
            Ok(meta) if meta.len() < self.offset => {
                // Rewritten: from the start
                *self = RunSeries {
                    args: self.args.take(),
                    args_modified: self.args_modified,
                    ..RunSeries::default()
                };
                self.read_metrics(&metrics, meta.len());
            }
            Ok(meta) if meta.len() > self.offset => self.read_metrics(&metrics, meta.len()),
            _ => {}
        }
        let args = dir.join("args.json");
        let modified = std::fs::metadata(&args).and_then(|m| m.modified()).ok();
        if modified != self.args_modified {
            self.args_modified = modified;
            self.args = std::fs::read_to_string(&args)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok());
        }
    }

    fn read_metrics(&mut self, path: &Path, len: u64) {
        let Ok(mut file) = File::open(path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file
            .take(len - self.offset)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }
        // A line still being written waits for the next time
        let Some(end) = bytes.iter().rposition(|&b| b == b'\n') else {
            return;
        };
        self.offset += end as u64 + 1;
        for line in bytes[..end].split(|&b| b == b'\n') {
            self.add_line(line);
        }
    }

    /// Take in one line of `metrics.jsonl`
    pub fn add_line(&mut self, line: &[u8]) {
        let Ok(Value::Object(row)) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let Some(step) = row.get("step").and_then(Value::as_f64) else {
            return;
        };
        let number = |key: &str| row.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN);
        let split = row.get("split").and_then(Value::as_str).unwrap_or_default();
        if split == "train" {
            self.train.push([
                step,
                number("loss_total"),
                number("lr"),
                number("s_per_step"),
            ]);
            // Training again after a closing row: resumed
            self.end = None;
        }
        if END_SPLITS.contains(&split) && self.end.is_none() {
            self.end = Some(step);
        }
        if split.starts_with("val") {
            self.val.push(ValRow {
                step,
                split: split.to_string(),
                values: row
                    .iter()
                    .filter(|(key, _)| *key != "step")
                    .filter_map(|(key, value)| Some((key.clone(), value.as_f64()?)))
                    .collect(),
            });
        }
    }

    /// The step reached, if any row says
    pub fn step(&self) -> Option<f64> {
        let train = self.train.last().map(|row| row[0]);
        let val = self.val.last().map(|row| row.step);
        match (train, val) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The steps it is to run, from `args.json`
    pub fn total(&self) -> Option<f64> {
        self.args
            .as_ref()?
            .get("steps")?
            .as_f64()
            .filter(|&s| s > 0.0)
    }

    /// Seconds per step lately: the median of the last train rows'
    pub fn s_per_step(&self) -> Option<f64> {
        let mut recent: Vec<f64> = self
            .train
            .iter()
            .rev()
            .take(7)
            .map(|row| row[3])
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        recent.sort_by(f64::total_cmp);
        recent.get(recent.len() / 2).copied()
    }

    /// The train rows, at most `limit`: each bucket's mean loss at its last
    /// step, the last row as it is
    pub fn curve(&self, limit: usize) -> Vec<[f64; 4]> {
        if self.train.len() <= limit || limit < 2 {
            return self.train.clone();
        }
        let size = self.train.len().div_ceil(limit - 1);
        let (last, rest) = self.train.split_last().expect("rows");
        let mut out: Vec<[f64; 4]> = rest
            .chunks(size)
            .map(|chunk| {
                let losses: Vec<f64> = chunk
                    .iter()
                    .map(|r| r[1])
                    .filter(|v| v.is_finite())
                    .collect();
                let loss = if losses.is_empty() {
                    f64::NAN
                } else {
                    losses.iter().sum::<f64>() / losses.len() as f64
                };
                let end = chunk[chunk.len() - 1];
                [end[0], loss, end[2], end[3]]
            })
            .collect();
        out.push(*last);
        out
    }
}

/// The last `N/M` in `text`'s lines, from the end: the first whole
/// numbers with `N <= M` in the last line that has them (`[988/1017]`,
/// `step 29500/30000`)
pub fn log_progress(text: &str) -> Option<(f64, f64)> {
    text.lines().rev().find_map(|line| {
        let bytes = line.as_bytes();
        let is_digit = |i: usize| bytes.get(i).is_some_and(u8::is_ascii_digit);
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'/' {
                i += 1;
                continue;
            }
            // Digits on both sides, not parts of decimals or longer words
            let mut a = i;
            while a > 0 && is_digit(a - 1) {
                a -= 1;
            }
            let mut b = i + 1;
            while is_digit(b) {
                b += 1;
            }
            let apart = |c: u8| !(c == b'.' || c == b'/' || c.is_ascii_alphanumeric());
            let clean = a < i
                && b > i + 1
                && (a == 0 || apart(bytes[a - 1]))
                && (b == bytes.len() || apart(bytes[b]));
            if clean {
                let done: f64 = line[a..i].parse().ok()?;
                let total: f64 = line[i + 1..b].parse().ok()?;
                if total >= 2.0 && done <= total {
                    return Some((done, total));
                }
            }
            i = b.max(i + 1);
        }
        None
    })
}

/// Whether a trainer said its loop is over ([`END_LINES`]) after the last
/// `N/M` in `text`: that run has ended, whether or not it reached `M`
pub fn log_done(text: &str) -> bool {
    for line in text.lines().rev() {
        let line = clean_line(line);
        if END_LINES.iter().any(|end| line.contains(end)) {
            return true;
        }
        if log_progress(&line).is_some() {
            return false;
        }
    }
    false
}

/// The last `max` bytes of a file, from a line's start
fn tail(path: &Path, max: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(match (start > 0, text.find('\n')) {
        (true, Some(i)) => text[i + 1..].to_string(),
        _ => text,
    })
}

/// A log's last line, as a progress bar redraws it (`\r`) and without
/// terminal colours
fn clean_line(line: &str) -> String {
    let line = line.rsplit('\r').next().unwrap_or(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip an escape sequence up to its final letter
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.trim_end().to_string()
}

// ------------------------------------------------------------------ state

/// One GPU's numbers in a [`Sample`]; `NAN` for what was not read
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuSample {
    /// Busy, %
    pub util: f64,
    /// Memory used, MiB, and the part of it the queue's entries hold (known
    /// on this host only)
    pub mem_mib: f64,
    pub jobs_mib: f64,
    pub temp_c: f64,
    pub power_w: f64,
}

impl GpuSample {
    /// Nothing read
    pub const UNKNOWN: Self = Self {
        util: f64::NAN,
        mem_mib: f64::NAN,
        jobs_mib: f64::NAN,
        temp_c: f64::NAN,
        power_w: f64::NAN,
    };
}

impl Default for GpuSample {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

/// One sample of the machine; `NAN` for what was not read
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub t_ms: u64,
    /// This host's GPU
    pub gpu: GpuSample,
    /// The win11 VM's GPU, unknown while its runner's file is stale
    pub remote: GpuSample,
    /// CPU busy over all threads, %
    pub cpu: f64,
    /// Load average over the last minute
    pub load: f64,
    /// Memory in use (total less available), MiB
    pub ram_mib: f64,
    /// The entries seen running, each with the CPU its processes here took,
    /// in cores (`NAN` for one on another machine)
    pub running: Vec<(String, f64)>,
}

impl Default for Sample {
    fn default() -> Self {
        Self {
            t_ms: 0,
            gpu: GpuSample::UNKNOWN,
            remote: GpuSample::UNKNOWN,
            cpu: f64::NAN,
            load: f64::NAN,
            ram_mib: f64::NAN,
            running: Vec::new(),
        }
    }
}

/// A number rounded to `digits` decimals for JSON, `null` when not read
fn rounded(v: f64, digits: i32) -> Value {
    match (v.is_finite(), digits) {
        (false, _) => Value::Null,
        (true, 0) => json!(v.round() as i64),
        (true, _) => {
            let scale = 10f64.powi(digits);
            json!((v * scale).round() / scale)
        }
    }
}

impl Sample {
    /// Its numbers: t; this host's GPU busy, memory, the jobs' memory,
    /// temperature, power; CPU, RAM; the remote GPU's busy, memory,
    /// temperature, power; load
    fn numbers(&self) -> Vec<Value> {
        let (gpu, remote) = (&self.gpu, &self.remote);
        vec![
            json!(self.t_ms),
            rounded(gpu.util, 0),
            rounded(gpu.mem_mib, 0),
            rounded(gpu.jobs_mib, 0),
            rounded(gpu.temp_c, 0),
            rounded(gpu.power_w, 1),
            rounded(self.cpu, 1),
            rounded(self.ram_mib, 0),
            rounded(remote.util, 0),
            rounded(remote.mem_mib, 0),
            rounded(remote.temp_c, 0),
            rounded(remote.power_w, 1),
            rounded(self.load, 2),
        ]
    }

    /// The CPU each entry took, in cores, where known: `{id: cores}`
    fn cores(&self) -> Value {
        let cores: Map<String, Value> = self
            .running
            .iter()
            .filter(|(_, cores)| cores.is_finite())
            .map(|(id, cores)| (id.clone(), rounded(*cores, 2)))
            .collect();
        Value::Object(cores)
    }

    /// As a row for the page: its [`Self::numbers`], then the CPU of each
    /// entry ([`Self::cores`])
    fn row(&self) -> Value {
        let mut values = self.numbers();
        values.push(self.cores());
        Value::Array(values)
    }

    /// As a line of the log on disk: its row with the ids of the entries
    /// seen running at index 8, where the log's first lines have them after
    /// their eight numbers, so each version reads the other's lines
    pub fn line(&self) -> String {
        let mut values = self.numbers();
        let ids: Vec<&str> = self.running.iter().map(|(id, _)| id.as_str()).collect();
        values.insert(8, json!(ids));
        values.push(self.cores());
        Value::Array(values).to_string()
    }

    /// A line of the log on disk back ([`Self::line`], or a first version's
    /// line, whose remote GPU, load and CPU per entry are not known);
    /// `None` for a torn or foreign line
    pub fn parse_line(line: &str) -> Option<Self> {
        let values: Vec<Value> = serde_json::from_str(line).ok()?;
        let number = |i: usize| values.get(i).and_then(Value::as_f64).unwrap_or(f64::NAN);
        let cores = values.get(14).and_then(Value::as_object);
        let running = values
            .get(8)
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| {
                        let id = id.as_str()?;
                        let cores = cores.and_then(|c| c.get(id)?.as_f64());
                        Some((id.to_string(), cores.unwrap_or(f64::NAN)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Sample {
            t_ms: values.first()?.as_u64()?,
            gpu: GpuSample {
                util: number(1),
                mem_mib: number(2),
                jobs_mib: number(3),
                temp_c: number(4),
                power_w: number(5),
            },
            cpu: number(6),
            ram_mib: number(7),
            remote: GpuSample {
                util: number(9),
                mem_mib: number(10),
                temp_c: number(11),
                power_w: number(12),
                ..GpuSample::UNKNOWN
            },
            load: number(13),
            running,
        })
    }
}

/// Samples as one, at the last one's time: the mean utilization, power,
/// CPU and load, the most memory, temperature and RAM, and every entry
/// seen running with its mean CPU while it ran
pub fn merge(bucket: &[Sample]) -> Sample {
    let Some(last) = bucket.last() else {
        return Sample::default();
    };
    let finite = |f: &dyn Fn(&Sample) -> f64| -> Vec<f64> {
        bucket.iter().map(f).filter(|v| v.is_finite()).collect()
    };
    let mean = |f: &dyn Fn(&Sample) -> f64| {
        let values = finite(f);
        if values.is_empty() {
            f64::NAN
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        }
    };
    let most = |f: &dyn Fn(&Sample) -> f64| finite(f).into_iter().fold(f64::NAN, f64::max);
    let gpu = |pick: fn(&Sample) -> &GpuSample| GpuSample {
        util: mean(&|s| pick(s).util),
        mem_mib: most(&|s| pick(s).mem_mib),
        jobs_mib: most(&|s| pick(s).jobs_mib),
        temp_c: most(&|s| pick(s).temp_c),
        power_w: mean(&|s| pick(s).power_w),
    };
    let mut running: Vec<(String, f64)> = Vec::new();
    for (id, _) in bucket.iter().flat_map(|s| &s.running) {
        if !running.iter().any(|(seen, _)| seen == id) {
            let cores = mean(&|s| {
                s.running
                    .iter()
                    .find(|(other, _)| other == id)
                    .map_or(f64::NAN, |(_, cores)| *cores)
            });
            running.push((id.clone(), cores));
        }
    }
    running.sort_by(|a, b| a.0.cmp(&b.0));
    Sample {
        t_ms: last.t_ms,
        gpu: gpu(|s| &s.gpu),
        remote: gpu(|s| &s.remote),
        cpu: mean(&|s| s.cpu),
        load: mean(&|s| s.load),
        ram_mib: most(&|s| s.ram_mib),
        running,
    }
}

/// The samples at most `max` of them: buckets of the same width (aligned,
/// so asking again gives the same buckets), each [`merge`]d
pub fn downsample(samples: &[Sample], max: usize) -> Vec<Sample> {
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Vec::new();
    };
    if samples.len() <= max || max == 0 {
        return samples.to_vec();
    }
    let width = ((last.t_ms - first.t_ms) / max as u64 + 1).max(1);
    samples
        .chunk_by(|a, b| a.t_ms / width == b.t_ms / width)
        .map(merge)
        .collect()
}

/// Extend an entry's spans of running with a sighting at `t_ms`
fn note_running(spans: &mut Vec<(u64, u64)>, t_ms: u64) {
    match spans.last_mut() {
        Some((_, end)) if t_ms.saturating_sub(*end) <= SPAN_GAP_MS => *end = t_ms,
        _ => spans.push((t_ms, t_ms)),
    }
}

/// The log's samples as kept: in time order, none older than
/// [`HISTORY_KEEP`], those older than [`KEEP`] [`merge`]d into buckets of
/// [`HISTORY_BUCKET_MS`]
fn compact(mut samples: Vec<Sample>, now: u64) -> Vec<Sample> {
    samples.sort_by_key(|s| s.t_ms);
    let dropped = now.saturating_sub(HISTORY_KEEP.as_millis() as u64);
    let thinned = now.saturating_sub(KEEP.as_millis() as u64);
    samples.retain(|s| s.t_ms >= dropped);
    let recent = samples.split_off(samples.partition_point(|s| s.t_ms < thinned));
    let mut out: Vec<Sample> = samples
        .chunk_by(|a, b| a.t_ms / HISTORY_BUCKET_MS == b.t_ms / HISTORY_BUCKET_MS)
        .map(merge)
        .collect();
    out.extend(recent);
    out
}

/// Append a sample to the log on disk, creating its folder
fn append_history(path: &Path, sample: &Sample) -> Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{}\n", sample.line()).as_bytes())?;
    Ok(())
}

/// A process the page is told about
#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProcInfo {
    pid: u32,
    pgid: u32,
    name: String,
    /// Its command line, shortened
    command: String,
    gpu_mib: Option<f64>,
    /// The entry it belongs to
    entry: Option<String>,
}

/// What the sampler saw of an entry's processes
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Live {
    pids: Vec<u32>,
    /// CPU over the last interval, % of one core
    cpu_percent: f64,
    rss_bytes: u64,
    gpu_mib: f64,
    /// When its oldest process started, Unix ms
    since_ms: u64,
    /// How it was found: `pgid`, `pid` or `match`
    found_by: &'static str,
}

/// An entry's progress: steps done of the total, and the rate seen
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
struct Progress {
    step: f64,
    total: Option<f64>,
    /// `metrics` (its run folder) or `log`
    source: &'static str,
    /// The run has ended (its closing rows, [`END_SPLITS`]; with only a
    /// log, its stop lines, [`END_LINES`]), early when `step` is short of
    /// `total`; its kept checkpoint's step, when known
    ended: bool,
    best_step: Option<f64>,
    /// Steps per second, as seen lately (else from `s_per_step`)
    rate: Option<f64>,
    s_per_step: Option<f64>,
    /// When it should reach `total`: only while it runs, and neither ended
    /// nor stalled
    eta_ms: Option<u64>,
    /// The last train and validation losses, and the learning rate
    loss: Option<f64>,
    val_loss: Option<f64>,
    lr: Option<f64>,
    /// When the run folder or log was last written, Unix ms
    updated_ms: Option<u64>,
    /// When its step last moved, Unix ms: the run folder's last write, or
    /// when the sampler first saw the log's step
    moved_ms: Option<u64>,
}

/// Everything the sampler keeps
#[derive(Default)]
struct Inner {
    samples: VecDeque<Sample>,
    gpu: Option<Gpu>,
    gpu_error: Option<String>,
    /// The win11 VM's GPU as its runner last wrote it, if it has a file
    remote: Option<Remote>,
    cpu: Option<f64>,
    cores: usize,
    load: Option<[f64; 3]>,
    memory: Option<[u64; 4]>,
    /// The queue as last read, and the file's size and time then
    queue: Queue,
    queue_error: Option<String>,
    queue_seen: Option<(u64, SystemTime)>,
    /// Compute processes on the GPU, named
    gpu_procs: Vec<ProcInfo>,
    /// Each live entry's processes
    live: HashMap<String, Live>,
    progress: HashMap<String, Progress>,
    /// (Unix ms, step) seen of each entry, for its rate
    seen: HashMap<String, VecDeque<(u64, f64)>>,
    /// The step of each entry's log and when the sampler first saw it
    moved: HashMap<String, (f64, u64)>,
    /// When each entry was seen running: spans of Unix ms
    spans: HashMap<String, Vec<(u64, u64)>>,
    /// Each run folder's metrics, by folder
    runs: HashMap<PathBuf, RunSeries>,
    /// Busy and total CPU ticks at the last sample, and each process's
    /// ticks then
    cpu_before: Option<(u64, u64)>,
    ticks_before: HashMap<u32, u64>,
    sampled_at: Option<Instant>,
}

/// The Pipeline app: the sampler's findings and the queue
pub struct Pipeline {
    settings: Settings,
    inner: Mutex<Inner>,
}

impl Pipeline {
    /// Start sampling the machine on a thread of its own
    pub fn start(settings: Settings) -> Arc<Self> {
        let pipeline = Arc::new(Self {
            settings,
            inner: Mutex::new(Inner {
                cores: num_cpus::get(),
                ..Inner::default()
            }),
        });
        let sampler = Arc::clone(&pipeline);
        std::thread::Builder::new()
            .name(String::from("pipeline"))
            .spawn(move || {
                let mut compacted = None;
                loop {
                    let started = Instant::now();
                    if let Some(path) = &sampler.settings.history
                        && compacted.is_none_or(|at: Instant| at.elapsed() >= COMPACT_EVERY)
                    {
                        sampler.load_history(path, compacted.is_none());
                        compacted = Some(Instant::now());
                    }
                    let sample = sampler.sample();
                    if let Some(path) = &sampler.settings.history
                        && let Err(e) = append_history(path, &sample)
                    {
                        log::debug!("pipeline: cannot append to {}: {e:#}", path.display());
                    }
                    std::thread::sleep(SAMPLE_EVERY.saturating_sub(started.elapsed()));
                }
            })
            .expect("thread");
        pipeline
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A path from the queue: absolute, or from the queue file's folder
    fn resolve(&self, path: &str) -> PathBuf {
        let path = Path::new(path.trim());
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.settings
                .queue
                .parent()
                .unwrap_or(Path::new("."))
                .join(path)
        }
    }

    /// Read the queue file again if it changed
    fn reload_queue(&self, inner: &mut Inner) {
        let path = &self.settings.queue;
        let seen = std::fs::metadata(path)
            .ok()
            .and_then(|meta| Some((meta.len(), meta.modified().ok()?)));
        if seen.is_some() && seen == inner.queue_seen {
            return;
        }
        inner.queue_seen = seen;
        let read = || -> Result<Queue> {
            let (len, _) = seen.context("no queue file yet")?;
            anyhow::ensure!(len <= MAX_QUEUE_BYTES, "the queue file is over 1 MB");
            parse_queue(&std::fs::read_to_string(path)?)
        };
        match read() {
            Ok(queue) => {
                inner.queue = queue;
                inner.queue_error = None;
            }
            Err(e) if seen.is_none() => {
                inner.queue = Queue::default();
                inner.queue_error = Some(format!("{e:#}"));
            }
            // A file being written by hand: keep the last good one
            Err(e) => inner.queue_error = Some(format!("{e:#}")),
        }
        // Forget the run folders the queue no longer names
        let dirs: HashSet<PathBuf> = inner
            .queue
            .entries
            .iter()
            .filter_map(|entry| entry.run_dir.as_deref().map(|dir| self.resolve(dir)))
            .collect();
        inner.runs.retain(|dir, _| dirs.contains(dir));
    }

    /// Compact the log on disk ([`compact`]) and, the first time, take its
    /// last [`KEEP`] as the timeline's start
    fn load_history(&self, path: &Path, first: bool) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                log::warn!("pipeline: cannot read {}: {e}", path.display());
                return;
            }
        };
        let samples = compact(
            text.lines().filter_map(Sample::parse_line).collect(),
            unix_ms(),
        );
        let mut out = String::new();
        for sample in &samples {
            out.push_str(&sample.line());
            out.push('\n');
        }
        if let Err(e) = write_atomic(path, out.as_bytes()) {
            log::warn!("pipeline: cannot rewrite {}: {e:#}", path.display());
        }
        if !first {
            return;
        }
        let oldest = unix_ms().saturating_sub(KEEP.as_millis() as u64);
        let mut inner = self.inner();
        for sample in samples.into_iter().filter(|s| s.t_ms >= oldest) {
            for (id, _) in &sample.running {
                note_running(inner.spans.entry(id.clone()).or_default(), sample.t_ms);
            }
            inner.samples.push_back(sample);
        }
    }

    /// One sample of the machine, both GPUs, the queue's processes and
    /// their progress
    fn sample(&self) -> Sample {
        let now = unix_ms();
        // The slow parts first, without the lock
        let gpu = nvidia_smi(&[
            &format!("--query-gpu={GPU_QUERY}"),
            "--format=csv,noheader,nounits",
        ])
        .map(|text| text.lines().next().and_then(parse_gpu));
        let apps = gpu
            .as_ref()
            .ok()
            .and_then(|_| {
                nvidia_smi(&[
                    "--query-compute-apps=pid,used_memory",
                    "--format=csv,noheader,nounits",
                ])
                .ok()
            })
            .map(|text| parse_apps(&text))
            .unwrap_or_default();
        let remote = read_remote(&self.settings.remote, now);
        let (clk_tck, page) = clock();
        let cpu = cpu_ticks();
        let memory = meminfo();
        let load = loadavg();
        let procs: Vec<ProcStat> = std::fs::read_dir("/proc")
            .map(|dir| {
                dir.filter_map(|entry| {
                    let entry = entry.ok()?;
                    entry.file_name().to_str()?.parse::<u32>().ok()?;
                    parse_stat(&std::fs::read_to_string(entry.path().join("stat")).ok()?)
                })
                .collect()
            })
            .unwrap_or_default();
        // Every command line, when an entry is found by a piece of its own
        let patterns = {
            let mut inner = self.inner();
            self.reload_queue(&mut inner);
            inner.queue.entries.iter().any(|entry| {
                entry.active()
                    && entry
                        .pattern
                        .as_deref()
                        .is_some_and(|p| !p.trim().is_empty())
            })
        };
        let commands: HashMap<u32, String> = if patterns {
            procs
                .iter()
                .map(|p| (p.pid, cmdline(p.pid).join(" ")))
                .collect()
        } else {
            HashMap::new()
        };
        let app_words: HashMap<u32, Vec<String>> =
            apps.iter().map(|&(pid, _)| (pid, cmdline(pid))).collect();

        let mut inner = self.inner();
        let elapsed = inner.sampled_at.map(|at| at.elapsed().as_secs_f64());
        inner.sampled_at = Some(Instant::now());

        // Which processes belong to which entry: its process group, its
        // process and children, or the processes its pattern names (with
        // their groups when they lead them); an entry of another machine
        // has none here
        let by_pid: HashMap<u32, &ProcStat> = procs.iter().map(|p| (p.pid, p)).collect();
        let boot_ms = cpu.map_or(0, |(_, _, boot)| boot * 1000);
        let mut owner: HashMap<u32, String> = HashMap::new();
        let mut live: HashMap<String, Live> = HashMap::new();
        // Running entries claim processes before waiting ones
        let mut entries: Vec<&Entry> = inner
            .queue
            .entries
            .iter()
            .filter(|e| e.active() && e.machine().is_none())
            .collect();
        entries.sort_by_key(|entry| entry.status() != "running");
        for entry in entries {
            let mut members: Vec<u32> = Vec::new();
            let mut found_by = "";
            if let Some(pgid) = entry.pgid {
                members.extend(procs.iter().filter(|p| p.pgid == pgid).map(|p| p.pid));
                found_by = "pgid";
            }
            if let Some(pid) = entry.pid
                && by_pid.contains_key(&pid)
            {
                members.extend(descendants(&procs, pid));
                if !members.is_empty() && found_by.is_empty() {
                    found_by = "pid";
                }
            }
            if members.is_empty()
                && let Some(pattern) = entry.pattern.as_deref().map(str::trim)
                && !pattern.is_empty()
            {
                for (&pid, command) in &commands {
                    if pid == std::process::id() || !command.contains(pattern) {
                        continue;
                    }
                    let Some(proc_) = by_pid.get(&pid) else {
                        continue;
                    };
                    if proc_.pgid == pid {
                        members.extend(procs.iter().filter(|p| p.pgid == pid).map(|p| p.pid));
                    } else {
                        members.extend(descendants(&procs, pid));
                    }
                }
                found_by = "match";
            }
            members.sort_unstable();
            members.dedup();
            members.retain(|pid| !owner.contains_key(pid));
            if members.is_empty() {
                continue;
            }
            let mut info = Live {
                found_by,
                since_ms: u64::MAX,
                ..Live::default()
            };
            for pid in &members {
                let proc_ = by_pid[pid];
                owner.insert(*pid, entry.id.clone());
                info.rss_bytes += proc_.rss_pages * page;
                info.since_ms = info
                    .since_ms
                    .min(boot_ms + proc_.start_ticks * 1000 / clk_tck);
                if let (Some(before), Some(seconds)) = (inner.ticks_before.get(pid), elapsed)
                    && seconds > 0.0
                {
                    info.cpu_percent +=
                        proc_.ticks.saturating_sub(*before) as f64 / clk_tck as f64 / seconds
                            * 100.0;
                }
            }
            info.pids = members;
            live.insert(entry.id.clone(), info);
        }
        inner.ticks_before = procs.iter().map(|p| (p.pid, p.ticks)).collect();

        // The GPU's compute processes, named, with their entries
        let mut gpu_procs = Vec::new();
        for &(pid, mib) in &apps {
            let entry = owner.get(&pid).cloned();
            if let Some(id) = &entry
                && let Some(info) = live.get_mut(id)
            {
                info.gpu_mib += mib;
            }
            let words = app_words.get(&pid).cloned().unwrap_or_default();
            let comm = by_pid.get(&pid).map(|p| p.comm.clone()).unwrap_or_default();
            let mut command = words.join(" ");
            if command.chars().count() > 400 {
                command = command.chars().take(400).collect::<String>() + "…";
            }
            gpu_procs.push(ProcInfo {
                pid,
                pgid: by_pid.get(&pid).map_or(0, |p| p.pgid),
                name: short_name(&words, &comm),
                command,
                gpu_mib: Some(mib),
                entry,
            });
        }
        gpu_procs.sort_by(|a, b| {
            b.gpu_mib
                .unwrap_or(0.0)
                .total_cmp(&a.gpu_mib.unwrap_or(0.0))
        });
        let jobs_mib: f64 = live.values().map(|info| info.gpu_mib).sum();

        // The CPU over the interval
        let cpu_percent = match (cpu, inner.cpu_before) {
            (Some((busy, total, _)), Some((busy0, total0))) if total > total0 => {
                Some((busy.saturating_sub(busy0)) as f64 / (total - total0) as f64 * 100.0)
            }
            _ => None,
        };
        inner.cpu_before = cpu.map(|(busy, total, _)| (busy, total));
        inner.cpu = cpu_percent.or(inner.cpu);
        inner.load = load;
        inner.memory = memory;

        // The entries seen running: those with processes here, with the
        // CPU they took (not known of the first sample), and the one the
        // win11 runner runs, while its file is fresh
        let mut running: Vec<(String, f64)> = live
            .iter()
            .map(|(id, info)| {
                let cores = elapsed.map_or(f64::NAN, |_| info.cpu_percent / 100.0);
                (id.clone(), cores)
            })
            .collect();
        if let Some(id) = remote.as_ref().and_then(Remote::running)
            && inner.queue.entries.iter().any(|entry| entry.id == id)
            && !live.contains_key(id)
        {
            running.push((id.to_string(), f64::NAN));
        }
        running.sort_by(|a, b| a.0.cmp(&b.0));

        let reading = gpu.as_ref().ok().and_then(Option::as_ref);
        let number = |v: Option<f64>| v.unwrap_or(f64::NAN);
        let remote_reading = remote
            .as_ref()
            .filter(|remote| remote.fresh)
            .and_then(|remote| remote.gpu.as_ref());
        let sample = Sample {
            t_ms: now,
            gpu: GpuSample {
                util: number(reading.and_then(|g| g.util)),
                mem_mib: number(reading.and_then(|g| g.mem_used_mib)),
                jobs_mib: if reading.is_some() {
                    jobs_mib
                } else {
                    f64::NAN
                },
                temp_c: number(reading.and_then(|g| g.temp_c)),
                power_w: number(reading.and_then(|g| g.power_w)),
            },
            remote: GpuSample {
                util: number(remote_reading.and_then(|g| g.util)),
                mem_mib: number(remote_reading.and_then(|g| g.mem_used_mib)),
                temp_c: number(remote_reading.and_then(|g| g.temp_c)),
                power_w: number(remote_reading.and_then(|g| g.power_w)),
                ..GpuSample::UNKNOWN
            },
            cpu: number(cpu_percent),
            load: load.map_or(f64::NAN, |[one, ..]| one),
            ram_mib: memory.map_or(f64::NAN, |[total, available, ..]| {
                total.saturating_sub(available) as f64 / (1 << 20) as f64
            }),
            running,
        };
        inner.samples.push_back(sample.clone());
        inner.remote = remote;
        let oldest = now.saturating_sub(KEEP.as_millis() as u64);
        while inner.samples.front().is_some_and(|s| s.t_ms < oldest) {
            inner.samples.pop_front();
        }
        match gpu {
            Ok(Some(reading)) => {
                inner.gpu = Some(reading);
                inner.gpu_error = None;
            }
            Ok(None) => {
                inner.gpu = None;
                inner.gpu_error = Some(String::from("nvidia-smi listed no GPU"));
            }
            Err(e) => {
                inner.gpu = None;
                inner.gpu_error = Some(format!("{e:#}"));
            }
        }
        inner.gpu_procs = gpu_procs;

        // When each entry was seen running
        for (id, _) in &sample.running {
            note_running(inner.spans.entry(id.clone()).or_default(), now);
        }
        for spans in inner.spans.values_mut() {
            spans.retain(|&(_, end)| end >= oldest);
        }
        inner.spans.retain(|_, spans| !spans.is_empty());
        inner.live = live;

        // Progress of the entries that run, and of the waiting ones whose
        // run folder has rows (run, or running, before the file said so)
        let running: Vec<Entry> = inner
            .queue
            .entries
            .iter()
            .filter(|entry| {
                entry.status() == "running"
                    || inner.live.contains_key(&entry.id)
                    || (entry.active() && entry.run_dir.is_some())
            })
            .cloned()
            .collect();
        let mut progress = HashMap::new();
        for entry in &running {
            let alive = sample.running.iter().any(|(id, _)| *id == entry.id);
            if let Some(found) = self.progress_of(&mut inner, entry, alive, now) {
                progress.insert(entry.id.clone(), found);
            }
        }
        let ids: HashSet<&String> = running.iter().map(|entry| &entry.id).collect();
        inner.seen.retain(|id, _| ids.contains(id));
        inner.moved.retain(|id, _| ids.contains(id));
        inner.progress = progress;
        sample
    }

    /// An entry's progress now, from its run folder or its log, with the
    /// rate over what was seen of it lately; `alive`: its processes run
    /// (here, or on the machine whose runner runs it)
    fn progress_of(
        &self,
        inner: &mut Inner,
        entry: &Entry,
        alive: bool,
        now: u64,
    ) -> Option<Progress> {
        let mut found = None;
        if let Some(dir) = entry.run_dir.as_deref() {
            let dir = self.resolve(dir);
            let series = inner.runs.entry(dir.clone()).or_default();
            series.refresh(&dir);
            if let Some(step) = series.step() {
                let updated_ms = modified_ms(&dir.join("metrics.jsonl"));
                found = Some(Progress {
                    step,
                    total: series.total(),
                    source: "metrics",
                    ended: series.end.is_some(),
                    best_step: series.end,
                    updated_ms,
                    moved_ms: updated_ms,
                    s_per_step: series.s_per_step(),
                    loss: series
                        .train
                        .iter()
                        .rev()
                        .map(|row| row[1])
                        .find(|v| v.is_finite()),
                    val_loss: series
                        .val
                        .iter()
                        .rev()
                        .find(|row| row.split == "val")
                        .and_then(|row| row.values.get("loss_total").copied()),
                    lr: series
                        .train
                        .iter()
                        .rev()
                        .map(|row| row[2])
                        .find(|v| v.is_finite()),
                    ..Progress::default()
                });
            }
        }
        // A log only tells of a live entry: an old one may be anything's.
        // Its end lines count only here, where its `N/M` is the progress: a
        // job's log holds each of its steps, so a run folder's run may be
        // after the end line of the step before it
        if found.is_none()
            && (entry.status() == "running" || alive)
            && let Some(log) = entry.log.as_deref().map(|log| self.resolve(log))
            && let Some(text) = tail(&log, LOG_TAIL_BYTES)
            && let Some((step, total)) = log_progress(&text)
        {
            // It moved when the sampler first saw this step
            let moved = inner.moved.entry(entry.id.clone()).or_insert((step, now));
            if moved.0 != step {
                *moved = (step, now);
            }
            found = Some(Progress {
                step,
                total: Some(total),
                source: "log",
                ended: log_done(&text),
                updated_ms: modified_ms(&log),
                moved_ms: Some(moved.1),
                ..Progress::default()
            });
        }
        let mut progress = found?;
        // The rate over the last minutes the sampler saw it
        let seen = inner.seen.entry(entry.id.clone()).or_default();
        if seen.back().is_some_and(|&(_, step)| step > progress.step) {
            // Started over
            seen.clear();
        }
        seen.push_back((now, progress.step));
        while seen
            .front()
            .is_some_and(|&(t, _)| now.saturating_sub(t) > RATE_WINDOW_MS)
        {
            seen.pop_front();
        }
        let first = seen.iter().find(|&&(_, step)| step < progress.step);
        progress.rate = match first {
            Some(&(t, step)) if now > t + 15_000 => {
                Some((progress.step - step) / ((now - t) as f64 / 1000.0))
            }
            _ => progress.s_per_step.map(|s| 1.0 / s),
        };
        // No ETA for a run that is over, or that does something else now
        let stalled = progress
            .moved_ms
            .is_some_and(|t| now.saturating_sub(t) > STALL.as_millis() as u64);
        if let (Some(total), Some(rate)) = (progress.total, progress.rate)
            && rate > 0.0
            && total >= progress.step
            && alive
            && !progress.ended
            && !stalled
        {
            progress.eta_ms = Some(now + ((total - progress.step) / rate * 1000.0) as u64);
        }
        Some(progress)
    }

    /// The state the page shows; with `since`, the samples after it too
    fn state(&self, since: Option<u64>) -> Value {
        let mut inner = self.inner();
        // A page asking right after a write sees it
        self.reload_queue(&mut inner);
        let order = run_order(&inner.queue.entries);
        // What each runner takes next: this host's GPU and CPU, and the
        // VM's GPU when its runner has written a file
        let mut runners = vec![format!("gpu:{LOCAL}"), String::from("cpu")];
        if inner.remote.is_some() {
            runners.push(format!("gpu:{REMOTE_HOST}"));
        }
        let next: Vec<(String, usize)> = runners
            .into_iter()
            .filter_map(|device| {
                let i = next_for(&inner.queue.entries, &device)?;
                Some((device, i))
            })
            .collect();
        let remote = inner.remote.as_ref();
        let entries: Vec<Value> = inner
            .queue
            .entries
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let time = |t: &Option<String>| t.as_deref().and_then(parse_time);
                let spans = inner.spans.get(&entry.id);
                let machine = entry.machine();
                json!({
                    "id": entry.id,
                    "title": entry.title,
                    "why": entry.why,
                    "status": entry.status(),
                    "priority": entry.priority,
                    "rank": order.iter().position(|&j| j == i).map(|r| r + 1),
                    "next_on": next
                        .iter()
                        .filter(|(_, j)| *j == i)
                        .map(|(device, _)| device)
                        .collect::<Vec<_>>(),
                    "waits": waits_for(&inner.queue.entries, entry),
                    "group": entry.group,
                    "owner": entry.owner,
                    "device": entry.device,
                    "host": entry.host,
                    "host_pid": entry.host_pid,
                    "command": entry.command,
                    // Another machine: whether its runner's file tells of
                    // it (fresh, and written since it started: a runner
                    // copies a job's data over before it writes again), and
                    // says it runs it
                    "remote": machine.map(|host| {
                        let file = remote.filter(|remote| remote.host == host);
                        let started = time(&entry.started);
                        let known = file.is_some_and(|remote| {
                            remote.fresh
                                && remote.at_ms.zip(started).is_none_or(|(at, started)| {
                                    i64::try_from(at).is_ok_and(|at| at >= started)
                                })
                        });
                        json!({
                            "host": host,
                            "known": known,
                            "running": file.and_then(Remote::running) == Some(entry.id.as_str()),
                        })
                    }),
                    "pid": entry.pid,
                    "pgid": entry.pgid,
                    "match": entry.pattern,
                    "run_dir": entry.run_dir,
                    "log": entry.log,
                    "queued_ms": time(&entry.queued),
                    "started_ms": time(&entry.started),
                    "ended_ms": time(&entry.ended),
                    "eta_ms": time(&entry.eta),
                    "result_summary": entry.result_summary,
                    "next_step": entry.next_step,
                    "notes": entry.notes.iter().map(|note| json!({
                        "at_ms": parse_time(&note.at),
                        "text": note.text,
                    })).collect::<Vec<_>>(),
                    "live": inner.live.get(&entry.id),
                    "progress": inner.progress.get(&entry.id),
                    "seen": spans.map(|spans| json!({
                        "first_ms": spans.first().map(|s| s.0),
                        "last_ms": spans.last().map(|s| s.1),
                    })),
                })
            })
            .collect();
        let samples: Option<Vec<Value>> = since.map(|since| {
            inner
                .samples
                .iter()
                .filter(|s| s.t_ms > since)
                .map(Sample::row)
                .collect()
        });
        json!({
            "now_ms": unix_ms(),
            "sample_ms": SAMPLE_EVERY.as_millis() as u64,
            "stall_ms": STALL.as_millis() as u64,
            "gpu": inner.gpu,
            "gpu_error": inner.gpu_error,
            "gpu_procs": inner.gpu_procs,
            "remote": inner.remote,
            "cpu": {
                "percent": inner.cpu,
                "cores": inner.cores,
                "load": inner.load,
            },
            "memory": inner.memory.map(|[total, available, swap_total, swap_free]| json!({
                "total": total,
                "available": available,
                "swap_total": swap_total,
                "swap_free": swap_free,
            })),
            "queue": {
                "path": self.settings.queue,
                "updated_ms": inner.queue.updated.as_deref().and_then(parse_time),
                "error": inner.queue_error,
                "problems": inner.queue.problems,
                "entries": entries,
            },
            "samples": samples,
            "sampling_since_ms": inner.samples.front().map(|s| s.t_ms),
        })
    }

    /// The samples of the last `minutes`, averaged down, and when each
    /// entry was seen running
    fn timeline(&self, minutes: u64) -> Value {
        let inner = self.inner();
        let now = unix_ms();
        let window = minutes.clamp(1, KEEP.as_secs() / 60) * 60_000;
        let from = now.saturating_sub(window);
        let samples: Vec<Sample> = inner
            .samples
            .iter()
            .filter(|s| s.t_ms >= from)
            .cloned()
            .collect();
        let rows: Vec<Value> = downsample(&samples, MAX_POINTS)
            .iter()
            .map(Sample::row)
            .collect();
        let spans: Map<String, Value> = inner
            .spans
            .iter()
            .filter_map(|(id, spans)| {
                let shown: Vec<Value> = spans
                    .iter()
                    .filter(|&&(_, end)| end >= from)
                    .map(|&(start, end)| json!([start, end]))
                    .collect();
                (!shown.is_empty()).then(|| (id.clone(), Value::Array(shown)))
            })
            .collect();
        json!({
            "now_ms": now,
            "from_ms": from,
            "sample_ms": SAMPLE_EVERY.as_millis() as u64,
            "sampling_since_ms": inner.samples.front().map(|s| s.t_ms),
            "samples": rows,
            "spans": spans,
        })
    }

    /// The entry `id` of the queue
    fn entry(&self, id: &str) -> Result<Entry, Status> {
        let mut inner = self.inner();
        self.reload_queue(&mut inner);
        inner
            .queue
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .cloned()
            .ok_or_else(|| Status(StatusCode::NOT_FOUND, anyhow::anyhow!("no entry {id}")))
    }

    /// An entry's run folder: its loss curve, validation rows and args
    fn run(&self, id: &str) -> Result<Value, Status> {
        let entry = self.entry(id)?;
        let dir = entry
            .run_dir
            .as_deref()
            .map(|dir| self.resolve(dir))
            .context("the entry names no run folder")?;
        let mut inner = self.inner();
        let series = inner.runs.entry(dir.clone()).or_default();
        series.refresh(&dir);
        let round = |v: f64| {
            if v.is_finite() {
                json!((v * 1e5).round() / 1e5)
            } else {
                Value::Null
            }
        };
        let curve: Vec<Value> = series
            .curve(MAX_CURVE)
            .iter()
            .map(|row| json!([row[0], round(row[1]), row[2], round(row[3])]))
            .collect();
        Ok(json!({
            "id": entry.id,
            "run_dir": dir,
            "found": dir.join("metrics.jsonl").is_file(),
            "total": series.total(),
            "step": series.step(),
            "s_per_step": series.s_per_step(),
            "train_rows": series.train.len(),
            "train": curve,
            "val": series.val.iter().map(|row| json!({
                "step": row.step,
                "split": row.split,
                "values": VAL_KEYS
                    .iter()
                    .filter_map(|key| Some((key.to_string(), round(*row.values.get(*key)?))))
                    .collect::<Map<String, Value>>(),
            })).collect::<Vec<_>>(),
            "args": series.args,
        }))
    }

    /// The last lines of an entry's log
    fn log(&self, id: &str) -> Result<Value, Status> {
        let entry = self.entry(id)?;
        let path = entry
            .log
            .as_deref()
            .map(|log| self.resolve(log))
            .context("the entry names no log")?;
        let text = tail(&path, LOG_TAIL_BYTES)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let lines: Vec<String> = text.lines().map(clean_line).collect();
        let lines = lines[lines.len().saturating_sub(LOG_LINES)..].to_vec();
        Ok(json!({ "path": path, "lines": lines, "modified_ms": modified_ms(&path) }))
    }

    fn get(&self, path: &str, query: &HashMap<String, String>) -> Result<Value, Status> {
        let text = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
        match path {
            "state" => Ok(self.state(text("since").parse().ok())),
            "timeline" => Ok(self.timeline(text("minutes").parse().unwrap_or(180))),
            "run" => self.run(text("id")),
            "log" => self.log(text("id")),
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }

    fn post(&self, path: &str, body: &[u8]) -> Result<Value, Status> {
        match path {
            "order" => {
                #[derive(Deserialize)]
                struct Order {
                    order: Vec<String>,
                }
                let order: Order =
                    serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad order: {e}"))?;
                let changed = write_order(&self.settings.queue, &order.order)?;
                if changed {
                    log::info!("Pipeline: queue reordered from the page");
                }
                Ok(self.state(None))
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }
}

/// When a file was last written, Unix ms
fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    let since = modified.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(since.as_millis() as u64)
}

/// `pid` and the processes under it
fn descendants(procs: &[ProcStat], pid: u32) -> Vec<u32> {
    let mut found = vec![pid];
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        found.extend(
            procs
                .iter()
                .filter(|p| p.ppid == parent && p.pid != parent)
                .map(|p| p.pid),
        );
        i += 1;
    }
    found
}

/// An error with the status to answer it with
pub struct Status(StatusCode, anyhow::Error);

impl From<anyhow::Error> for Status {
    fn from(e: anyhow::Error) -> Self {
        Status(StatusCode::BAD_REQUEST, e)
    }
}

/// The Pipeline's routes under `/api/pipeline/`
pub fn routes(pipeline: Arc<Pipeline>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || warp::path("api").and(warp::path("pipeline"));
    let reader = Arc::clone(&pipeline);
    let get = warp::get()
        .and(base())
        .and(warp::path::tail())
        .and(warp::query::<HashMap<String, String>>())
        .and_then(
            move |tail: warp::path::Tail, query: HashMap<String, String>| {
                let pipeline = Arc::clone(&reader);
                blocking(move || pipeline.get(tail.as_str(), &query))
            },
        );
    let post = warp::post()
        .and(base())
        .and(warp::path::tail())
        .and(warp::body::content_length_limit(BODY_LIMIT))
        .and(warp::body::bytes())
        .and_then(
            move |tail: warp::path::Tail, body: warp::hyper::body::Bytes| {
                let pipeline = Arc::clone(&pipeline);
                blocking(move || pipeline.post(tail.as_str(), &body))
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
        .header("cache-control", "no-store")
        .body(value.to_string().into_bytes())
        .unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEUE: &str = r#"{
  "version": 1,
  "updated": "2026-09-29T13:20:00-07:00",
  "entries": [
    {"id": "a", "title": "A", "status": "done", "ended": "2026-09-29T12:46:49-07:00"},
    {"id": "b", "title": "B", "status": "queued", "custom": [1, 2]},
    {"id": "c", "title": "C", "status": "Queued", "priority": 5},
    {"id": "d", "title": "D", "status": "paused"},
    {"id": "e", "title": "E", "status": "running", "pgid": 12, "host_pid": 5,
     "command": "python -m x", "host": "win11", "after": ["a"]},
    {"id": "f", "pid": "not a number"}
  ],
  "owner": "day agent"
}"#;

    #[test]
    fn queue_reads_what_it_can() {
        let queue = parse_queue(QUEUE).unwrap();
        let ids: Vec<&str> = queue.entries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d", "e"]);
        assert_eq!(queue.problems.len(), 1);
        assert!(queue.problems[0].starts_with("entry 6:"));
        // Fields it does not know are kept
        assert_eq!(queue.entries[1].extra["custom"], json!([1, 2]));
        assert_eq!(queue.entries[2].status(), "queued");
        assert!(parse_queue("[]").is_err());
        let twice = r#"{"entries": [{"id": "x"}, {"id": "x"}, {"title": "no id"}]}"#;
        let queue = parse_queue(twice).unwrap();
        assert_eq!(queue.entries.len(), 1);
        assert_eq!(queue.problems.len(), 2);
    }

    #[test]
    fn waiting_entries_run_by_priority_then_file_order() {
        let queue = parse_queue(QUEUE).unwrap();
        let order: Vec<&str> = run_order(&queue.entries)
            .iter()
            .map(|&i| queue.entries[i].id.as_str())
            .collect();
        // c has priority 5; b and d have none (0) and keep the file's order
        assert_eq!(order, ["c", "b", "d"]);
    }

    #[test]
    fn reordering_writes_priorities_and_keeps_the_rest() {
        let now = "2026-09-29T14:00:00-07:00";
        let text = reorder(QUEUE, &["d".into(), "b".into()], now)
            .unwrap()
            .unwrap();
        let queue = parse_queue(&text).unwrap();
        let order: Vec<&str> = run_order(&queue.entries)
            .iter()
            .map(|&i| queue.entries[i].id.as_str())
            .collect();
        // The named ones first, then the others as they were
        assert_eq!(order, ["d", "b", "c"]);
        let priority = |id: &str| queue.entries.iter().find(|e| e.id == id).unwrap().priority;
        assert_eq!(
            [priority("d"), priority("b"), priority("c")],
            [Some(3), Some(2), Some(1)]
        );
        // Done and running entries are untouched, the unreadable one kept
        assert_eq!(priority("a"), None);
        assert_eq!(priority("e"), None);
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["entries"][5], json!({"id": "f", "pid": "not a number"}));
        assert_eq!(doc["owner"], "day agent");
        assert_eq!(doc["updated"], now);
        assert_eq!(doc["entries"][1]["custom"], json!([1, 2]));
        // Fields in the helper's order: id first, then title, status
        let b = text.find(r#""id": "b""#).unwrap();
        assert!(text[b..].find("\"title\"").unwrap() < text[b..].find("\"status\"").unwrap());
        // The runners' fields too: after, host, command, pgid, host_pid
        let e = &text[text.find(r#""id": "e""#).unwrap()..];
        let at = |field: &str| e.find(&format!("\"{field}\"")).unwrap();
        assert!(at("after") < at("host") && at("host") < at("command"));
        assert!(at("command") < at("pgid") && at("pgid") < at("host_pid"));
        // The same order again changes nothing; running and unknown ids
        // are left out
        assert!(
            reorder(&text, &["d".into(), "e".into(), "zz".into()], now)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn order_is_written_under_the_lock() {
        let dir = std::env::temp_dir().join(format!("procon-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("queue.json");
        std::fs::write(&path, QUEUE).unwrap();
        let order = ["d".to_string(), "b".to_string()];
        assert!(matches!(write_order(&path, &order), Ok(true)));
        assert!(lock_path(&path).exists());
        let queue = parse_queue(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let first = run_order(&queue.entries)[0];
        assert_eq!(queue.entries[first].id, "d");
        // The same order again leaves the file alone
        assert!(matches!(write_order(&path, &order), Ok(false)));
        // No file, no queue
        assert!(write_order(&dir.join("none.json"), &order).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn times_read_with_or_without_an_offset() {
        assert_eq!(
            parse_time("2026-09-29T13:20:00-07:00"),
            Some(1_790_713_200_000)
        );
        assert_eq!(parse_time("2026-09-29T20:20:00Z"), Some(1_790_713_200_000));
        assert!(parse_time("2026-09-29 13:20").is_some());
        assert!(parse_time("2026-09-29T13:20:00.5").is_some());
        assert_eq!(parse_time("soon"), None);
    }

    #[test]
    fn nvidia_smi_lines() {
        let gpu = parse_gpu(
            "15, 4297, 12282, 55, 60.76, 220.00, 74, 2520, P2, NVIDIA GeForce RTX 4070 SUPER",
        )
        .unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 4070 SUPER");
        assert_eq!(gpu.util, Some(15.0));
        assert_eq!(gpu.mem_total_mib, Some(12282.0));
        assert_eq!(gpu.power_w, Some(60.76));
        assert_eq!(gpu.pstate, "P2");
        let unsupported = parse_gpu("[N/A], 1, 2, 3, [N/A], [N/A], [N/A], 4, P8, A, B").unwrap();
        assert_eq!(unsupported.util, None);
        assert_eq!(unsupported.name, "A, B");
        assert!(parse_gpu("1, 2").is_none());
        assert_eq!(
            parse_apps("2475228, 283\n929491, 2860\n\nbad\n"),
            [(2475228, 283.0), (929491, 2860.0)]
        );
    }

    #[test]
    fn proc_stat_lines() {
        let stat = "884470 (python (x)) S 1 884470 884470 0 -1 4194560 1 2 3 4 \
                    1500 250 0 0 20 0 30 0 123456 99999 5000 18446744073709551615";
        let p = parse_stat(stat).unwrap();
        assert_eq!(p.pid, 884470);
        assert_eq!(p.comm, "python (x)");
        assert_eq!((p.ppid, p.pgid), (1, 884470));
        assert_eq!(p.ticks, 1750);
        assert_eq!(p.start_ticks, 123456);
        assert_eq!(p.rss_pages, 5000);
        assert!(parse_stat("nonsense").is_none());
    }

    #[test]
    fn process_names() {
        let words = |text: &str| -> Vec<String> { text.split(' ').map(String::from).collect() };
        assert_eq!(
            short_name(
                &words("/w/.venv/bin/python -m agentzero.policy.tokens --out x"),
                "python"
            ),
            "agentzero.policy.tokens"
        );
        assert_eq!(
            short_name(
                &words("/w/.venv/bin/python3 /w/.venv/bin/agentzero-track-serve --port 7340"),
                ""
            ),
            "agentzero-track-serve"
        );
        assert_eq!(
            short_name(&words("/usr/bin/python /d/e_decode.py out.json"), ""),
            "e_decode.py"
        );
        assert_eq!(
            short_name(&words("uv run --quiet agentzero-predict x"), ""),
            "agentzero-predict"
        );
        assert_eq!(
            short_name(&words("node /x/gpu-watchdog.mjs 12"), ""),
            "gpu-watchdog.mjs"
        );
        assert_eq!(short_name(&words("/usr/bin/ffmpeg -i x"), ""), "ffmpeg");
        assert_eq!(short_name(&[], "kworker"), "kworker");
    }

    #[test]
    fn progress_from_logs() {
        assert_eq!(
            log_progress("x\n[987/1017] a job 15: 66/29 73/31 (10 s)\n"),
            Some((987.0, 1017.0))
        );
        assert_eq!(
            log_progress("step 29750/30000  loss 3.033\n  val 30000: turn r x/y 0.41/0.28\n"),
            Some((29750.0, 30000.0))
        );
        // Decimals, dates and counts over their total do not count
        assert_eq!(log_progress("r 0.42/0.28\n"), None);
        assert_eq!(log_progress("eggs 75/29\n"), None);
        assert_eq!(log_progress("v2/3 1/1\n"), None);
        assert_eq!(
            log_progress("packed 800/842 jobs (267 s)"),
            Some((800.0, 842.0))
        );
    }

    #[test]
    fn metrics_rows() {
        let mut series = RunSeries::default();
        for line in [
            r#"{"step": 1, "split": "warmup", "s": 24.2}"#,
            r#"{"step": 50, "split": "train", "lr": 7.5e-05, "s_per_step": 0.13, "loss_total": 6.6}"#,
            r#"{"step": 100, "split": "train", "lr": 0.00015, "s_per_step": 0.032, "loss_total": 3.9}"#,
            r#"{"step": 100, "split": "val", "loss_total": 5.4, "button_f1": 0.32}"#,
            r#"{"step": 100, "split": "val_azu", "loss_total": 3.05}"#,
            r#"{"step": 2000, "split": "thresholds", "y": 0.6}"#,
            "not json",
        ] {
            series.add_line(line.as_bytes());
        }
        assert_eq!(series.train.len(), 2);
        assert_eq!(series.val.len(), 2);
        assert_eq!(series.val[0].values["button_f1"], 0.32);
        assert_eq!(series.step(), Some(100.0));
        assert!((series.s_per_step().unwrap() - 0.13).abs() < 1e-9);
        series.args = Some(json!({"steps": 30000}));
        assert_eq!(series.total(), Some(30000.0));
    }

    #[test]
    fn metrics_are_read_as_they_grow() {
        let dir = std::env::temp_dir().join(format!("procon-pipeline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let metrics = dir.join("metrics.jsonl");
        std::fs::write(
            &metrics,
            "{\"step\": 10, \"split\": \"train\", \"loss_total\": 2}\n{\"step\": 20, \"spl",
        )
        .unwrap();
        std::fs::write(dir.join("args.json"), r#"{"steps": 40}"#).unwrap();
        let mut series = RunSeries::default();
        series.refresh(&dir);
        // The half-written line waits
        assert_eq!(series.train.len(), 1);
        assert_eq!(series.total(), Some(40.0));
        std::fs::write(
            &metrics,
            "{\"step\": 10, \"split\": \"train\", \"loss_total\": 2}\n\
             {\"step\": 20, \"split\": \"train\", \"loss_total\": 1}\n",
        )
        .unwrap();
        series.refresh(&dir);
        assert_eq!(series.train.len(), 2);
        assert_eq!(series.step(), Some(20.0));
        // Written anew, shorter: read from the start
        std::fs::write(&metrics, "{\"step\": 5, \"split\": \"train\"}\n").unwrap();
        series.refresh(&dir);
        assert_eq!(series.step(), Some(5.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn curves_and_timelines_are_averaged_down() {
        let mut series = RunSeries::default();
        for step in 1..=1000 {
            series.train.push([step as f64, 1.0, 0.1, 0.03]);
        }
        let curve = series.curve(100);
        assert!(curve.len() <= 100);
        assert_eq!(curve.last().unwrap()[0], 1000.0);
        let samples: Vec<Sample> = (0..1000)
            .map(|i| Sample {
                t_ms: i * 5000,
                gpu: GpuSample {
                    util: if i % 2 == 0 { 100.0 } else { 0.0 },
                    mem_mib: i as f64,
                    ..GpuSample::UNKNOWN
                },
                ..Sample::default()
            })
            .collect();
        let down = downsample(&samples, 100);
        assert!(down.len() <= 101, "{}", down.len());
        assert_eq!(down.last().unwrap().t_ms, 999 * 5000);
        // The mean utilization, the most memory; nothing read stays unknown
        assert!(down.iter().all(|s| (s.gpu.util - 50.0).abs() <= 10.0));
        assert_eq!(down.last().unwrap().gpu.mem_mib, 999.0);
        assert!(down.iter().all(|s| s.remote.util.is_nan()));
        assert_eq!(downsample(&samples[..10], 100).len(), 10);
    }

    #[test]
    fn merged_samples_keep_each_entry_and_its_cpu() {
        let sample = |t_ms: u64, running: &[(&str, f64)]| Sample {
            t_ms,
            cpu: 20.0,
            running: running
                .iter()
                .map(|(id, cores)| (String::from(*id), *cores))
                .collect(),
            ..Sample::default()
        };
        let merged = merge(&[
            sample(0, &[("b", 1.0)]),
            sample(5000, &[("a", f64::NAN), ("b", 3.0)]),
            sample(10000, &[]),
        ]);
        assert_eq!(merged.t_ms, 10000);
        assert_eq!(merged.cpu, 20.0);
        // Every entry seen, with its mean CPU while it ran; one on another
        // machine stays unknown
        assert_eq!(merged.running.len(), 2);
        assert_eq!(merged.running[0].0, "a");
        assert!(merged.running[0].1.is_nan());
        assert_eq!(merged.running[1], (String::from("b"), 2.0));
    }

    #[test]
    fn history_survives_the_disk_and_is_compacted() {
        let hour = 3_600_000;
        let now = 30 * 24 * hour;
        let sample = |t_ms: u64, ids: &[&str]| Sample {
            t_ms,
            gpu: GpuSample {
                util: 40.0,
                mem_mib: 1000.0,
                ..GpuSample::UNKNOWN
            },
            running: ids.iter().map(|id| (String::from(*id), 1.5)).collect(),
            ..Sample::default()
        };
        let line = sample(5, &["a"]).line();
        let back = Sample::parse_line(&line).unwrap();
        assert_eq!(back.t_ms, 5);
        assert!(back.gpu.power_w.is_nan());
        assert_eq!(back.running, [(String::from("a"), 1.5)]);
        // A torn last line is skipped
        assert!(Sample::parse_line(&line[..line.len() - 3]).is_none());

        let mut samples = vec![sample(now - 8 * 24 * hour, &[])];
        // Two minutes a day ago, every 5 s: one sample a minute
        let old = now - 24 * hour;
        samples.extend((0..24).map(|i| sample(old + i * 5000, if i == 3 { &["b"] } else { &[] })));
        samples.extend((0..10).map(|i| sample(now - i * 5000, &[])));
        let kept = compact(samples, now);
        assert_eq!(kept.len(), 2 + 10);
        assert_eq!(kept[0].running, [(String::from("b"), 1.5)]);
        assert!(kept.windows(2).all(|w| w[0].t_ms <= w[1].t_ms));
        assert_eq!(kept.last().unwrap().t_ms, now);
    }

    #[test]
    fn history_lines_of_both_versions_read() {
        // A line of the first version: eight numbers, then the ids
        let old = r#"[1790754500728,86,4774,1916,73,188.2,60.6,21551,["g-present-dropout"]]"#;
        let sample = Sample::parse_line(old).unwrap();
        assert_eq!(sample.gpu.util, 86.0);
        assert_eq!(sample.gpu.jobs_mib, 1916.0);
        assert_eq!(sample.cpu, 60.6);
        assert_eq!(sample.ram_mib, 21551.0);
        assert!(sample.remote.util.is_nan() && sample.load.is_nan());
        assert_eq!(sample.running.len(), 1);
        assert!(sample.running[0].1.is_nan());
        // A line of this version keeps the ids at index 8, so the first
        // version's reader still finds its numbers and running entries
        let new = Sample {
            t_ms: 7,
            gpu: GpuSample {
                util: 90.0,
                ..GpuSample::UNKNOWN
            },
            remote: GpuSample {
                util: 37.0,
                mem_mib: 3011.0,
                temp_c: 70.0,
                power_w: 105.0,
                ..GpuSample::UNKNOWN
            },
            cpu: 34.2,
            load: 6.68,
            ram_mib: 20000.0,
            running: vec![
                (String::from("g-chunk-train"), f64::NAN),
                (String::from("g-present-dropout"), 1.61),
            ],
        };
        let line = new.line();
        let values: Vec<Value> = serde_json::from_str(&line).unwrap();
        assert_eq!(values[1], json!(90));
        assert_eq!(values[8], json!(["g-chunk-train", "g-present-dropout"]));
        assert_eq!(values[14], json!({"g-present-dropout": 1.61}));
        let back = Sample::parse_line(&line).unwrap();
        assert_eq!(back.remote.util, 37.0);
        assert_eq!(back.remote.power_w, 105.0);
        assert!(back.remote.jobs_mib.is_nan());
        assert_eq!(back.load, 6.68);
        assert!(back.running[0].1.is_nan());
        assert_eq!(back.running[1].1, 1.61);
        // The page's row: the numbers, then the CPU of each entry
        let row = new.row();
        assert_eq!(row[8], json!(37));
        assert_eq!(row[12], json!(6.68));
        assert_eq!(row[13], json!({"g-present-dropout": 1.61}));
    }

    #[test]
    fn remote_gpu_files_tell_only_while_fresh() {
        let text = r#"{
  "host": "win11",
  "at": "2026-09-30T00:46:45-07:00",
  "ok": true,
  "gpu": {
    "name": "NVIDIA GeForce RTX 4080 SUPER",
    "utilization_percent": 37.0,
    "memory_used_mib": 3011.0,
    "memory_total_mib": 16376.0,
    "temperature_c": 70.0,
    "power_w": 105.0
  },
  "processes": [{"pid": 20832, "name": "python.exe", "memory_mib": null}],
  "job": {"id": "g-chunk-train", "host_pid": 12128},
  "runner": {"pid": 2115117, "hold": false}
}"#;
        let at = parse_time("2026-09-30T00:46:45-07:00").unwrap() as u64;
        let remote = parse_remote("win11", text, at + 8_000, |pid| pid == 2115117);
        assert!(remote.fresh && remote.runner && !remote.hold);
        assert_eq!(remote.at_ms, Some(at));
        let gpu = remote.gpu.as_ref().unwrap();
        assert_eq!(gpu.name, "NVIDIA GeForce RTX 4080 SUPER");
        assert_eq!(gpu.util, Some(37.0));
        assert_eq!(gpu.mem_total_mib, Some(16376.0));
        assert_eq!(remote.processes[0].name, "python.exe");
        assert_eq!(remote.processes[0].memory_mib, None);
        assert_eq!(remote.running(), Some("g-chunk-train"));
        assert_eq!(remote.job_pid, Some(12128));
        // Stale: the job it names says nothing of now
        let stale = parse_remote("win11", text, at + 10 * 60_000, |_| false);
        assert!(!stale.fresh && !stale.runner);
        assert_eq!(stale.running(), None);
        // The runner could not reach the VM
        let unreachable = r#"{"host": "win11", "at": "2026-09-30T00:46:45-07:00",
            "ok": false, "error": "ssh: connect timed out", "job": null}"#;
        let remote = parse_remote("win11", unreachable, at, |_| true);
        assert!(remote.fresh && remote.gpu.is_none());
        assert_eq!(remote.error.as_deref(), Some("ssh: connect timed out"));
        assert_eq!(remote.running(), None);
        // A file that does not read
        let broken = parse_remote("win11", "{", at, |_| true);
        assert!(!broken.fresh && broken.error.is_some());
    }

    #[test]
    fn entries_run_where_their_host_or_device_says() {
        let entry = |host: Option<&str>, device: Option<&str>| Entry {
            id: String::from("x"),
            host: host.map(String::from),
            device: device.map(String::from),
            ..Entry::default()
        };
        assert_eq!(entry(Some("win11"), None).machine(), Some("win11"));
        assert_eq!(entry(None, Some("gpu:win11")).machine(), Some("win11"));
        assert_eq!(entry(Some("linux"), Some("gpu:win11")).machine(), None);
        assert_eq!(entry(None, Some("gpu:linux")).machine(), None);
        assert_eq!(entry(None, Some("gpu")).machine(), None);
        assert_eq!(entry(None, None).machine(), None);
        assert!(fits(None, "gpu:win11") && fits(Some("gpu"), "gpu:linux"));
        assert!(fits(Some("gpu:win11"), "gpu:win11"));
        assert!(!fits(Some("gpu:win11"), "gpu:linux"));
        assert!(!fits(Some("cpu"), "gpu:linux") && fits(Some("cpu"), "cpu"));
        assert!(!fits(Some("gpu"), "cpu"));
    }

    #[test]
    fn each_runner_takes_the_first_entry_it_can() {
        let queue = parse_queue(
            r#"{"entries": [
              {"id": "held", "status": "queued", "priority": 9, "device": "gpu:linux",
               "after": ["train"]},
              {"id": "train", "status": "running", "device": "gpu:win11", "host": "win11"},
              {"id": "no-command", "status": "queued", "priority": 8, "device": "gpu"},
              {"id": "anywhere", "status": "queued", "priority": 7, "device": "gpu",
               "command": "python -m x"},
              {"id": "cpu-job", "status": "queued", "priority": 6, "device": "cpu"},
              {"id": "gone-after", "status": "queued", "priority": 5, "after": "missing"},
              {"id": "paused", "status": "paused", "priority": 10}
            ]}"#,
        )
        .unwrap();
        let entries = &queue.entries;
        let id = |i: Option<usize>| i.map(|i| entries[i].id.as_str());
        // `held` waits for a running entry; the VM's GPU needs a command
        assert_eq!(waits_for(entries, &entries[0]), ["train"]);
        assert_eq!(id(next_for(entries, "gpu:linux")), Some("no-command"));
        assert_eq!(id(next_for(entries, "gpu:win11")), Some("anywhere"));
        assert_eq!(id(next_for(entries, "cpu")), Some("cpu-job"));
        // An id no longer in the queue holds nothing back
        assert!(waits_for(entries, &entries[5]).is_empty());
    }

    #[test]
    fn runs_end_by_their_closing_rows() {
        let mut series = RunSeries::default();
        for line in [
            r#"{"step": 500, "split": "train", "loss_total": 3.5}"#,
            r#"{"step": 600, "split": "train", "loss_total": 3.4}"#,
            r#"{"step": 600, "split": "val", "loss_total": 5.0, "keyframe_button_acc": 0.4}"#,
        ] {
            series.add_line(line.as_bytes());
        }
        assert_eq!(series.end, None);
        // The policy's trainer stopped early: its tuned thresholds, at the
        // best checkpoint's step
        series.add_line(br#"{"step": 100, "split": "thresholds", "zr": 0.55}"#);
        assert_eq!(series.end, Some(100.0));
        assert_eq!(series.step(), Some(600.0));
        assert_eq!(series.val[0].values["keyframe_button_acc"], 0.4);
        // Trained again in the same folder: not over any more
        series.add_line(br#"{"step": 650, "split": "train", "loss_total": 3.3}"#);
        assert_eq!(series.end, None);
        // The IDM's trainer: its tuned validations of best.pt, then last.pt
        let mut idm = RunSeries::default();
        idm.add_line(br#"{"step": 7555, "split": "train", "loss_total": 1.0}"#);
        idm.add_line(br#"{"step": 7000, "split": "val-tuned", "file": "best.pt"}"#);
        idm.add_line(br#"{"step": 7555, "split": "val-tuned", "file": "last.pt"}"#);
        assert_eq!(idm.end, Some(7000.0));
    }

    #[test]
    fn logs_tell_when_a_trainer_is_done() {
        let early = "step   500/2000  loss 3.5\n  val   600: loss 5.0\n\
                     no better loss in 5 validations; stopping\n\
                     done; best validation loss 4.399 (step 100); runs/policy/x\n";
        assert!(log_done(early));
        assert_eq!(log_progress(early), Some((500.0, 2000.0)));
        // A later run's steps after an earlier one's end
        let again = format!("{early}step   250/12000  loss 3.4\n");
        assert!(!log_done(&again));
        assert!(!log_done("step 250/2000\n"));
        // Stopping early, before the last line: its best checkpoint is
        // validated again meanwhile (the IDM's words too)
        assert!(log_done(
            "step 500/2000\nno better loss in 5 validations; stopping\n"
        ));
        assert!(log_done(
            "step 5000/7555\nstopping: no new best in 4 validations\n"
        ));
    }

    #[test]
    fn log_lines_are_cleaned() {
        assert_eq!(clean_line("50%\r75%\r100% done  "), "100% done");
        assert_eq!(clean_line("\u{1b}[32mok\u{1b}[0m"), "ok");
    }
}
