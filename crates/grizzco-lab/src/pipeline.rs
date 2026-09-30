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
//! takes `host: win11`, writes the VM's GPU, CPU and memory to
//! `win11/gpu.json` beside the queue file every 10 s ([`Remote`]; a runner
//! of before writes the GPU alone, without its power limit, fan and clock),
//! and copies each job's log and metrics back into the same paths here
//! every minute. An entry on the VM has no process here: whether it runs
//! is what that file says, while it is fresh ([`REMOTE_STALE`]). Each
//! runner takes the first queued entry that fits its device and waits for
//! nothing ([`next_for`]).
//!
//! The page reorders the waiting entries by dragging them: `POST order`
//! writes their priorities into the file, holding the lock the helper takes
//! (`<queue>.lock`, an exclusive `flock`) and writing atomically, with
//! every other field kept as it was ([`write_order`]). Nothing else here
//! writes.
//!
//! A sampler samples the machine every [`SAMPLE_EVERY`], read-only
//! ([`Sample`]): this host's GPU through `nvidia-smi` (utilization,
//! memory, temperature, power, and each compute process's memory), the
//! VM's GPU, CPU and memory from its file (unknown, never 0, while the file
//! is stale), CPU, load and memory from `/proc`, the processes of each
//! entry here with the CPU they took and its main one ([`Live`]), and
//! each live entry's progress from its run folder (`metrics.jsonl` rows
//! with `step` and `split`, `args.json` with `steps`) or else its log (the
//! last `N/M` in it), with the rate and ETA it saw. A run whose trainer
//! wrote its closing rows ([`END_SPLITS`]; for a run known only by its
//! log, printed that it stops, [`END_LINES`]) has ended, early when before
//! its last step; a live entry whose step has not moved for [`STALL`] does
//! something else now (an evaluation after training, say). Neither has an
//! ETA. What a live entry does is also read from its log
//! ([`log_activity`]): the step its job script started last (`== start
//! <name> (step N of the job) <date> <time>`, the agents' job scripts;
//! `== end <name>` closes it), whether a counter came after that start,
//! and the log's last line.
//!
//! The sampler also watches the disks the work lands on ([`Storage`]): the
//! Proxmox host's ZFS pools (`[pipeline] storage_host`, `pve`: every VM's
//! disk is a thin zvol on its `rpool`, and a full pool hangs the host and
//! both VMs), read once a minute over ssh without waiting for the answer
//! ([`PoolProbe`]), this host's `/`, and the win11 VM's `C:` from its
//! runner's file (`disk`). The watched pool's level ([`pool_level`], the
//! thresholds of AgentZero's storage guard) goes to the page, which shows
//! a banner here and a chip in every app while it is low.
//!
//! The sampler is a process of its own, `grizzco-lab sample`
//! ([`run_sampler`]), so the machine is followed while the lab is stopped
//! too; one at a time (it holds [`LOCK_FILE`], its pid in it). It appends
//! each sample to a log on this machine, [`HISTORY_FILE`] in the local
//! cache (`cuttlefish::store::cache_dir`, never the synced knowledge
//! folder), with the entries seen running then, the CPU each took and the
//! free space of each disk and the VM's CPU and memory ([`Sample::line`]),
//! and rewrites [`SNAPSHOT_FILE`] beside it with what it saw of now
//! ([`Snapshot`]: the GPUs, their processes, each entry's processes,
//! progress and what its log says it runs, the disks).
//! It rewrites the log at start and every hour ([`compact`]): samples older
//! than [`KEEP`] thinned to one a minute, those older than
//! [`HISTORY_KEEP`] dropped, a torn last line skipped. It exits when its
//! binary is built again, so the new build takes over.
//!
//! The lab samples nothing itself ([`Pipeline::start`]): it follows the
//! log (the last [`KEEP`] of it in memory for the timeline, new lines as
//! they come, all of it again once the compaction rewrote it) and the
//! snapshot, and starts a sampler, detached in a session of its own
//! (which outlives the lab, its Ctrl-C included; its log is [`SAMPLER_LOG`]),
//! whenever none holds the lock, at most once a [`SAMPLER_RETRY`]. A
//! snapshot older than [`SNAPSHOT_STALE`] says nothing of now.
//!
//! Endpoints under `/api/pipeline/`:
//!
//! - `GET state[?since=<ms>]`: the machine now, both GPUs and their
//!   processes, the VM's CPU and memory, the disks ([`Storage::to_json`]),
//!   the queue with each entry's processes, progress, runner and what its
//!   log says it runs, and with `since` the samples taken after it
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
use procon_core::dump::unix_ms;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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

/// The samples' log, in the cache folder
pub const HISTORY_FILE: &str = "pipeline-gpu.jsonl";

/// What the sampler saw last, in the cache folder, rewritten after every
/// sample ([`Snapshot`])
pub const SNAPSHOT_FILE: &str = "pipeline-now.json";

/// The lock the sampler holds for its life, its pid in it, in the cache
/// folder: one sampler at a time
pub const LOCK_FILE: &str = "pipeline-sampler.lock";

/// The log of a sampler the lab started, in the cache folder
pub const SAMPLER_LOG: &str = "pipeline-sampler.log";

/// A snapshot older than this says nothing of now: no sampler runs
pub const SNAPSHOT_STALE: Duration = Duration::from_secs(30);

/// The lab starts a sampler at most this often
pub const SAMPLER_RETRY: Duration = Duration::from_secs(30);

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

/// The Proxmox host whose pools are read, by default (`[pipeline]
/// storage_host`)
pub const STORAGE_HOST: &str = "pve";

/// What runs there, by default (`[pipeline] storage_command`)
pub const STORAGE_COMMAND: &str = "zpool list -Hp -o name,size,alloc,free,cap,frag";

/// The pool every VM's disk lives on, whose level the page shows
pub const WATCHED_POOL: &str = "rpool";

/// How often the pools are read
const STORAGE_EVERY: Duration = Duration::from_secs(60);

/// A reading of the pools not over by then is given up: a host whose pool
/// filled up may hang
const STORAGE_TIMEOUT: Duration = Duration::from_secs(30);

/// A reading of the pools older than this says nothing of now
const STORAGE_STALE: Duration = Duration::from_secs(5 * 60);

/// A GB as the thresholds count it (AgentZero's storage guard too)
pub const GB: u64 = 1_000_000_000;

/// The watched pool is low under this much free, or at [`LOW_CAP`] % used
/// and more: AgentZero's runners then start nothing new
pub const LOW_FREE: u64 = 300 * GB;

/// See [`LOW_FREE`]
pub const LOW_CAP: f64 = 85.0;

/// The watched pool is critical under this much free: AgentZero's guard
/// then stops the running jobs
pub const CRITICAL_FREE: u64 = 150 * GB;

/// Largest request body
const BODY_LIMIT: u64 = 64 << 10;

/// The Pipeline's settings, from `[pipeline]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The queue file agents keep
    pub queue: PathBuf,
    /// The folder of the sampler's files: [`HISTORY_FILE`],
    /// [`SNAPSHOT_FILE`], [`LOCK_FILE`], [`SAMPLER_LOG`]
    pub cache: PathBuf,
    /// The file the win11 runner writes the VM's GPU to: `win11/gpu.json`
    /// beside the queue file, as AgentZero's helper reads it
    pub remote: PathBuf,
    /// The host whose ZFS pools are read over ssh (empty: none), and the
    /// command it runs for them
    pub storage_host: String,
    pub storage_command: String,
    /// The sampler's binary and config file: the lab starts it when none
    /// runs, and the sampler names the config in its snapshot; `None`: the
    /// lab never starts one
    pub sampler: Option<(PathBuf, PathBuf)>,
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
            cache: cuttlefish::store::cache_dir(),
            remote,
            storage_host: config
                .storage_host
                .unwrap_or_else(|| String::from(STORAGE_HOST))
                .trim()
                .to_string(),
            storage_command: config
                .storage_command
                .unwrap_or_else(|| String::from(STORAGE_COMMAND)),
            sampler: None,
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
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
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
/// the GPU, its compute processes, the machine's CPU and memory (a runner
/// of before writes neither), the job it runs, and the runner
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
    cpu: Option<HostCpu>,
    #[serde(default)]
    memory: Option<HostMemory>,
    #[serde(default)]
    job: Option<RemoteJob>,
    #[serde(default)]
    runner: Option<RemoteRunner>,
    /// The VM's `C:`
    #[serde(default)]
    disk: Option<Disk>,
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
    power_limit_w: Option<f64>,
    fan_percent: Option<f64>,
    sm_clock_mhz: Option<f64>,
    #[serde(default)]
    pstate: Option<String>,
}

/// Another machine's CPU as its runner reads it: busy over the last
/// second, %, and its logical processors
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct HostCpu {
    pub percent: Option<f64>,
    pub cores: Option<u32>,
}

/// Another machine's memory as its runner reads it, in bytes
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct HostMemory {
    #[serde(default)]
    pub used: u64,
    #[serde(default)]
    pub total: u64,
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
    /// Why it holds for storage, when it does
    #[serde(default)]
    space: Option<String>,
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
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Remote {
    /// The machine's name in the queue (`host`, `gpu:<host>`)
    pub host: String,
    /// When its runner read the GPU, Unix ms
    pub at_ms: Option<u64>,
    /// Written within [`REMOTE_STALE`]
    pub fresh: bool,
    /// The reading, when the runner could take one (`name`, `util`,
    /// `mem_used_mib`, `mem_total_mib`, `temp_c`, `power_w`, and from a
    /// runner of now `power_limit_w`, `fan`, `sm_mhz`, `pstate`)
    pub gpu: Option<Gpu>,
    /// Why there is no reading: the runner could not reach the machine, or
    /// the file does not read
    pub error: Option<String>,
    pub processes: Vec<RemoteProc>,
    /// The machine's CPU and memory, when its runner reads them
    #[serde(default)]
    pub cpu: Option<HostCpu>,
    #[serde(default)]
    pub memory: Option<HostMemory>,
    /// The entry its runner runs there, and that job's process there
    pub job: Option<String>,
    pub job_pid: Option<u64>,
    /// Whether its runner still runs here (its pid is alive), and whether
    /// it holds new entries back (`HOLD`), and why when for storage
    pub runner: bool,
    pub hold: bool,
    pub space: Option<String>,
    /// The machine's system disk (`C:`), as its runner last read it
    pub disk: Option<Disk>,
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
        power_limit_w: gpu.power_limit_w,
        fan: gpu.fan_percent,
        sm_mhz: gpu.sm_clock_mhz,
        pstate: gpu.pstate.unwrap_or_default(),
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
        cpu: file.cpu.filter(|_| file.ok),
        memory: file.memory.filter(|memory| file.ok && memory.total > 0),
        job: file
            .job
            .as_ref()
            .map(|job| job.id.clone())
            .filter(|id| !id.is_empty()),
        job_pid: file.job.and_then(|job| job.host_pid),
        runner: runner.pid.is_some_and(alive),
        hold: runner.hold,
        space: runner.space.filter(|reason| !reason.trim().is_empty()),
        disk: file.disk.filter(|disk| disk.total > 0),
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

// ---------------------------------------------------------------- storage

/// A ZFS pool as `zpool list -Hp -o name,size,alloc,free,cap,frag` gives it
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Pool {
    pub name: String,
    /// Bytes
    pub size: u64,
    pub alloc: u64,
    pub free: u64,
    /// Used, %
    pub cap: f64,
    /// Fragmentation of its free space, %; `-` in zpool's answer is none
    pub frag: Option<f64>,
}

/// The pools of `zpool list -Hp -o name,size,alloc,free,cap,frag`'s lines
/// (tab-separated, exact bytes); lines that do not read are left out
pub fn parse_pools(text: &str) -> Vec<Pool> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
            let [name, size, alloc, free, cap, frag] = fields[..] else {
                return None;
            };
            let percent = |text: &str| text.trim_end_matches('%').parse::<f64>().ok();
            Some(Pool {
                name: name.to_string(),
                size: size.parse().ok()?,
                alloc: alloc.parse().ok()?,
                free: free.parse().ok()?,
                cap: percent(cap)?,
                frag: percent(frag),
            })
        })
        .collect()
}

/// A disk's free and total bytes
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Disk {
    pub free: u64,
    pub total: u64,
}

/// Where a pool stands: `critical` under [`CRITICAL_FREE`] free, `low`
/// under [`LOW_FREE`] or at [`LOW_CAP`] % used and more, else `ok` (the
/// levels of AgentZero's storage guard: `critical`, `hold`, `ok`)
pub fn pool_level(pool: &Pool) -> &'static str {
    if pool.free < CRITICAL_FREE {
        "critical"
    } else if pool.free < LOW_FREE || pool.cap >= LOW_CAP {
        "low"
    } else {
        "ok"
    }
}

/// Free space where the lab's work lands, as last read: the storage host's
/// pools, this host's `/` and the win11 VM's `C:`
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Storage {
    /// The host whose pools are read, empty for none
    pub host: String,
    pub pools: Vec<Pool>,
    /// When the pools were last read, Unix ms
    pub pools_ms: Option<u64>,
    /// Why the last reading failed, and when (they go on once a minute)
    pub error: Option<String>,
    pub failed_ms: Option<u64>,
    /// This host's `/`
    pub root: Option<Disk>,
    /// The win11 VM's `C:`, while its runner's file is fresh
    pub remote: Option<Disk>,
}

impl Storage {
    /// The pools, while their reading is fresh
    fn fresh_pools(&self, now: u64) -> &[Pool] {
        let stale = STORAGE_STALE.as_millis() as u64;
        match self.pools_ms {
            Some(at) if now.saturating_sub(at) <= stale => &self.pools,
            _ => &[],
        }
    }

    /// The watched pool's level ([`pool_level`]), `unknown` once a reading
    /// failed or lacks it and none is fresh; `None` without a storage host
    /// or before the first answer
    pub fn level(&self, now: u64) -> Option<&'static str> {
        if self.host.is_empty() {
            return None;
        }
        let watched = self
            .fresh_pools(now)
            .iter()
            .find(|pool| pool.name == WATCHED_POOL);
        match watched {
            Some(pool) => Some(pool_level(pool)),
            None if self.failed_ms.is_some() || self.pools_ms.is_some() => Some("unknown"),
            None => None,
        }
    }

    /// Free space in GB by disk, `<host>:<pool>` (while fresh), `linux:/`
    /// and `win11:C:`, as the samples keep it
    fn free_gb(&self, now: u64) -> Vec<(String, f64)> {
        let in_gb = |bytes: u64| bytes as f64 / GB as f64;
        let mut free: Vec<(String, f64)> = self
            .fresh_pools(now)
            .iter()
            .map(|pool| (format!("{}:{}", self.host, pool.name), in_gb(pool.free)))
            .collect();
        if let Some(root) = self.root {
            free.push((format!("{LOCAL}:/"), in_gb(root.free)));
        }
        if let Some(disk) = self.remote {
            free.push((format!("{REMOTE_HOST}:C:"), in_gb(disk.free)));
        }
        free
    }

    /// For the page: the readings, the watched pool, its level and the
    /// thresholds
    pub fn to_json(&self, now: u64) -> Value {
        json!({
            "host": self.host,
            "pools": self.fresh_pools(now),
            "pools_ms": self.pools_ms,
            "error": self.error,
            "failed_ms": self.failed_ms,
            "root": self.root,
            "remote": self.remote,
            "watched": WATCHED_POOL,
            "level": self.level(now),
            "low_free": LOW_FREE,
            "low_cap": LOW_CAP,
            "critical_free": CRITICAL_FREE,
        })
    }
}

/// The pools' reading over ssh: started at most once a [`STORAGE_EVERY`]
/// and looked at by each sample without waiting, given up after
/// [`STORAGE_TIMEOUT`]
#[derive(Debug, Default)]
struct PoolProbe {
    /// The ssh under way, and when it started
    running: Option<(Child, Instant)>,
    /// When the last one started
    started: Option<Instant>,
}

impl PoolProbe {
    /// The reading that ended since the last look, if one did; starts the
    /// next when it is due
    fn poll(&mut self, host: &str, command: &str) -> Option<Result<Vec<Pool>>> {
        if let Some((child, started)) = self.running.as_mut() {
            let answer = match child.try_wait() {
                Ok(Some(status)) => {
                    let mut out = String::new();
                    let mut err = String::new();
                    if let Some(mut pipe) = child.stdout.take() {
                        let _ = pipe.read_to_string(&mut out);
                    }
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut err);
                    }
                    let last = err.lines().rev().find(|l| !l.trim().is_empty());
                    match parse_pools(&out) {
                        _ if !status.success() => Err(anyhow::anyhow!(
                            "ssh {host}: {}",
                            last.map_or_else(|| status.to_string(), str::to_string)
                        )),
                        pools if pools.is_empty() => {
                            Err(anyhow::anyhow!("ssh {host}: no pools in its answer"))
                        }
                        pools => Ok(pools),
                    }
                }
                Ok(None) if started.elapsed() < STORAGE_TIMEOUT => return None,
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    Err(anyhow::anyhow!(
                        "ssh {host}: no answer in {} s",
                        STORAGE_TIMEOUT.as_secs()
                    ))
                }
                Err(e) => Err(anyhow::Error::from(e).context("ssh")),
            };
            self.running = None;
            return Some(answer);
        }
        if self.started.is_some_and(|at| at.elapsed() < STORAGE_EVERY) {
            return None;
        }
        self.started = Some(Instant::now());
        let spawned = Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                host,
                command,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => {
                self.running = Some((child, Instant::now()));
                None
            }
            Err(e) => Some(Err(anyhow::Error::from(e).context("cannot run ssh"))),
        }
    }
}

/// A GB count for the log, `839.3 GB`
fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / GB as f64)
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

/// Most characters of a log's last line sent to the page
const LINE_CHARS: usize = 300;

/// What a live entry's log says it does now ([`log_activity`])
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Activity {
    /// The step its job script started last, while no end line of it
    /// followed (`== start <name> ...`), and when it started, Unix ms
    pub step: Option<String>,
    pub step_ms: Option<u64>,
    /// Whether an `N/M` came after that start: the log's counter is that
    /// step's, else an earlier one's
    pub counted: bool,
    /// The log's last line, and when the log was last written, Unix ms
    pub line: Option<String>,
    pub line_ms: Option<u64>,
}

/// The step a job script's line starts, `== start <name> (step N of the
/// job) <date> <time>` (the agents' scripts), and when, the local time at
/// its end; the win11 VM's own `== start <date> <time> in <folder>` starts
/// no step
fn step_start(line: &str) -> Option<(String, Option<u64>)> {
    let words: Vec<&str> = line.strip_prefix("== start ")?.split_whitespace().collect();
    if !words.get(1)?.starts_with('(') {
        return None;
    }
    let name = words[0].to_string();
    let when = words
        .len()
        .checked_sub(2)
        .filter(|&at| at > 0)
        .and_then(|at| parse_time(&words[at..].join(" ")))
        .and_then(|ms| u64::try_from(ms).ok());
    Some((name, when))
}

/// What a job's log (`text`, its last part) says it does now: the step its
/// script started last unless its `== end <name>` line came after, when,
/// whether a counter followed that start, and the last line (cleaned, at
/// most [`LINE_CHARS`]); `line_ms` is left to the caller
pub fn log_activity(text: &str) -> Activity {
    let mut activity = Activity::default();
    for line in text
        .lines()
        .map(clean_line)
        .filter(|l| !l.trim().is_empty())
    {
        if let Some((name, when)) = step_start(&line) {
            activity.step = Some(name);
            activity.step_ms = when;
            activity.counted = false;
        } else if let Some(name) = line.strip_prefix("== end ")
            && name.split_whitespace().next() == activity.step.as_deref()
        {
            activity.step = None;
            activity.step_ms = None;
            activity.counted = false;
        } else if activity.step.is_some() && log_progress(&line).is_some() {
            activity.counted = true;
        }
        activity.line = Some(line);
    }
    if let Some(line) = &mut activity.line
        && line.chars().count() > LINE_CHARS
    {
        *line = line.chars().take(LINE_CHARS).collect::<String>() + "…";
    }
    activity
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
    /// Free space by disk, in GB (see [`Storage`]): the storage host's
    /// pools (`pve:rpool`), this host's `/` (`linux:/`) and the VM's `C:`
    /// (`win11:C:`), those read
    pub free_gb: Vec<(String, f64)>,
    /// The win11 VM's CPU busy, %, and memory in use, MiB, while its
    /// runner's file is fresh and tells them
    pub remote_cpu: f64,
    pub remote_ram_mib: f64,
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
            free_gb: Vec::new(),
            remote_cpu: f64::NAN,
            remote_ram_mib: f64::NAN,
        }
    }
}

/// Where a line of the log on disk keeps the VM's CPU and memory, after
/// the CPU of each entry (14) and the free space of each disk (15): what
/// a version adds goes at the end
const LINE_REMOTE_CPU: usize = 16;
const LINE_REMOTE_RAM: usize = 17;

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

    /// The free space of each disk read, in GB: `{disk: GB}`
    fn free(&self) -> Value {
        let free: Map<String, Value> = self
            .free_gb
            .iter()
            .filter(|(_, gb)| gb.is_finite())
            .map(|(disk, gb)| (disk.clone(), rounded(*gb, 1)))
            .collect();
        Value::Object(free)
    }

    /// The VM's CPU, %, and memory, MiB
    fn remote_host(&self) -> [Value; 2] {
        [rounded(self.remote_cpu, 1), rounded(self.remote_ram_mib, 0)]
    }

    /// As a row for the page: its [`Self::numbers`], then the CPU of each
    /// entry ([`Self::cores`]), the free space of each disk
    /// ([`Self::free`]), and the VM's CPU and memory
    fn row(&self) -> Value {
        let mut values = self.numbers();
        values.push(self.cores());
        values.push(self.free());
        values.extend(self.remote_host());
        Value::Array(values)
    }

    /// As a line of the log on disk: its row with the ids of the entries
    /// seen running at index 8, where the log's first lines have them after
    /// their eight numbers, so each version reads the other's lines (and
    /// what a version adds goes at the end)
    pub fn line(&self) -> String {
        let mut values = self.numbers();
        let ids: Vec<&str> = self.running.iter().map(|(id, _)| id.as_str()).collect();
        values.insert(8, json!(ids));
        values.push(self.cores());
        values.push(self.free());
        values.extend(self.remote_host());
        Value::Array(values).to_string()
    }

    /// A line of the log on disk back ([`Self::line`], or an earlier
    /// version's line, whose remote GPU, load, CPU per entry, free space or
    /// VM's CPU and memory are not known); `None` for a torn or foreign
    /// line
    pub fn parse_line(line: &str) -> Option<Self> {
        let values: Vec<Value> = serde_json::from_str(line).ok()?;
        let free_gb = values
            .get(15)
            .and_then(Value::as_object)
            .map(|free| {
                free.iter()
                    .filter_map(|(disk, gb)| Some((disk.clone(), gb.as_f64()?)))
                    .collect()
            })
            .unwrap_or_default();
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
            free_gb,
            remote_cpu: number(LINE_REMOTE_CPU),
            remote_ram_mib: number(LINE_REMOTE_RAM),
        })
    }
}

/// Samples as one, at the last one's time: the mean utilization, power,
/// CPU and load, the most memory, temperature and RAM (of both machines),
/// every entry seen running with its mean CPU while it ran, and each
/// disk's least free space
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
    let mut free_gb: Vec<(String, f64)> = Vec::new();
    for (disk, gb) in bucket.iter().flat_map(|s| &s.free_gb) {
        match free_gb.iter_mut().find(|(seen, _)| seen == disk) {
            Some((_, least)) => *least = least.min(*gb),
            None => free_gb.push((disk.clone(), *gb)),
        }
    }
    Sample {
        t_ms: last.t_ms,
        gpu: gpu(|s| &s.gpu),
        remote: gpu(|s| &s.remote),
        cpu: mean(&|s| s.cpu),
        load: mean(&|s| s.load),
        ram_mib: most(&|s| s.ram_mib),
        running,
        free_gb,
        remote_cpu: mean(&|s| s.remote_cpu),
        remote_ram_mib: most(&|s| s.remote_ram_mib),
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
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct Live {
    pids: Vec<u32>,
    /// CPU over the last interval, % of one core
    cpu_percent: f64,
    rss_bytes: u64,
    gpu_mib: f64,
    /// When its oldest process started, Unix ms
    since_ms: u64,
    /// How it was found: `pgid`, `pid` or `match`
    found_by: String,
    /// Its main process's short name ([`short_name`]): the one holding
    /// the most GPU memory, else the one that took the most CPU lately,
    /// else the newest
    #[serde(default)]
    main: Option<String>,
}

/// An entry's progress: steps done of the total, and the rate seen
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct Progress {
    step: f64,
    total: Option<f64>,
    /// `metrics` (its run folder) or `log`
    source: String,
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

/// What the sampler saw of now, for the lab ([`SNAPSHOT_FILE`])
#[derive(Debug, Default, Deserialize, Serialize)]
struct Snapshot {
    /// When, Unix ms
    t_ms: u64,
    /// The sampler's pid, and the config file it reads (empty: unknown)
    pid: u32,
    #[serde(default)]
    config: String,
    gpu: Option<Gpu>,
    gpu_error: Option<String>,
    remote: Option<Remote>,
    cpu: Option<f64>,
    cores: usize,
    load: Option<[f64; 3]>,
    memory: Option<[u64; 4]>,
    gpu_procs: Vec<ProcInfo>,
    live: HashMap<String, Live>,
    progress: HashMap<String, Progress>,
    /// What each live entry's log says it does
    #[serde(default)]
    activity: HashMap<String, Activity>,
    storage: Storage,
}

/// A sampler the lab started, and when it last started one
#[derive(Default)]
struct Started {
    child: Option<Child>,
    at: Option<Instant>,
}

/// Everything the sampler keeps; in the lab, what it read of the
/// sampler's files
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
    /// What each live entry's log says it does
    activity: HashMap<String, Activity>,
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
    /// The lab: the log as read so far (its inode and the bytes read), the
    /// snapshot's file time as last read and when the sampler wrote it
    /// (Unix ms), and the sampler's config file it warned of
    history_read: (u64, u64),
    snapshot_seen: Option<SystemTime>,
    snapshot_ms: Option<u64>,
    other_config: Option<String>,
}

/// The Pipeline app: the sampler's findings and the queue
pub struct Pipeline {
    settings: Settings,
    inner: Mutex<Inner>,
    /// The disks as last read, apart from `inner`, so the dashboard's
    /// status never waits for the sampler
    storage: Mutex<Storage>,
    /// The pools' reading under way
    probe: Mutex<PoolProbe>,
}

impl Pipeline {
    fn new(settings: Settings) -> Self {
        Self {
            storage: Mutex::new(Storage {
                host: settings.storage_host.clone(),
                ..Storage::default()
            }),
            settings,
            inner: Mutex::new(Inner {
                cores: num_cpus::get(),
                ..Inner::default()
            }),
            probe: Mutex::default(),
        }
    }

    /// The lab's Pipeline: the sampler's files read back every
    /// [`SAMPLE_EVERY`] on a thread of its own, which starts a sampler
    /// whenever none runs (see the module docs)
    pub fn start(settings: Settings) -> Arc<Self> {
        let pipeline = Arc::new(Self::new(settings));
        let lab = Arc::clone(&pipeline);
        std::thread::Builder::new()
            .name(String::from("pipeline"))
            .spawn(move || {
                let mut started = Started::default();
                loop {
                    lab.keep_sampler(&mut started);
                    lab.refresh();
                    std::thread::sleep(SAMPLE_EVERY);
                }
            })
            .expect("thread");
        pipeline
    }

    /// Start a sampler when none holds the lock, at most once a
    /// [`SAMPLER_RETRY`]; reap the one started before once it ended
    fn keep_sampler(&self, started: &mut Started) {
        let Some((exe, config)) = &self.settings.sampler else {
            return;
        };
        if let Some(child) = started.child.as_mut()
            && let Ok(Some(status)) = child.try_wait()
        {
            log::warn!(
                "The Pipeline's sampler (pid {}) ended: {status}",
                child.id()
            );
            started.child = None;
        }
        let cache = &self.settings.cache;
        if sampler_runs(&cache.join(LOCK_FILE))
            || started.at.is_some_and(|at| at.elapsed() < SAMPLER_RETRY)
        {
            return;
        }
        started.at = Some(Instant::now());
        match spawn_sampler(exe, config, cache) {
            Ok(child) => {
                log::info!(
                    "Started the Pipeline's sampler (pid {}): {} sample --config {}; its log is {}",
                    child.id(),
                    exe.display(),
                    config.display(),
                    cache.join(SAMPLER_LOG).display()
                );
                started.child = Some(child);
            }
            Err(e) => log::warn!("Cannot start the Pipeline's sampler: {e:#}"),
        }
    }

    /// Read what the sampler wrote since the last look
    fn refresh(&self) {
        let mut inner = self.inner();
        self.read_snapshot(&mut inner);
        self.read_history(&mut inner);
    }

    /// The sampler's snapshot, when it changed: now's readings, and the
    /// disks
    fn read_snapshot(&self, inner: &mut Inner) {
        let path = self.settings.cache.join(SNAPSHOT_FILE);
        let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) else {
            return;
        };
        if inner.snapshot_seen == Some(modified) {
            return;
        }
        let read = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| Ok(serde_json::from_slice::<Snapshot>(&bytes)?));
        let snapshot = match read {
            Ok(snapshot) => snapshot,
            Err(e) => {
                log::debug!("pipeline: cannot read {}: {e:#}", path.display());
                return;
            }
        };
        inner.snapshot_seen = Some(modified);
        // A sampler of another config (a test lab's, say) is told of once
        let ours = self
            .settings
            .sampler
            .as_ref()
            .map(|(_, config)| config.display().to_string());
        if let Some(ours) = ours
            && !snapshot.config.is_empty()
            && snapshot.config != ours
            && inner.other_config.as_ref() != Some(&snapshot.config)
        {
            log::warn!(
                "The Pipeline's sampler (pid {}) reads {}, not {ours}",
                snapshot.pid,
                snapshot.config
            );
            inner.other_config = Some(snapshot.config.clone());
        }
        inner.snapshot_ms = Some(snapshot.t_ms);
        inner.gpu = snapshot.gpu;
        inner.gpu_error = snapshot.gpu_error;
        inner.remote = snapshot.remote;
        inner.cpu = snapshot.cpu;
        inner.cores = snapshot.cores;
        inner.load = snapshot.load;
        inner.memory = snapshot.memory;
        inner.gpu_procs = snapshot.gpu_procs;
        inner.live = snapshot.live;
        inner.progress = snapshot.progress;
        inner.activity = snapshot.activity;
        *self.storage() = snapshot.storage;
    }

    /// The log's lines written since the last look into the timeline, the
    /// last [`KEEP`] of it; all of it again once the compaction rewrote it
    /// (another file, or a shorter one)
    fn read_history(&self, inner: &mut Inner) {
        use std::os::unix::fs::MetadataExt;
        let path = self.settings.cache.join(HISTORY_FILE);
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        let (inode, mut offset) = inner.history_read;
        if meta.ino() != inode || meta.len() < offset {
            inner.samples.clear();
            inner.spans.clear();
            offset = 0;
        }
        if meta.len() > offset {
            let mut bytes = Vec::new();
            let read = File::open(&path).and_then(|mut file| {
                file.seek(SeekFrom::Start(offset))?;
                file.take(meta.len() - offset).read_to_end(&mut bytes)
            });
            if let Err(e) = read {
                log::debug!("pipeline: cannot read {}: {e}", path.display());
                return;
            }
            // A line still being written waits for the next look
            let whole = bytes
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |end| end + 1);
            offset += whole as u64;
            let oldest = unix_ms().saturating_sub(KEEP.as_millis() as u64);
            let text = String::from_utf8_lossy(&bytes[..whole]);
            for sample in text.lines().filter_map(Sample::parse_line) {
                let last = inner.samples.back().map_or(0, |s| s.t_ms);
                if sample.t_ms < oldest || sample.t_ms <= last {
                    continue;
                }
                for (id, _) in &sample.running {
                    note_running(inner.spans.entry(id.clone()).or_default(), sample.t_ms);
                }
                inner.samples.push_back(sample);
            }
        }
        inner.history_read = (meta.ino(), offset);
        let oldest = unix_ms().saturating_sub(KEEP.as_millis() as u64);
        while inner.samples.front().is_some_and(|s| s.t_ms < oldest) {
            inner.samples.pop_front();
        }
        for spans in inner.spans.values_mut() {
            spans.retain(|&(_, end)| end >= oldest);
        }
        inner.spans.retain(|_, spans| !spans.is_empty());
    }

    /// What the sampler saw of now, for the lab
    fn snapshot(&self, now: u64) -> Snapshot {
        let inner = self.inner();
        Snapshot {
            t_ms: now,
            pid: std::process::id(),
            config: self
                .settings
                .sampler
                .as_ref()
                .map(|(_, config)| config.display().to_string())
                .unwrap_or_default(),
            gpu: inner.gpu.clone(),
            gpu_error: inner.gpu_error.clone(),
            remote: inner.remote.clone(),
            cpu: inner.cpu,
            cores: inner.cores,
            load: inner.load,
            memory: inner.memory,
            gpu_procs: inner.gpu_procs.clone(),
            live: inner.live.clone(),
            progress: inner.progress.clone(),
            activity: inner.activity.clone(),
            storage: self.storage().clone(),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn storage(&self) -> std::sync::MutexGuard<'_, Storage> {
        self.storage.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The disks now: the pools' reading if one ended ([`PoolProbe`]),
    /// this host's `/`, and the VM's `C:` from its runner's fresh file; a
    /// change of the watched pool's level is logged
    fn read_storage(&self, remote: Option<&Remote>, now: u64) -> Storage {
        let host = &self.settings.storage_host;
        let answer = if host.is_empty() {
            None
        } else {
            self.probe
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .poll(host, &self.settings.storage_command)
        };
        let mut storage = self.storage();
        let before = storage.level(now);
        match answer {
            Some(Ok(pools)) => {
                storage.pools = pools;
                storage.pools_ms = Some(now);
                storage.error = None;
                storage.failed_ms = None;
            }
            Some(Err(e)) => {
                storage.error = Some(format!("{e:#}"));
                storage.failed_ms = Some(now);
            }
            None => {}
        }
        storage.root = crate::web::disk_space(Path::new("/"))
            .map(|(free, total)| Disk { free, total })
            .filter(|disk| disk.total > 0);
        storage.remote = remote
            .filter(|remote| remote.fresh)
            .and_then(|remote| remote.disk);
        let level = storage.level(now);
        if level != before
            && let Some(level) = level
        {
            let pool = storage.pools.iter().find(|pool| pool.name == WATCHED_POOL);
            let told = match pool {
                Some(pool) => format!(
                    "{} free of {} ({:.0} % used)",
                    gb(pool.free),
                    gb(pool.size),
                    pool.cap
                ),
                None => storage.error.clone().unwrap_or_default(),
            };
            let line = format!("{WATCHED_POOL} on {host}: {level}, {told}");
            if level == "ok" {
                log::info!("{line}");
            } else {
                log::warn!("{line}");
            }
        }
        storage.clone()
    }

    /// The disks for the dashboard's status, while the watched pool is not
    /// fine (low, critical or unknown); `null` otherwise
    pub fn storage_alert(&self) -> Value {
        let storage = self.storage();
        let now = unix_ms();
        match storage.level(now) {
            Some("ok") | None => Value::Null,
            Some(_) => storage.to_json(now),
        }
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

    /// Compact the log on disk ([`compact`])
    fn compact_history(path: &Path) {
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
        let storage = self.read_storage(remote.as_ref(), now);
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
        let gpu_of: HashMap<u32, f64> = apps.iter().copied().collect();
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
                found_by: found_by.to_string(),
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
            // Its main process: the most GPU memory, else the most CPU
            // since the last sample, else the newest
            let rank = |pid: &u32| {
                let proc_ = by_pid[pid];
                let ticks = inner
                    .ticks_before
                    .get(pid)
                    .map_or(0, |before| proc_.ticks.saturating_sub(*before));
                (
                    gpu_of.get(pid).copied().unwrap_or(0.0),
                    ticks,
                    proc_.start_ticks,
                )
            };
            info.main = members
                .iter()
                .max_by(|a, b| {
                    let ((gpu_a, ticks_a, start_a), (gpu_b, ticks_b, start_b)) = (rank(a), rank(b));
                    gpu_a
                        .total_cmp(&gpu_b)
                        .then(ticks_a.cmp(&ticks_b))
                        .then(start_a.cmp(&start_b))
                })
                .map(|pid| {
                    let words = app_words.get(pid).cloned().unwrap_or_else(|| cmdline(*pid));
                    short_name(&words, &by_pid[pid].comm)
                });
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
        let fresh = remote.as_ref().filter(|remote| remote.fresh);
        let remote_reading = fresh.and_then(|remote| remote.gpu.as_ref());
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
            free_gb: storage.free_gb(now),
            remote_cpu: number(fresh.and_then(|remote| remote.cpu?.percent)),
            remote_ram_mib: fresh
                .and_then(|remote| remote.memory)
                .map_or(f64::NAN, |memory| memory.used as f64 / (1 << 20) as f64),
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
        let mut activity = HashMap::new();
        for entry in &running {
            let alive = sample.running.iter().any(|(id, _)| *id == entry.id);
            // A log only tells of a live entry: an old one may be anything's
            let log = entry
                .log
                .as_deref()
                .filter(|_| entry.status() == "running" || alive)
                .map(|log| self.resolve(log))
                .and_then(|log| Some((tail(&log, LOG_TAIL_BYTES)?, modified_ms(&log))));
            if let Some(found) = self.progress_of(&mut inner, entry, alive, log.as_ref(), now) {
                progress.insert(entry.id.clone(), found);
            }
            if let Some((text, line_ms)) = &log {
                let found = Activity {
                    line_ms: *line_ms,
                    ..log_activity(text)
                };
                activity.insert(entry.id.clone(), found);
            }
        }
        let ids: HashSet<&String> = running.iter().map(|entry| &entry.id).collect();
        inner.seen.retain(|id, _| ids.contains(id));
        inner.moved.retain(|id, _| ids.contains(id));
        inner.progress = progress;
        inner.activity = activity;
        sample
    }

    /// An entry's progress now, from its run folder or its log (`log`, the
    /// last part of a live entry's and when it was written), with the rate
    /// over what was seen of it lately; `alive`: its processes run (here,
    /// or on the machine whose runner runs it)
    fn progress_of(
        &self,
        inner: &mut Inner,
        entry: &Entry,
        alive: bool,
        log: Option<&(String, Option<u64>)>,
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
                    source: String::from("metrics"),
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
        // Its end lines count only here, where its `N/M` is the progress: a
        // job's log holds each of its steps, so a run folder's run may be
        // after the end line of the step before it
        if found.is_none()
            && let Some((text, updated_ms)) = log
            && let Some((step, total)) = log_progress(text)
        {
            // It moved when the sampler first saw this step
            let moved = inner.moved.entry(entry.id.clone()).or_insert((step, now));
            if moved.0 != step {
                *moved = (step, now);
            }
            found = Some(Progress {
                step,
                total: Some(total),
                source: String::from("log"),
                ended: log_done(text),
                updated_ms: *updated_ms,
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
        self.read_snapshot(&mut inner);
        self.read_history(&mut inner);
        // A page asking right after a write sees it
        self.reload_queue(&mut inner);
        // A stale snapshot says nothing of now: no sampler runs
        let now = unix_ms();
        let fresh = inner
            .snapshot_ms
            .is_some_and(|t| now.saturating_sub(t) <= SNAPSHOT_STALE.as_millis() as u64);
        let log = self.settings.cache.join(SAMPLER_LOG);
        let gpu_error = match inner.snapshot_ms {
            _ if fresh => inner.gpu_error.clone(),
            Some(t) => Some(format!(
                "the sampler has written nothing for {} s (its log: {})",
                now.saturating_sub(t) / 1000,
                log.display()
            )),
            None => Some(format!(
                "no sampler has written yet (its log: {})",
                log.display()
            )),
        };
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
        let remote = inner.remote.as_ref().filter(|_| fresh);
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
                    "live": inner.live.get(&entry.id).filter(|_| fresh),
                    "progress": inner.progress.get(&entry.id),
                    "activity": inner.activity.get(&entry.id).filter(|_| fresh),
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
            "now_ms": now,
            "sample_ms": SAMPLE_EVERY.as_millis() as u64,
            "stall_ms": STALL.as_millis() as u64,
            // When the sampler last wrote what it saw
            "sampled_ms": inner.snapshot_ms,
            "gpu": inner.gpu.as_ref().filter(|_| fresh),
            "gpu_error": gpu_error,
            "gpu_procs": if fresh { &inner.gpu_procs[..] } else { &[] },
            "remote": remote,
            "cpu": {
                "percent": inner.cpu.filter(|_| fresh),
                "cores": inner.cores,
                "load": inner.load.filter(|_| fresh),
            },
            "memory": inner.memory.filter(|_| fresh).map(|[total, available, swap_total, swap_free]| json!({
                "total": total,
                "available": available,
                "swap_total": swap_total,
                "swap_free": swap_free,
            })),
            "storage": self.storage().to_json(now),
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
        let mut inner = self.inner();
        self.read_history(&mut inner);
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

/// The sampler, `grizzco-lab sample` (see the module docs): the machine
/// every [`SAMPLE_EVERY`] into the log ([`HISTORY_FILE`], compacted at
/// start and every [`COMPACT_EVERY`]) and [`SNAPSHOT_FILE`], until it is
/// stopped or its binary is built again (the lab then starts the new
/// build); an error when another sampler runs
pub fn run_sampler(settings: Settings) -> Result<()> {
    let cache = settings.cache.clone();
    std::fs::create_dir_all(&cache).with_context(|| format!("cannot make {}", cache.display()))?;
    let _lock = hold_lock(&cache.join(LOCK_FILE))?;
    let built = |exe: &Path| std::fs::metadata(exe).and_then(|meta| meta.modified()).ok();
    let exe = std::env::current_exe().ok();
    let exe_built = exe.as_deref().and_then(built);
    let history = cache.join(HISTORY_FILE);
    log::info!(
        "Sampling the machine every {} s into {} (pid {})",
        SAMPLE_EVERY.as_secs(),
        history.display(),
        std::process::id()
    );
    let pipeline = Pipeline::new(settings);
    let mut compacted: Option<Instant> = None;
    loop {
        let started = Instant::now();
        if compacted.is_none_or(|at| at.elapsed() >= COMPACT_EVERY) {
            Pipeline::compact_history(&history);
            compacted = Some(Instant::now());
        }
        let sample = pipeline.sample();
        if let Err(e) = append_history(&history, &sample) {
            log::warn!("pipeline: cannot append to {}: {e:#}", history.display());
        }
        let snapshot = serde_json::to_vec(&pipeline.snapshot(sample.t_ms))?;
        if let Err(e) = write_atomic(&cache.join(SNAPSHOT_FILE), &snapshot) {
            log::warn!("pipeline: cannot write the snapshot: {e:#}");
        }
        if let Some(exe) = &exe
            && built(exe) != exe_built
        {
            log::info!(
                "{} was built again: this sampler stops, for the lab to start the new build",
                exe.display()
            );
            return Ok(());
        }
        std::thread::sleep(SAMPLE_EVERY.saturating_sub(started.elapsed()));
    }
}

/// Take the sampler's lock at `path` for this process's life (the file
/// returned holds it), its pid in it; an error naming the pid in it when
/// another sampler holds it
fn hold_lock(path: &Path) -> Result<File> {
    use std::io::Write;
    let mut file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            let pid = std::fs::read_to_string(path).unwrap_or_default();
            bail!("a sampler runs already (pid {})", pid.trim());
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return Err(anyhow::Error::from(e).context("cannot lock the sampler's lock"));
        }
    }
    file.set_len(0)?;
    writeln!(file, "{}", std::process::id())?;
    Ok(file)
}

/// Whether a sampler holds the lock at `path`: taking it for a moment tells
fn sampler_runs(path: &Path) -> bool {
    let opened = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path);
    opened.is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// Start the sampler `exe` with `config`, detached in a session of its own
/// (neither the terminal's Ctrl-C nor its hangup reaches it), its output
/// appended to [`SAMPLER_LOG`] in `cache`
fn spawn_sampler(exe: &Path, config: &Path, cache: &Path) -> Result<Child> {
    std::fs::create_dir_all(cache).with_context(|| format!("cannot make {}", cache.display()))?;
    let log = File::options()
        .create(true)
        .append(true)
        .open(cache.join(SAMPLER_LOG))?;
    let mut command = Command::new(exe);
    command
        .arg("sample")
        .arg("--config")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid(2) is async-signal-safe and touches no memory
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
        .spawn()
        .with_context(|| format!("cannot run {}", exe.display()))
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
                let what = crate::exit::request(
                    "GET",
                    &format!("/api/pipeline/{}", tail.as_str()),
                    &query,
                );
                blocking(what, move || pipeline.get(tail.as_str(), &query))
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
                let what = format!("POST /api/pipeline/{}", tail.as_str());
                blocking(what, move || pipeline.post(tail.as_str(), &body))
            },
        );
    get.or(post).unify().boxed()
}

/// Run `answer` on a blocking thread, named `what` for the exit, and turn
/// it into a JSON response
async fn blocking(
    what: String,
    answer: impl FnOnce() -> Result<Value, Status> + Send + 'static,
) -> Result<Response<Vec<u8>>, core::convert::Infallible> {
    let (status, value) = match crate::exit::blocking(what, answer).await {
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
        let vm = |mut sample: Sample, cpu: f64, ram_mib: f64| {
            sample.remote_cpu = cpu;
            sample.remote_ram_mib = ram_mib;
            sample
        };
        let merged = merge(&[
            vm(sample(0, &[("b", 1.0)]), 10.0, 900.0),
            vm(sample(5000, &[("a", f64::NAN), ("b", 3.0)]), 30.0, 700.0),
            sample(10000, &[]),
        ]);
        assert_eq!(merged.t_ms, 10000);
        assert_eq!(merged.cpu, 20.0);
        // The VM's CPU as a mean, its memory as the most, where read
        assert_eq!((merged.remote_cpu, merged.remote_ram_mib), (20.0, 900.0));
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
            free_gb: vec![
                (String::from("pve:rpool"), 839.314),
                (String::from("linux:/"), 841.9),
            ],
            remote_cpu: 31.04,
            remote_ram_mib: 13638.2,
        };
        let line = new.line();
        let values: Vec<Value> = serde_json::from_str(&line).unwrap();
        assert_eq!(values[1], json!(90));
        assert_eq!(values[8], json!(["g-chunk-train", "g-present-dropout"]));
        assert_eq!(values[14], json!({"g-present-dropout": 1.61}));
        assert_eq!(values[15], json!({"pve:rpool": 839.3, "linux:/": 841.9}));
        assert_eq!(values[LINE_REMOTE_CPU], json!(31.0));
        assert_eq!(values[LINE_REMOTE_RAM], json!(13638));
        let back = Sample::parse_line(&line).unwrap();
        assert_eq!(back.remote.util, 37.0);
        assert_eq!(back.remote.power_w, 105.0);
        assert!(back.remote.jobs_mib.is_nan());
        assert_eq!(back.load, 6.68);
        assert!(back.running[0].1.is_nan());
        assert_eq!(back.running[1].1, 1.61);
        assert!(back.free_gb.contains(&(String::from("pve:rpool"), 839.3)));
        assert_eq!((back.remote_cpu, back.remote_ram_mib), (31.0, 13638.0));
        // The version before this one wrote no free space: none read, and
        // no VM's CPU or memory
        let before: Vec<Value> = values[..15].to_vec();
        let back = Sample::parse_line(&Value::Array(before).to_string()).unwrap();
        assert!(back.free_gb.is_empty());
        assert_eq!(back.running[1].1, 1.61);
        assert!(back.remote_cpu.is_nan() && back.remote_ram_mib.is_nan());
        // The page's row: the numbers, then the CPU of each entry, the free
        // space of each disk, and the VM's CPU and memory
        let row = new.row();
        assert_eq!(row[8], json!(37));
        assert_eq!(row[12], json!(6.68));
        assert_eq!(row[13], json!({"g-present-dropout": 1.61}));
        assert_eq!(row[14], json!({"pve:rpool": 839.3, "linux:/": 841.9}));
        assert_eq!((&row[15], &row[16]), (&json!(31.0), &json!(13638)));
    }

    #[test]
    fn pools_read_and_the_watched_one_tells_its_level() {
        let text = "backup\t11991548690432\t8050604924928\t3940943765504\t67\t4\n\
                    rpool\t3882650435584\t3043335847936\t839314587648\t78\t48\n\
                    tank\t100\t50\t50\t50\t-\n\
                    not a pool line\n";
        let pools = parse_pools(text);
        assert_eq!(pools.len(), 3);
        let rpool = &pools[1];
        assert_eq!(rpool.name, "rpool");
        assert_eq!(rpool.free, 839_314_587_648);
        assert_eq!((rpool.cap, rpool.frag), (78.0, Some(48.0)));
        assert_eq!(pools[2].frag, None);
        assert_eq!(pool_level(rpool), "ok");
        let pool = |free: u64, cap: f64| Pool {
            free,
            cap,
            ..rpool.clone()
        };
        // Low under 300 GB free or at 85 % used; critical under 150 GB
        assert_eq!(pool_level(&pool(299 * GB, 70.0)), "low");
        assert_eq!(pool_level(&pool(400 * GB, 85.0)), "low");
        assert_eq!(pool_level(&pool(149 * GB, 97.0)), "critical");
        assert_eq!(pool_level(&pool(300 * GB, 84.0)), "ok");

        let now = 10 * 60_000;
        let mut storage = Storage {
            host: String::from("pve"),
            ..Storage::default()
        };
        // Nothing asked yet
        assert_eq!(storage.level(now), None);
        storage.pools = pools.clone();
        storage.pools_ms = Some(now - 60_000);
        storage.root = Some(Disk {
            free: 841_938_042_880,
            total: 1_931_659_444_224,
        });
        assert_eq!(storage.level(now), Some("ok"));
        let free = storage.free_gb(now);
        assert!(free.contains(&(String::from("pve:rpool"), 839.314587648)));
        assert!(free.contains(&(String::from("linux:/"), 841.93804288)));
        // A reading that failed while the last one is fresh changes nothing
        storage.failed_ms = Some(now);
        assert_eq!(storage.level(now), Some("ok"));
        // Once the last reading is stale: unknown, and its pools not kept
        let later = now + STORAGE_STALE.as_millis() as u64;
        assert_eq!(storage.level(later), Some("unknown"));
        assert_eq!(
            storage.free_gb(later),
            [(String::from("linux:/"), 841.93804288)]
        );
        let json = storage.to_json(later);
        assert_eq!(json["level"], "unknown");
        assert_eq!(json["pools"], json!([]));
        // No storage host: no level
        storage.host.clear();
        assert_eq!(storage.level(now), None);
    }

    #[test]
    fn merged_samples_keep_each_disks_least_free_space() {
        let sample = |t_ms: u64, rpool: f64| Sample {
            t_ms,
            free_gb: vec![
                (String::from("pve:rpool"), rpool),
                (String::from("linux:/"), 800.0),
            ],
            ..Sample::default()
        };
        let merged = merge(&[sample(1, 310.0), sample(2, 290.5), sample(3, 300.0)]);
        assert_eq!(
            merged.free_gb,
            [
                (String::from("pve:rpool"), 290.5),
                (String::from("linux:/"), 800.0)
            ]
        );
    }

    /// A scratch folder for the sampler's files
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pipeline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_lab_follows_the_samplers_log_and_snapshot() {
        let dir = scratch("follow");
        let pipeline = Pipeline::new(Settings {
            queue: dir.join("queue.json"),
            cache: dir.clone(),
            remote: dir.join("win11/gpu.json"),
            storage_host: String::from("pve"),
            storage_command: String::new(),
            sampler: None,
        });
        let now = unix_ms();
        let sample = |t_ms: u64, ids: &[&str]| Sample {
            t_ms,
            running: ids.iter().map(|id| (id.to_string(), 1.0)).collect(),
            ..Sample::default()
        };
        let times = |pipeline: &Pipeline| -> Vec<u64> {
            pipeline.inner().samples.iter().map(|s| s.t_ms).collect()
        };
        let path = dir.join(HISTORY_FILE);
        // Older than KEEP left out; a line still being written waits
        let old = sample(now - KEEP.as_millis() as u64 - 1000, &[]).line();
        let last = sample(now - 5_000, &[]).line();
        let (begun, rest) = last.split_at(20);
        let first = sample(now - 10_000, &["a"]).line();
        std::fs::write(&path, format!("{old}\n{first}\n{begun}")).unwrap();
        pipeline.refresh();
        assert_eq!(times(&pipeline), [now - 10_000]);
        // The line finished, and one more: each read once
        let more = sample(now - 1_000, &["a"]).line();
        let mut file = File::options().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, format!("{rest}\n{more}\n").as_bytes()).unwrap();
        pipeline.refresh();
        pipeline.refresh();
        assert_eq!(times(&pipeline), [now - 10_000, now - 5_000, now - 1_000]);
        assert_eq!(pipeline.inner().spans["a"].len(), 1);
        // Rewritten by the compaction (another file): read from the start
        let compacted = format!("{}\n", sample(now - 2_000, &["b"]).line());
        write_atomic(&path, compacted.as_bytes()).unwrap();
        pipeline.refresh();
        assert_eq!(times(&pipeline), [now - 2_000]);
        assert!(pipeline.inner().spans.contains_key("b"));
        assert!(!pipeline.inner().spans.contains_key("a"));

        // The snapshot: now's readings, each entry's processes, the disks
        let mut snapshot = Snapshot {
            t_ms: now,
            pid: 7,
            gpu: Some(Gpu {
                name: String::from("NVIDIA GeForce RTX 4070 SUPER"),
                util: Some(50.0),
                ..Gpu::default()
            }),
            cores: 16,
            storage: Storage {
                host: String::from("pve"),
                pools: parse_pools("rpool\t3882650435584\t3700000000000\t120000000000\t97\t60"),
                pools_ms: Some(now),
                ..Storage::default()
            },
            ..Snapshot::default()
        };
        snapshot.live.insert(
            String::from("a"),
            Live {
                pids: vec![42],
                found_by: String::from("pgid"),
                ..Live::default()
            },
        );
        let write = |snapshot: &Snapshot| {
            let bytes = serde_json::to_vec(snapshot).unwrap();
            write_atomic(&dir.join(SNAPSHOT_FILE), &bytes).unwrap();
        };
        write(&snapshot);
        let state = pipeline.state(None);
        assert_eq!(state["gpu"]["util"], 50.0);
        assert_eq!(state["gpu_error"], Value::Null);
        assert_eq!(state["sampled_ms"], now);
        assert_eq!(state["cpu"]["cores"], 16);
        assert_eq!(state["storage"]["level"], "critical");
        assert_eq!(pipeline.storage_alert()["level"], "critical");
        assert_eq!(pipeline.inner().live["a"].found_by, "pgid");
        // A stale one says nothing of now
        snapshot.t_ms = now - 60_000;
        write(&snapshot);
        pipeline.inner().snapshot_seen = None;
        let state = pipeline.state(None);
        assert_eq!(state["gpu"], Value::Null);
        assert!(state["gpu_error"].as_str().unwrap().contains("the sampler"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_sampler_at_a_time() {
        let dir = scratch("lock");
        let path = dir.join(LOCK_FILE);
        assert!(!sampler_runs(&path));
        let held = hold_lock(&path).unwrap();
        assert!(sampler_runs(&path));
        let refused = format!("{:#}", hold_lock(&path).unwrap_err());
        assert!(
            refused.contains(&std::process::id().to_string()),
            "{refused}"
        );
        drop(held);
        assert!(!sampler_runs(&path));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn remote_files_give_the_vms_disk_and_why_its_runner_holds() {
        let text = r#"{"host": "win11", "at": "2026-09-30T11:55:23-07:00", "ok": true,
            "gpu": {"name": "NVIDIA GeForce RTX 4080 SUPER", "utilization_percent": 70.0},
            "disk": {"free": 120000000000, "total": 999000000000},
            "runner": {"pid": 1, "hold": true, "space": "C: under 50 GB"}}"#;
        let now = parse_time("2026-09-30T11:55:30-07:00").unwrap() as u64;
        let remote = parse_remote("win11", text, now, |_| true);
        assert_eq!(
            remote.disk,
            Some(Disk {
                free: 120_000_000_000,
                total: 999_000_000_000
            })
        );
        assert!(remote.hold);
        assert_eq!(remote.space.as_deref(), Some("C: under 50 GB"));
        // An older runner's file has neither
        let older = r#"{"at": "2026-09-30T11:55:23-07:00", "ok": true, "runner": {"pid": 1}}"#;
        let remote = parse_remote("win11", older, now, |_| true);
        assert_eq!((remote.disk, remote.space), (None, None));
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
        // A runner of before tells the GPU alone
        assert_eq!(
            (gpu.power_limit_w, gpu.fan, gpu.pstate.as_str()),
            (None, None, "")
        );
        assert!(remote.cpu.is_none() && remote.memory.is_none());
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
    fn remote_files_of_now_tell_the_machine_as_this_host_is_told() {
        let text = r#"{
  "host": "win11",
  "at": "2026-09-30T12:31:54-07:00",
  "ok": true,
  "gpu": {
    "name": "NVIDIA GeForce RTX 4080 SUPER",
    "utilization_percent": 90.0,
    "memory_used_mib": 4916.0,
    "memory_total_mib": 16376.0,
    "temperature_c": 89.0,
    "power_w": 218.56,
    "power_limit_w": 320.0,
    "fan_percent": 100.0,
    "sm_clock_mhz": 2490.0,
    "pstate": "P2"
  },
  "processes": [{"pid": 4732, "name": "python.exe", "memory_mib": null}],
  "disk": {"free": 360326111232, "total": 535938723840},
  "cpu": {"percent": 31.0, "cores": 16},
  "memory": {"used": 14300585984, "total": 34320936960},
  "job": null,
  "runner": {"pid": 53208, "hold": true}
}"#;
        let at = parse_time("2026-09-30T12:31:54-07:00").unwrap() as u64;
        let remote = parse_remote("win11", text, at + 5_000, |_| true);
        let gpu = remote.gpu.as_ref().unwrap();
        assert_eq!(gpu.power_limit_w, Some(320.0));
        assert_eq!((gpu.fan, gpu.sm_mhz), (Some(100.0), Some(2490.0)));
        assert_eq!(gpu.pstate, "P2");
        assert_eq!(
            remote.cpu,
            Some(HostCpu {
                percent: Some(31.0),
                cores: Some(16)
            })
        );
        assert_eq!(
            remote.memory,
            Some(HostMemory {
                used: 14300585984,
                total: 34320936960
            })
        );
        assert!(remote.hold && remote.running().is_none());
        // Unreachable, it tells nothing of the machine
        let down = text.replace("\"ok\": true", "\"ok\": false");
        let remote = parse_remote("win11", &down, at, |_| true);
        assert!(remote.gpu.is_none() && remote.cpu.is_none() && remote.memory.is_none());
    }

    #[test]
    fn logs_tell_the_step_a_job_runs() {
        // An agent's job script: a training step, then a scoring step
        let log = "== start pdk-azu (step 1 of the job) 2026-09-30 01:22:15\n\
                   [pdk-azu, part 1 of the job] step   500/12000  loss 3.637\n\
                   == end pdk-azu (exit 0, group peak RSS 5462 MB) 2026-09-30 01:33:03\n\
                   == start decode-pdk (step 2 of the job) 2026-09-30 01:33:03\n";
        let activity = log_activity(log);
        assert_eq!(activity.step.as_deref(), Some("decode-pdk"));
        assert_eq!(
            activity.step_ms,
            parse_time("2026-09-30 01:33:03").map(|t| t as u64)
        );
        // The counter in the log is the step before's
        assert!(!activity.counted);
        assert_eq!(
            activity.line.as_deref(),
            Some("== start decode-pdk (step 2 of the job) 2026-09-30 01:33:03")
        );
        // The step counts
        let counting = log_activity(&format!("{log}step 250/2000 loss 3.1\n"));
        assert!(counting.counted);
        assert_eq!(counting.line.as_deref(), Some("step 250/2000 loss 3.1"));
        // Between steps; a start line without its time
        let between = log_activity(
            "== start a (step 1 of the job)\nok\n== end a (exit 0) 2026-09-30 01:00:00\n",
        );
        assert_eq!((between.step, between.step_ms), (None, None));
        let untimed = log_activity("== start a (step 1 of the job)\n");
        assert_eq!(
            (untimed.step.as_deref(), untimed.step_ms),
            (Some("a"), None)
        );
        // The win11 VM's job starts no step of its own
        let vm = log_activity(
            "== start Wed 09/30/2026 13:01:46.27 in C:\\Users\\cjr\\AgentZero\n\
             step  3750/12000  loss 3.296\n",
        );
        assert_eq!((vm.step, vm.counted), (None, false));
        // A log without steps: its last line as a progress bar leaves it
        let plain = log_activity("loading\r50%\r100%\n\n");
        assert_eq!((plain.step, plain.line.as_deref()), (None, Some("100%")));
        let long = log_activity(&"x".repeat(LINE_CHARS + 50));
        assert_eq!(long.line.unwrap().chars().count(), LINE_CHARS + 1);
        assert_eq!(log_activity(""), Activity::default());
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
