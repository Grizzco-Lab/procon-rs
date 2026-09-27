//! A slow, read-only archive of Discord channels one is a member of, made
//! with one's own account token (`cuttlefish fetch discord`).
//!
//! Discord's terms forbid automating a user account, and an account seen
//! doing it can be banned; the user was told and accepted that risk for one
//! private archive of #vod-review. So this module does as little as a
//! person reading the channel would, and nothing else:
//!
//! - **Scope lock**: only the given channels and the threads in them; every
//!   request is checked against that list ([`in_scope`]). Read-only GETs:
//!   the channel object, messages 100 at a time with `before`/`after`, the
//!   thread listings (active, archived public). No gateway, no typing, no
//!   writes.
//! - **Pace**: a minimum delay between requests (4 s by default) with
//!   random jitter, 429 `retry_after` and the `X-RateLimit-*` headers
//!   obeyed, exponential backoff on server errors, a stop on 401 and 403,
//!   and an optional cap on requests per day. A desktop browser's
//!   User-Agent.
//! - **Resumable**: the API's JSON is kept as received in the output folder
//!   (`<id>.channel.json`, `<id>.messages.jsonl` appended, threads in
//!   `threads/`) with the cursors in `state.json`, so a run continues: new
//!   messages since the last run first, then older history until the
//!   start. Ctrl+C finishes the request under way and saves.
//!
//! The token comes from `DISCORD_USER_TOKEN` in the environment (the env
//! file, [`crate::env_file`]) and is never logged or written. The inbox
//! reads the output ([`crate::inbox`], [`crate::discord::read_archive`]).

use crate::crawl::encode;
use crate::discord::{API, FORUM_TYPES, snowflake};
use crate::store::write_atomic;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Environment variable holding the account's token
pub const TOKEN_VAR: &str = "DISCORD_USER_TOKEN";
/// Written into `state.json`, so the inbox knows the file is ours
pub const TOOL: &str = "cuttlefish fetch discord";
/// A desktop browser
pub const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0";
/// Messages per page, the API's maximum
pub const PAGE: usize = 100;
/// Longest wait taken from a rate-limit answer or backoff
const MAX_WAIT: Duration = Duration::from_secs(600);
/// First backoff after a server error; doubles each time
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
/// Attempts at one request before giving up
const ATTEMPTS: u32 = 8;

/// An HTTP answer
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub status: u16,
    /// Header names and values
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    /// A header's value (names compared without case)
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn number(&self, name: &str) -> Option<f64> {
        self.header(name)?.trim().parse().ok()
    }
}

/// GET requests; tests use a fake
pub trait Http {
    /// GETs `url` with the token as `Authorization`
    fn get(&mut self, url: &str, token: &str) -> Result<Response>;
}

/// HTTPS through ureq, as a desktop browser
pub struct Https {
    agent: ureq::Agent,
}

impl Default for Https {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Https { agent }
    }
}

impl Http for Https {
    fn get(&mut self, url: &str, token: &str) -> Result<Response> {
        let mut resp = self
            .agent
            .get(url)
            .header("Authorization", token)
            .header("Accept", "application/json")
            .call()
            .with_context(|| alloc::format!("GET {url}"))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    String::from(k.as_str()),
                    String::from(v.to_str().unwrap_or_default()),
                )
            })
            .collect();
        let body = resp.body_mut().read_to_string().unwrap_or_default();
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// Waiting and the date; tests use a fake that does not sleep
pub trait Clock {
    /// Waits `d`; false when told to stop before it passed
    fn sleep(&mut self, d: Duration) -> bool;
    /// Whether to stop before the next request
    fn stopped(&self) -> bool;
    /// Time since the run started, for the session limit
    fn elapsed(&self) -> Duration;
    /// Today's date (UTC), for the daily cap
    fn today(&self) -> String {
        Utc::now().format("%Y-%m-%d").to_string()
    }
}

/// Real time, interrupted by a flag (set by Ctrl+C)
pub struct Interruptible {
    pub stop: Arc<AtomicBool>,
    pub started: Instant,
}

impl Clock for Interruptible {
    fn sleep(&mut self, d: Duration) -> bool {
        let end = Instant::now() + d;
        while !self.stopped() {
            let now = Instant::now();
            if now >= end {
                return true;
            }
            std::thread::sleep((end - now).min(Duration::from_millis(200)));
        }
        false
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// A range to draw from, written `3-8` (or one number, `4`)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
}

impl Range {
    pub const fn new(min: f64, max: f64) -> Self {
        Range { min, max }
    }
}

impl core::str::FromStr for Range {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, String> {
        let bad = || alloc::format!("{s}: expected a number or a range such as 3-8");
        let number = |t: &str| t.trim().parse::<f64>().ok().filter(|n| *n >= 0.0);
        let (min, max) = match s.split_once('-') {
            Some((a, b)) => (number(a).ok_or_else(bad)?, number(b).ok_or_else(bad)?),
            None => {
                let n = number(s).ok_or_else(bad)?;
                (n, n)
            }
        };
        if min > max {
            return Err(bad());
        }
        Ok(Range { min, max })
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.min == self.max {
            write!(f, "{}", self.min)
        } else {
            write!(f, "{}-{}", self.min, self.max)
        }
    }
}

/// How fast to go: like a person reading, not a script
#[derive(Clone, Debug, PartialEq)]
pub struct Pace {
    /// Seconds between requests, drawn anew for each
    pub delay: Range,
    /// Requests between longer pauses, drawn anew after each pause
    pub pause_every: Range,
    /// Seconds of a longer pause
    pub pause: Range,
    /// At most this many requests per day (UTC)
    pub daily_cap: Option<u32>,
    /// At most this many requests this run (a check run: 2 reads one page)
    pub max_requests: Option<u32>,
    /// At most this long a run, in minutes; the next run continues
    pub max_minutes: Option<f64>,
}

impl Default for Pace {
    fn default() -> Self {
        Pace {
            delay: Range::new(3.0, 8.0),
            pause_every: Range::new(40.0, 120.0),
            pause: Range::new(60.0, 300.0),
            daily_cap: None,
            max_requests: None,
            max_minutes: None,
        }
    }
}

/// What to fetch
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Channel ids; nothing outside them and their threads is read
    pub channels: Vec<String>,
    /// Server id, for the active-threads listing; else the channels' own
    pub guild: Option<String>,
    /// Also the channels' threads and forum posts
    pub threads: bool,
    /// Every channel's files in the root itself (`--out`) rather than in
    /// `<guild>/<channel>/` below it
    pub flat: bool,
    pub pace: Pace,
}

/// Where a conversation's archive stands
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cursor {
    /// Channel or thread name
    pub name: String,
    /// Parent channel of a thread
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Newest message id kept; new messages are fetched after it
    pub newest: Option<String>,
    /// Oldest message id kept; history is fetched before it
    pub oldest: Option<String>,
    /// When the oldest message was posted
    pub oldest_at: Option<DateTime<Utc>>,
    /// History reached the first message
    pub complete: bool,
    /// Messages kept
    pub messages: u64,
}

/// `state.json`: the cursors and the day's request count
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// [`TOOL`]
    pub tool: String,
    /// Day (UTC) the count is for
    pub day: String,
    pub requests_today: u32,
    /// By channel or thread id
    pub conversations: BTreeMap<String, Cursor>,
}

impl State {
    fn path(out: &Path) -> PathBuf {
        out.join("state.json")
    }

    /// The state in `out`, or a fresh one
    pub fn load(out: &Path) -> Result<Self> {
        let path = Self::path(out);
        if !path.exists() {
            return Ok(State {
                tool: String::from(TOOL),
                ..State::default()
            });
        }
        serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| alloc::format!("reading {}", path.display()))
    }

    fn save(&self, out: &Path) -> Result<()> {
        write_atomic(&Self::path(out), &serde_json::to_vec_pretty(self)?)
    }
}

/// Why a run ended before the end
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// Ctrl+C
    Interrupted,
    /// The day's request cap
    DailyCap(u32),
    /// This run's request limit
    MaxRequests(u32),
    /// This run's time limit, in minutes
    MaxMinutes(u32),
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::Interrupted => write!(f, "interrupted"),
            Stop::DailyCap(n) => write!(f, "{n} requests today: the daily cap; run again tomorrow"),
            Stop::MaxRequests(n) => {
                write!(f, "{n} requests: this run's limit; the next run continues")
            }
            Stop::MaxMinutes(n) => {
                write!(f, "{n} minutes: this run's limit; the next run continues")
            }
        }
    }
}

impl std::error::Error for Stop {}

/// What a run did
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    /// Requests made
    pub requests: u32,
    /// Messages added to the archive
    pub messages: u64,
    /// Channels and threads in the archive
    pub conversations: usize,
    /// Set when the run ended before the end
    pub stopped: Option<Stop>,
}

/// Whether a request path is within the archive's scope: a channel or
/// thread in `allowed`, or the active-threads listing of `guild`
pub fn in_scope(path: &str, allowed: &BTreeSet<String>, guild: Option<&str>) -> bool {
    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split(['/', '?'])
        .filter(|p| !p.is_empty())
        .collect();
    match parts.as_slice() {
        ["channels", id, ..] => allowed.contains(*id),
        ["guilds", g, "threads", "active"] => guild == Some(*g),
        _ => false,
    }
}

/// Writes a channel object as received, unless the file already says the
/// same (the folder may be synced; no churn for unchanged threads)
fn keep(path: &Path, object: &Value) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(object)?;
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    write_atomic(path, &bytes)
}

/// The run: the HTTP client, the clock, the state and the scope
struct Fetcher<'a> {
    http: &'a mut dyn Http,
    clock: &'a mut dyn Clock,
    token: String,
    /// The archive root: `state.json`, and the channels' folders
    root: PathBuf,
    options: Options,
    state: State,
    /// Channels and threads that may be read
    allowed: BTreeSet<String>,
    guild: Option<String>,
    /// Requests this run
    requests: u32,
    /// Requests since the last longer pause, and how many until the next
    since_pause: u32,
    until_pause: u32,
    /// Messages added this run
    added: u64,
    /// Wait asked by the last answer (rate-limit headers, 429)
    rate_wait: Duration,
    /// Wait after server errors
    backoff: Duration,
    /// The last wait before a request, for progress lines
    delay: Duration,
    rng: u64,
    report: &'a mut dyn FnMut(&str),
}

/// Archives `options.channels` (and their threads) under `root`: each in
/// `<guild>/<channel>/` (or in `root` itself with `options.flat`), the
/// cursors in `root/state.json`. Progress goes to `report`; `token` is the
/// account's. Returns what was done, with `stopped` set when Ctrl+C, the
/// daily cap or the run's limits ended it early. A 401 or 403 is an error.
pub fn run(
    http: &mut dyn Http,
    clock: &mut dyn Clock,
    token: String,
    root: &Path,
    options: Options,
    report: &mut dyn FnMut(&str),
) -> Result<Summary> {
    std::fs::create_dir_all(root)?;
    let state = State::load(root)?;
    let allowed: BTreeSet<String> = options.channels.iter().cloned().collect();
    // Threads of earlier runs stay in scope
    let mut allowed_now = allowed.clone();
    for (id, c) in &state.conversations {
        if c.parent.as_ref().is_some_and(|p| allowed.contains(p)) {
            allowed_now.insert(id.clone());
        }
    }
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
        ^ u64::from(std::process::id()).rotate_left(32);
    let mut f = Fetcher {
        http,
        clock,
        token,
        root: root.to_path_buf(),
        guild: options.guild.clone(),
        options,
        state,
        allowed: allowed_now,
        requests: 0,
        since_pause: 0,
        until_pause: 0,
        added: 0,
        rate_wait: Duration::ZERO,
        backoff: Duration::ZERO,
        delay: Duration::ZERO,
        rng: seed.max(1),
        report,
    };
    f.until_pause = f.draw(f.options.pace.pause_every).round() as u32;
    let result = f.fetch_all();
    f.state.save(root)?;
    let stopped = match result {
        Ok(()) => None,
        Err(e) => match e.downcast_ref::<Stop>() {
            Some(stop) => Some(*stop),
            None => return Err(e),
        },
    };
    Ok(Summary {
        requests: f.requests,
        messages: f.added,
        conversations: f.state.conversations.len(),
        stopped,
    })
}

impl Fetcher<'_> {
    /// A number drawn evenly from the range
    fn draw(&mut self, range: Range) -> f64 {
        // xorshift64
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        let unit = (self.rng >> 11) as f64 / (1u64 << 53) as f64;
        range.min + (range.max - range.min) * unit
    }

    /// The wait before the next request: the longer pause when it is due
    /// (reported), else a delay drawn from the range
    fn paced(&mut self) -> Duration {
        if self.since_pause >= self.until_pause {
            let pause = self.draw(self.options.pace.pause);
            self.since_pause = 0;
            self.until_pause = self.draw(self.options.pace.pause_every).round().max(1.0) as u32;
            (self.report)(&alloc::format!(
                "pausing {:.1} min after {} requests; the next pause in {} requests",
                pause / 60.0,
                self.requests,
                self.until_pause
            ));
            return Duration::from_secs_f64(pause);
        }
        Duration::from_secs_f64(self.draw(self.options.pace.delay))
    }

    /// GET an API path as JSON, within scope, paced, with rate limits and
    /// errors handled. With `optional`, 403 and 404 give `None` instead of
    /// an error (a listing the account may not have).
    fn request(&mut self, path: &str, optional: bool) -> Result<Option<Value>> {
        if !in_scope(path, &self.allowed, self.guild.as_deref()) {
            bail!("{path} is out of scope: only the given channels and their threads are read");
        }
        let mut attempts = 0;
        loop {
            if self.clock.stopped() {
                return Err(Stop::Interrupted.into());
            }
            let today = self.clock.today();
            if self.state.day != today {
                self.state.day = today;
                self.state.requests_today = 0;
            }
            if let Some(cap) = self.options.pace.daily_cap
                && self.state.requests_today >= cap
            {
                return Err(Stop::DailyCap(cap).into());
            }
            if let Some(max) = self.options.pace.max_requests
                && self.requests >= max
            {
                return Err(Stop::MaxRequests(max).into());
            }
            if let Some(minutes) = self.options.pace.max_minutes
                && self.clock.elapsed().as_secs_f64() >= minutes * 60.0
            {
                return Err(Stop::MaxMinutes(minutes.round() as u32).into());
            }
            let mut wait = self.rate_wait.max(self.backoff);
            if self.requests > 0 {
                wait = wait.max(self.paced());
            }
            self.delay = wait;
            if !wait.is_zero() && !self.clock.sleep(wait) {
                return Err(Stop::Interrupted.into());
            }
            self.rate_wait = Duration::ZERO;
            self.requests += 1;
            self.since_pause += 1;
            self.state.requests_today += 1;
            attempts += 1;
            let url = alloc::format!("{API}{path}");
            let resp = match self.http.get(&url, &self.token) {
                Ok(r) => r,
                Err(e) if attempts < ATTEMPTS => {
                    self.backoff = (self.backoff * 2).clamp(FIRST_BACKOFF, MAX_WAIT);
                    (self.report)(&alloc::format!(
                        "{e:#}; trying again in {:.0} s",
                        self.backoff.as_secs_f64()
                    ));
                    continue;
                }
                Err(e) => return Err(e.context("giving up")),
            };
            if resp.number("x-ratelimit-remaining") == Some(0.0) {
                let reset = resp.number("x-ratelimit-reset-after").unwrap_or(1.0);
                self.rate_wait = Duration::from_secs_f64(reset.clamp(0.0, MAX_WAIT.as_secs_f64()));
            }
            let body: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
            match resp.status {
                200..=299 => {
                    self.backoff = Duration::ZERO;
                    if body.is_null() {
                        bail!("{path}: the answer is not JSON");
                    }
                    return Ok(Some(body));
                }
                429 => {
                    let retry = body["retry_after"]
                        .as_f64()
                        .or_else(|| resp.number("retry-after"))
                        .unwrap_or(5.0);
                    self.rate_wait =
                        Duration::from_secs_f64(retry.clamp(0.5, MAX_WAIT.as_secs_f64()));
                    (self.report)(&alloc::format!(
                        "rate limited; waiting {:.1} s as asked",
                        self.rate_wait.as_secs_f64()
                    ));
                    if attempts >= ATTEMPTS {
                        bail!("{path}: rate limited {ATTEMPTS} times in a row; stopping");
                    }
                }
                403 | 404 if optional => {
                    (self.report)(&alloc::format!(
                        "HTTP {} for {path}: this listing is not available; going on without it",
                        resp.status
                    ));
                    return Ok(None);
                }
                401 | 403 => bail!(
                    "Discord answered HTTP {} for {path}: the token was not accepted, or the account cannot read this channel. Stopping; nothing else is tried. Check {TOKEN_VAR} in the env file and that the channel opens in the Discord client.",
                    resp.status
                ),
                404 => bail!(
                    "Discord answered HTTP 404 for {path}: no such channel, or the account is not in its server"
                ),
                500..=599 => {
                    self.backoff = (self.backoff * 2).clamp(FIRST_BACKOFF, MAX_WAIT);
                    if attempts >= ATTEMPTS {
                        bail!(
                            "Discord answered HTTP {} for {path} {ATTEMPTS} times; giving up",
                            resp.status
                        );
                    }
                    (self.report)(&alloc::format!(
                        "HTTP {} for {path}; trying again in {:.0} s",
                        resp.status,
                        self.backoff.as_secs_f64()
                    ));
                }
                status => {
                    let text: String = resp.body.chars().take(200).collect();
                    bail!("Discord answered HTTP {status} for {path}: {text}");
                }
            }
        }
    }

    fn get(&mut self, path: &str) -> Result<Value> {
        Ok(self.request(path, false)?.unwrap_or(Value::Null))
    }

    fn cursor(&mut self, id: &str) -> &mut Cursor {
        self.state
            .conversations
            .entry(String::from(id))
            .or_default()
    }

    /// Every given channel, then its threads
    fn fetch_all(&mut self) -> Result<()> {
        for id in self.options.channels.clone() {
            let info = self.get(&alloc::format!("/channels/{id}"))?;
            if self.guild.is_none() {
                self.guild = info["guild_id"].as_str().map(String::from);
            }
            let folder = if self.options.flat {
                self.root.clone()
            } else {
                let guild = info["guild_id"].as_str().unwrap_or("guild");
                self.root.join(guild).join(&id)
            };
            keep(&folder.join(alloc::format!("{id}.channel.json")), &info)?;
            let name = String::from(info["name"].as_str().unwrap_or(&id));
            self.cursor(&id).name = name.clone();
            (self.report)(&alloc::format!("#{name} ({id}) into {}", folder.display()));
            let mut threads = Vec::new();
            let forum = info["type"]
                .as_u64()
                .is_some_and(|t| FORUM_TYPES.contains(&t));
            if !forum {
                threads.extend(self.fetch_messages(
                    &id,
                    &folder,
                    info["last_message_id"].as_str(),
                )?);
            }
            if self.options.threads {
                threads.extend(self.list_threads(&id)?);
            }
            let mut seen = BTreeSet::new();
            for t in threads {
                let (Some(tid), Some(parent)) = (t["id"].as_str(), t["parent_id"].as_str()) else {
                    continue;
                };
                // Only this channel's threads, each once
                if parent != id || !seen.insert(String::from(tid)) {
                    continue;
                }
                self.allowed.insert(String::from(tid));
                let dir = folder.join("threads");
                keep(&dir.join(alloc::format!("{tid}.channel.json")), &t)?;
                let cursor = self.cursor(tid);
                cursor.name = String::from(t["name"].as_str().unwrap_or(tid));
                cursor.parent = Some(id.clone());
                self.fetch_messages(tid, &dir, t["last_message_id"].as_str())?;
            }
        }
        Ok(())
    }

    /// The channel's threads: active ones (through the server's listing,
    /// when the account has it) and archived public ones, as the API's
    /// thread objects
    fn list_threads(&mut self, id: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        if let Some(g) = self.guild.clone()
            && let Some(active) =
                self.request(&alloc::format!("/guilds/{g}/threads/active"), true)?
        {
            out.extend(
                active["threads"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|t| t["parent_id"].as_str() == Some(id))
                    .cloned(),
            );
        }
        let mut before: Option<String> = None;
        loop {
            let q = before
                .as_deref()
                .map(|b| alloc::format!("&before={}", encode(b)))
                .unwrap_or_default();
            let Some(page) = self.request(
                &alloc::format!("/channels/{id}/threads/archived/public?limit={PAGE}{q}"),
                true,
            )?
            else {
                break;
            };
            let threads = page["threads"].as_array().cloned().unwrap_or_default();
            before = threads
                .last()
                .and_then(|t| t["thread_metadata"]["archive_timestamp"].as_str())
                .map(String::from);
            out.extend(threads);
            if !page["has_more"].as_bool().unwrap_or(false) || before.is_none() {
                break;
            }
        }
        (self.report)(&alloc::format!("{} threads listed", out.len()));
        Ok(out)
    }

    /// A conversation's messages into `dir/<id>.messages.jsonl`: new ones
    /// since the last run (skipped when `last_message_id` says there are
    /// none), then older history until the start. Returns the thread
    /// objects seen on messages that started a thread.
    fn fetch_messages(
        &mut self,
        id: &str,
        dir: &Path,
        last_message_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let path = dir.join(alloc::format!("{id}.messages.jsonl"));
        let mut threads = Vec::new();
        let base = alloc::format!("/channels/{id}/messages?limit={PAGE}");
        match self.cursor(id).newest.clone() {
            None => {
                let page = self.get(&base)?;
                if self.take_page(id, &path, &page, &mut threads)? < PAGE {
                    self.cursor(id).complete = true;
                }
            }
            Some(mut newest) => {
                // Nothing new when the channel's last message is the one kept
                let mut stale = last_message_id.is_none_or(|l| snowflake(l) > snowflake(&newest));
                while stale {
                    let page = self.get(&alloc::format!("{base}&after={newest}"))?;
                    stale = self.take_page(id, &path, &page, &mut threads)? == PAGE;
                    newest = self.cursor(id).newest.clone().unwrap_or(newest);
                }
            }
        }
        while !self.cursor(id).complete {
            let Some(oldest) = self.cursor(id).oldest.clone() else {
                break;
            };
            let page = self.get(&alloc::format!("{base}&before={oldest}"))?;
            if self.take_page(id, &path, &page, &mut threads)? < PAGE {
                self.cursor(id).complete = true;
            }
        }
        Ok(threads)
    }

    /// Appends a page of messages as received, moves the cursor, saves the
    /// state and reports; returns the page's size
    fn take_page(
        &mut self,
        id: &str,
        path: &Path,
        page: &Value,
        threads: &mut Vec<Value>,
    ) -> Result<usize> {
        let messages = page
            .as_array()
            .with_context(|| alloc::format!("{}: expected a list of messages", path.display()))?;
        let mut lines = String::new();
        let mut newest: Option<u64> = None;
        let mut oldest: Option<(u64, Option<DateTime<Utc>>)> = None;
        for m in messages {
            let Some(mid) = m["id"].as_str() else {
                continue;
            };
            lines.push_str(&serde_json::to_string(m)?);
            lines.push('\n');
            if m["thread"].is_object() {
                threads.push(m["thread"].clone());
            }
            let n = snowflake(mid);
            newest = Some(newest.map_or(n, |x| x.max(n)));
            if oldest.is_none_or(|(o, _)| n < o) {
                let at = m["timestamp"]
                    .as_str()
                    .and_then(|t| t.parse::<DateTime<Utc>>().ok());
                oldest = Some((n, at));
            }
        }
        if !lines.is_empty() {
            std::fs::create_dir_all(path.parent().context("no folder")?)?;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| alloc::format!("opening {}", path.display()))?;
            file.write_all(lines.as_bytes())?;
        }
        let cursor = self.cursor(id);
        if let Some(n) = newest
            && cursor.newest.as_deref().is_none_or(|c| snowflake(c) < n)
        {
            cursor.newest = Some(n.to_string());
        }
        if let Some((n, at)) = oldest
            && cursor.oldest.as_deref().is_none_or(|c| snowflake(c) > n)
        {
            cursor.oldest = Some(n.to_string());
            cursor.oldest_at = at;
        }
        cursor.messages += messages.len() as u64;
        let (name, kept, oldest_at) = (cursor.name.clone(), cursor.messages, cursor.oldest_at);
        let line = alloc::format!(
            "#{name}: {kept} messages, oldest {}; {} requests this run, {} today; delay {:.1} s",
            oldest_at.map_or_else(|| String::from("-"), |t| t.format("%Y-%m-%d").to_string()),
            self.requests,
            self.state.requests_today,
            self.delay.as_secs_f64()
        );
        self.added += messages.len() as u64;
        self.state.save(&self.root)?;
        (self.report)(&line);
        Ok(messages.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use serde_json::json;

    /// Answers scripted requests in order, checking each path
    struct Fake {
        expected: VecDeque<(String, Response)>,
        log: Vec<String>,
    }

    impl Fake {
        fn new(script: Vec<(&str, Response)>) -> Self {
            Fake {
                expected: script
                    .into_iter()
                    .map(|(p, r)| (String::from(p), r))
                    .collect(),
                log: Vec::new(),
            }
        }
    }

    impl Http for Fake {
        fn get(&mut self, url: &str, token: &str) -> Result<Response> {
            assert_eq!(token, "sekrit-9f8e7d");
            let path = url.strip_prefix(API).expect("the API base");
            let (want, resp) = self
                .expected
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected request {path}"));
            assert_eq!(path, want);
            self.log.push(String::from(path));
            Ok(resp)
        }
    }

    /// Records sleeps instead of sleeping; can stop at the n-th
    #[derive(Default)]
    struct FakeClock {
        slept: Vec<Duration>,
        stop_at: Option<usize>,
        day: String,
    }

    impl Clock for FakeClock {
        fn sleep(&mut self, d: Duration) -> bool {
            self.slept.push(d);
            !self.stopped()
        }
        fn stopped(&self) -> bool {
            self.stop_at.is_some_and(|n| self.slept.len() >= n)
        }
        fn elapsed(&self) -> Duration {
            self.slept.iter().sum()
        }
        fn today(&self) -> String {
            self.day.clone()
        }
    }

    fn ok(body: Value) -> Response {
        Response {
            status: 200,
            headers: Vec::new(),
            body: body.to_string(),
        }
    }

    fn status(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: Vec::new(),
            body: String::from(body),
        }
    }

    fn message(id: u64) -> Value {
        let at = DateTime::<Utc>::from_timestamp(1_700_000_000 + id as i64 * 60, 0).unwrap();
        json!({"id": id.to_string(), "channel_id": "2", "timestamp": at.to_rfc3339(),
               "content": alloc::format!("m{id}"), "author": {"id": "5", "username": "alice"}})
    }

    /// Messages with these ids, newest first as the API sends them
    fn messages(ids: impl Iterator<Item = u64>) -> Value {
        let mut v: Vec<Value> = ids.map(message).collect();
        v.reverse();
        Value::Array(v)
    }

    fn channel(id: &str, kind: u64, last: u64) -> Value {
        json!({"id": id, "type": kind, "guild_id": "1", "name": alloc::format!("c{id}"),
               "last_message_id": last.to_string()})
    }

    fn thread(id: &str, parent: &str, last: u64) -> Value {
        json!({"id": id, "type": 11, "guild_id": "1", "parent_id": parent, "name": alloc::format!("t{id}"),
               "last_message_id": last.to_string(),
               "thread_metadata": {"archived": true, "archive_timestamp": "2024-05-01T00:00:00+00:00"}})
    }

    /// One channel into a flat folder, without the longer pauses
    fn options(threads: bool) -> Options {
        Options {
            channels: alloc::vec![String::from("2")],
            guild: None,
            threads,
            flat: true,
            pace: Pace {
                pause_every: Range::new(1e9, 1e9),
                ..Pace::default()
            },
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-fetch-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn go(
        fake: &mut Fake,
        clock: &mut FakeClock,
        out: &Path,
        options: Options,
    ) -> Result<(Summary, Vec<String>)> {
        let mut lines = Vec::new();
        let summary = run(
            fake,
            clock,
            String::from("sekrit-9f8e7d"),
            out,
            options,
            &mut |l| lines.push(String::from(l)),
        )?;
        Ok((summary, lines))
    }

    fn line_count(path: &Path) -> usize {
        std::fs::read_to_string(path).unwrap().lines().count()
    }

    #[test]
    fn pages_resume_and_pace() {
        let out = temp("pages");
        // First run: the latest page (full), then history until a short page
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 300))),
            ("/channels/2/messages?limit=100", ok(messages(201..=300))),
            (
                "/channels/2/messages?limit=100&before=201",
                ok(messages(101..=150))
            ),
        ]);
        let mut clock = FakeClock::default();
        let (summary, lines) = go(&mut fake, &mut clock, &out, options(false)).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.requests, 3);
        assert_eq!(summary.messages, 150);
        assert_eq!(summary.stopped, None);
        // No wait before the first request; 3 to 8 s drawn before the others
        assert_eq!(clock.slept.len(), 2);
        for d in &clock.slept {
            assert!((3.0..=8.0).contains(&d.as_secs_f64()), "{d:?}");
        }
        assert!(lines.iter().any(|l| l.contains("150 messages")));
        assert_eq!(line_count(&out.join("2.messages.jsonl")), 150);
        assert!(out.join("2.channel.json").is_file());
        let state = State::load(&out).unwrap();
        let c = &state.conversations["2"];
        assert_eq!(c.newest.as_deref(), Some("300"));
        assert_eq!(c.oldest.as_deref(), Some("101"));
        assert!(c.complete);
        assert_eq!(state.requests_today, 3);

        // Second run: five new messages, nothing older
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 305))),
            (
                "/channels/2/messages?limit=100&after=300",
                ok(messages(301..=305))
            ),
        ]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, options(false)).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.messages, 5);
        assert_eq!(line_count(&out.join("2.messages.jsonl")), 155);
        assert_eq!(
            State::load(&out).unwrap().conversations["2"]
                .newest
                .as_deref(),
            Some("305")
        );

        // Third run: the channel's last message is known, so no page is read
        let mut fake = Fake::new(alloc::vec![("/channels/2", ok(channel("2", 0, 305)))]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, options(false)).unwrap();
        assert_eq!(summary.requests, 1);
        assert_eq!(summary.messages, 0);
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn interrupted_history_resumes_older() {
        let out = temp("resume");
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 300))),
            ("/channels/2/messages?limit=100", ok(messages(201..=300))),
        ]);
        // Stops at the second sleep, before the history page
        let mut clock = FakeClock {
            stop_at: Some(2),
            ..FakeClock::default()
        };
        let (summary, _) = go(&mut fake, &mut clock, &out, options(false)).unwrap();
        assert_eq!(summary.stopped, Some(Stop::Interrupted));
        assert_eq!(summary.requests, 2);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 300))),
            (
                "/channels/2/messages?limit=100&before=201",
                ok(messages(101..=200))
            ),
            ("/channels/2/messages?limit=100&before=101", ok(json!([]))),
        ]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, options(false)).unwrap();
        assert_eq!(summary.stopped, None);
        assert!(State::load(&out).unwrap().conversations["2"].complete);
        assert_eq!(line_count(&out.join("2.messages.jsonl")), 200);
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn rate_limits_and_server_errors() {
        let out = temp("limits");
        let limited = Response {
            status: 429,
            headers: alloc::vec![(String::from("Retry-After"), String::from("3"))],
            body: String::from(r#"{"message": "slow down", "retry_after": 2.5}"#),
        };
        let mut nearly = ok(channel("2", 0, 10));
        nearly.headers = alloc::vec![
            (String::from("X-RateLimit-Remaining"), String::from("0")),
            (String::from("X-RateLimit-Reset-After"), String::from("7.5")),
        ];
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", limited),
            ("/channels/2", nearly),
            ("/channels/2/messages?limit=100", status(503, "")),
            ("/channels/2/messages?limit=100", status(502, "")),
            ("/channels/2/messages?limit=100", ok(messages(1..=10))),
        ]);
        let mut clock = FakeClock::default();
        // A short fixed pace, so the waits asked by the answers show
        let mut opts = options(false);
        opts.pace.delay = Range::new(1.0, 1.0);
        let (summary, lines) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert_eq!(summary.requests, 5);
        assert_eq!(summary.messages, 10);
        let secs: Vec<f64> = clock.slept.iter().map(Duration::as_secs_f64).collect();
        // retry_after from the body (over the header and the pace), then
        // the reset-after header, then 5 s and 10 s of backoff
        assert_eq!(secs.len(), 4);
        assert!((secs[0] - 2.5).abs() < 1e-6, "{secs:?}");
        assert!((secs[1] - 7.5).abs() < 1e-6, "{secs:?}");
        assert!((secs[2] - 5.0).abs() < 1e-6, "{secs:?}");
        assert!((secs[3] - 10.0).abs() < 1e-6, "{secs:?}");
        assert!(lines.iter().any(|l| l.starts_with("rate limited")));
        assert!(lines.iter().any(|l| l.starts_with("HTTP 503")));
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn stops_on_forbidden_without_leaking_the_token() {
        let out = temp("forbidden");
        let mut fake = Fake::new(alloc::vec![(
            "/channels/2",
            status(403, r#"{"message": "no"}"#)
        )]);
        let err = go(&mut fake, &mut FakeClock::default(), &out, options(false)).unwrap_err();
        let text = alloc::format!("{err:#}");
        assert!(text.contains("HTTP 403"), "{text}");
        assert!(text.contains("Stopping"), "{text}");
        assert!(!text.contains("sekrit"), "{text}");
        assert_eq!(fake.log.len(), 1);
        // 401 the same
        let mut fake = Fake::new(alloc::vec![("/channels/2", status(401, ""))]);
        let err = go(&mut fake, &mut FakeClock::default(), &out, options(false)).unwrap_err();
        assert!(alloc::format!("{err:#}").contains("HTTP 401"));
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn scope_is_the_given_channels_and_their_threads() {
        let allowed: BTreeSet<String> = [String::from("2")].into_iter().collect();
        assert!(in_scope("/channels/2", &allowed, None));
        assert!(in_scope(
            "/channels/2/messages?limit=100&before=5",
            &allowed,
            None
        ));
        assert!(in_scope(
            "/channels/2/threads/archived/public?limit=100",
            &allowed,
            None
        ));
        assert!(!in_scope("/channels/9/messages?limit=100", &allowed, None));
        assert!(!in_scope("/users/@me", &allowed, None));
        assert!(!in_scope("/guilds/1/threads/active", &allowed, None));
        assert!(in_scope("/guilds/1/threads/active", &allowed, Some("1")));
        assert!(!in_scope("/guilds/1/channels", &allowed, Some("1")));

        // A forum: no messages of its own; only its threads are read, even
        // when the server's listing names threads of other channels. Not
        // flat: the channel's files go to <root>/<guild>/<channel>/
        let out = temp("scope");
        let mut opts = options(true);
        opts.flat = false;
        let folder = out.join("1/2");
        let active = json!({"threads": [thread("30", "2", 31), thread("40", "9", 41)]});
        let archived = json!({"threads": [thread("50", "2", 51)], "has_more": false});
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 15, 0))),
            ("/guilds/1/threads/active", ok(active)),
            (
                "/channels/2/threads/archived/public?limit=100",
                ok(archived)
            ),
            ("/channels/30/messages?limit=100", ok(messages(30..=31))),
            ("/channels/50/messages?limit=100", ok(messages(50..=51))),
        ]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, opts.clone()).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.conversations, 3);
        assert!(folder.join("2.channel.json").is_file());
        assert!(folder.join("threads/30.channel.json").is_file());
        assert!(folder.join("threads/50.messages.jsonl").is_file());
        assert!(!folder.join("threads/40.channel.json").exists());
        assert!(out.join("state.json").is_file());
        let state = State::load(&out).unwrap();
        assert_eq!(state.conversations["30"].parent.as_deref(), Some("2"));

        // Next run: the listing is not available (403), the archived one
        // pages by archive time; known threads with no new messages are not
        // read again
        let page1 = json!({"threads": [thread("50", "2", 51)], "has_more": true});
        let page2 = json!({"threads": [thread("60", "2", 61)], "has_more": false});
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 15, 0))),
            ("/guilds/1/threads/active", status(403, "")),
            ("/channels/2/threads/archived/public?limit=100", ok(page1)),
            (
                "/channels/2/threads/archived/public?limit=100&before=2024-05-01T00%3A00%3A00%2B00%3A00",
                ok(page2),
            ),
            ("/channels/60/messages?limit=100", ok(messages(60..=61))),
        ]);
        let (summary, lines) = go(&mut fake, &mut FakeClock::default(), &out, opts).unwrap();
        assert!(fake.expected.is_empty(), "{:?}", fake.expected);
        assert_eq!(summary.messages, 2);
        assert!(lines.iter().any(|l| l.contains("not available")));
        assert!(folder.join("threads/60.messages.jsonl").is_file());
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn pauses_and_session_limits() {
        // A pause every second request, of 100 s
        let out = temp("pauses");
        let mut opts = options(false);
        opts.pace.delay = Range::new(2.0, 2.0);
        opts.pace.pause_every = Range::new(2.0, 2.0);
        opts.pace.pause = Range::new(100.0, 100.0);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 400))),
            ("/channels/2/messages?limit=100", ok(messages(301..=400))),
            (
                "/channels/2/messages?limit=100&before=301",
                ok(messages(201..=300))
            ),
            (
                "/channels/2/messages?limit=100&before=201",
                ok(messages(101..=200))
            ),
            (
                "/channels/2/messages?limit=100&before=101",
                ok(messages(1..=50))
            ),
        ]);
        let mut clock = FakeClock::default();
        let (summary, lines) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert_eq!(summary.requests, 5);
        let secs: Vec<f64> = clock.slept.iter().map(Duration::as_secs_f64).collect();
        assert_eq!(secs, [2.0, 100.0, 2.0, 100.0]);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("pausing 1.7 min"))
                .count(),
            2,
            "{lines:?}"
        );

        // A run of at most 0.1 minutes with 5 s between requests: the third
        // request starts at 5 s, the fourth would start at 10 s and is not
        // made
        let mut opts = options(false);
        opts.pace.delay = Range::new(5.0, 5.0);
        opts.pace.max_minutes = Some(0.1);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 700))),
            (
                "/channels/2/messages?limit=100&after=400",
                ok(messages(401..=500))
            ),
            (
                "/channels/2/messages?limit=100&after=500",
                ok(messages(501..=600))
            ),
        ]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, opts).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.stopped, Some(Stop::MaxMinutes(0)));
        assert_eq!(summary.requests, 3);
        assert_eq!(
            State::load(&out).unwrap().conversations["2"]
                .newest
                .as_deref(),
            Some("600")
        );
        std::fs::remove_dir_all(&out).unwrap();

        assert_eq!("3-8".parse::<Range>().unwrap(), Range::new(3.0, 8.0));
        assert_eq!("4".parse::<Range>().unwrap(), Range::new(4.0, 4.0));
        assert!("8-3".parse::<Range>().is_err());
        assert!("x".parse::<Range>().is_err());
        assert_eq!(Range::new(60.0, 300.0).to_string(), "60-300");
    }

    #[test]
    fn threads_started_in_a_text_channel_are_read() {
        let out = temp("started");
        let mut starter = message(5);
        starter["thread"] = thread("70", "2", 71);
        let mut foreign = message(4);
        foreign["thread"] = thread("80", "9", 81);
        let page = Value::Array(alloc::vec![starter, foreign, message(3)]);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 5))),
            ("/channels/2/messages?limit=100", ok(page)),
            ("/guilds/1/threads/active", ok(json!({"threads": []}))),
            (
                "/channels/2/threads/archived/public?limit=100",
                status(404, "")
            ),
            ("/channels/70/messages?limit=100", ok(messages(70..=71))),
        ]);
        let (summary, _) = go(&mut fake, &mut FakeClock::default(), &out, options(true)).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.messages, 5);
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn caps_and_limits() {
        let out = temp("caps");
        let mut opts = options(false);
        opts.pace.max_requests = Some(1);
        let mut fake = Fake::new(alloc::vec![("/channels/2", ok(channel("2", 0, 3)))]);
        let mut clock = FakeClock {
            day: String::from("2026-09-26"),
            ..FakeClock::default()
        };
        let (summary, _) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert_eq!(summary.stopped, Some(Stop::MaxRequests(1)));
        assert_eq!(summary.requests, 1);
        let state = State::load(&out).unwrap();
        assert_eq!(state.day, "2026-09-26");
        assert_eq!(state.requests_today, 1);

        // Same day, cap of 1: nothing more is asked
        let mut opts = options(false);
        opts.pace.daily_cap = Some(1);
        let mut fake = Fake::new(Vec::new());
        let (summary, _) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        assert_eq!(summary.stopped, Some(Stop::DailyCap(1)));
        assert_eq!(summary.requests, 0);

        // Next day the count starts over
        clock.day = String::from("2026-09-27");
        let mut fake = Fake::new(alloc::vec![("/channels/2", ok(channel("2", 0, 3)))]);
        let (summary, _) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert_eq!(summary.stopped, Some(Stop::DailyCap(1)));
        assert_eq!(summary.requests, 1);
        assert_eq!(State::load(&out).unwrap().requests_today, 1);
        std::fs::remove_dir_all(&out).unwrap();
    }
}
