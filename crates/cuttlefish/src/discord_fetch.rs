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
//!   thread listings (active, archived public), and for `--count` the
//!   server's message search limited to the channel. No gateway, no
//!   typing, no writes.
//! - **Pace**: a random delay between requests (3 to 8 s by default),
//!   longer pauses now and then, 429 `retry_after` and the `X-RateLimit-*`
//!   headers obeyed, exponential backoff on server errors, a stop on 401
//!   and 403, and an optional cap on requests per day.
//! - **Headers** of the user's own browser when given ([`Browser`]:
//!   `DISCORD_USER_AGENT`, `DISCORD_SUPER_PROPERTIES`, `DISCORD_LOCALE`,
//!   `DISCORD_TIMEZONE`), else a current desktop browser's User-Agent with
//!   the system's language and timezone. `X-Super-Properties` is sent only
//!   as the user gave it, never made up.
//! - **Resumable**: the API's JSON is kept as received in the output folder
//!   (`<id>.channel.json`, `<id>.messages.jsonl` appended, threads in
//!   `threads/`) with the cursors in `state.json`, so a run continues: new
//!   messages since the last run first, then older history until the
//!   start. Ctrl+C finishes the request under way and saves.
//! - **Attachments** ([`crate::discord_media`]): after the messages, the
//!   channel's uploaded files are counted (`--attachments list`, the
//!   default) or downloaded (`videos`, `media`), one at a time at the same
//!   pace, without the token, only from Discord's CDN and only the files
//!   of the messages read. Their signed links expire after about a day, so
//!   a message whose link expired has its page read again (an ordinary
//!   paced message GET) for a fresh one. Files land in
//!   `<media>/<guild>/<channel>/<message id>/<file name>` with a
//!   `media.jsonl` manifest; a file already there is skipped, a partial
//!   one continued.
//!
//! The token comes from `DISCORD_USER_TOKEN` in the environment (the env
//! file, [`crate::env_file`]) and is never logged or written. The inbox
//! reads the output ([`crate::inbox`], [`crate::discord::read_archive`]).

use crate::crawl::encode;
use crate::discord::{API, FORUM_TYPES, snowflake};
use crate::discord_media::{self, Attachment, Attachments, Entry, Listing, Manifest, Tally};
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
/// A current desktop browser (Firefox of September 2026 on Linux), when
/// the user gives none of their own; Firefox sends no client hints, so the
/// other headers stay consistent with it
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:155.0) Gecko/20100101 Firefox/155.0";
/// The user's browser's `User-Agent`, copied from its DevTools
pub const USER_AGENT_VAR: &str = "DISCORD_USER_AGENT";
/// The user's browser's `X-Super-Properties`, copied from its DevTools
pub const SUPER_PROPERTIES_VAR: &str = "DISCORD_SUPER_PROPERTIES";
/// `X-Discord-Locale`, by default the system's language
pub const LOCALE_VAR: &str = "DISCORD_LOCALE";
/// `X-Discord-Timezone`, by default the system's timezone
pub const TIMEZONE_VAR: &str = "DISCORD_TIMEZONE";
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
    /// GETs `url` with exactly these headers (names and values)
    fn get(&mut self, url: &str, headers: &[(String, String)]) -> Result<Response>;

    /// GETs `url` with these headers and streams the body into `out` when
    /// the status is 200 or 206 (the answer's `body` is then empty); with
    /// any other status the body is returned, not written
    fn download(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        out: &mut dyn Write,
    ) -> Result<Response>;
}

/// HTTPS through ureq, with no headers of its own but the transfer's
pub struct Https {
    agent: ureq::Agent,
}

impl Default for Https {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .user_agent(ureq::config::AutoHeaderValue::None)
            .accept(ureq::config::AutoHeaderValue::None)
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Https { agent }
    }
}

impl Http for Https {
    fn get(&mut self, url: &str, headers: &[(String, String)]) -> Result<Response> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut resp = request
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

    fn download(
        &mut self,
        url: &str,
        headers: &[(String, String)],
        out: &mut dyn Write,
    ) -> Result<Response> {
        let mut request = self.agent.get(url);
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let mut resp = request
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
        let body = if matches!(status, 200 | 206) {
            // Unlimited: a file of any size, straight to disk
            std::io::copy(&mut resp.body_mut().as_reader(), out)
                .with_context(|| alloc::format!("downloading {url}"))?;
            String::new()
        } else {
            resp.body_mut().read_to_string().unwrap_or_default()
        };
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

/// Where a header's value came from, for the start line (never the value)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderSource {
    /// The environment variable of that name (the env file)
    Var(&'static str),
    /// The `browser_user_agent` inside `DISCORD_SUPER_PROPERTIES`
    SuperProperties,
    /// This machine's settings
    System,
    /// The built-in value
    BuiltIn,
}

impl fmt::Display for HeaderSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderSource::Var(name) => write!(f, "{name}"),
            HeaderSource::SuperProperties => {
                write!(f, "{SUPER_PROPERTIES_VAR} (its browser_user_agent)")
            }
            HeaderSource::System => write!(f, "the system"),
            HeaderSource::BuiltIn => write!(f, "built in"),
        }
    }
}

/// The headers a Discord web client sends with each API request, taken
/// from the user's own browser where given (copied from DevTools into the
/// env file) and sent exactly as given
#[derive(Clone, Debug, PartialEq)]
pub struct Browser {
    pub user_agent: (String, HeaderSource),
    /// `X-Super-Properties`: only what the user gave, never made up
    pub super_properties: Option<String>,
    /// `X-Discord-Locale` (`en-US`), also the base of `Accept-Language`
    pub locale: (String, HeaderSource),
    /// `X-Discord-Timezone` (`Europe/Berlin`), when known
    pub timezone: Option<(String, HeaderSource)>,
}

impl Default for Browser {
    /// The built-in browser, in American English, without a timezone
    fn default() -> Self {
        Browser {
            user_agent: (String::from(DEFAULT_USER_AGENT), HeaderSource::BuiltIn),
            super_properties: None,
            locale: (String::from("en-US"), HeaderSource::BuiltIn),
            timezone: None,
        }
    }
}

/// A locale as Discord writes it (`en-US`, `ja`) from a POSIX one
/// (`en_US.UTF-8`); `C` and `POSIX` are none
fn discord_locale(posix: &str) -> Option<String> {
    let base = posix.split(['.', '@']).next()?.trim();
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    Some(base.replace('_', "-"))
}

/// The system's timezone: `TZ` when it names a zone, else `/etc/timezone`,
/// else where `/etc/localtime` points in the zoneinfo tree
fn system_timezone() -> Option<String> {
    let zone = |s: &str| {
        let s = s.trim().trim_start_matches(':');
        (s.contains('/') && !s.starts_with('/')).then(|| String::from(s))
    };
    std::env::var("TZ")
        .ok()
        .and_then(|tz| zone(&tz))
        .or_else(|| zone(&std::fs::read_to_string("/etc/timezone").ok()?))
        .or_else(|| {
            let target = std::fs::read_link("/etc/localtime").ok()?;
            let target = target.to_string_lossy();
            zone(target.split_once("zoneinfo/")?.1)
        })
}

impl Browser {
    /// From the environment (the env file) and the system
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|v| var(v).filter(|s| !s.is_empty()))
            .and_then(|l| discord_locale(&l));
        Browser::from_vars(var, locale, system_timezone())
    }

    /// From variables looked up with `var`, and the system's locale and
    /// timezone
    pub fn from_vars(
        var: impl Fn(&str) -> Option<String>,
        system_locale: Option<String>,
        system_timezone: Option<String>,
    ) -> Self {
        let var = |name: &'static str| {
            var(name)
                .map(|v| String::from(v.trim()))
                .filter(|v| !v.is_empty())
                .map(|v| (v, HeaderSource::Var(name)))
        };
        let super_properties = var(SUPER_PROPERTIES_VAR).map(|(v, _)| v);
        // The agent the super properties name, so the two agree
        let named = super_properties.as_deref().and_then(|p| {
            use base64::Engine;
            let json = base64::engine::general_purpose::STANDARD.decode(p).ok()?;
            let value: Value = serde_json::from_slice(&json).ok()?;
            let agent = value["browser_user_agent"].as_str()?;
            (!agent.is_empty()).then(|| (String::from(agent), HeaderSource::SuperProperties))
        });
        let builtin = Browser::default();
        Browser {
            user_agent: var(USER_AGENT_VAR).or(named).unwrap_or(builtin.user_agent),
            super_properties,
            locale: var(LOCALE_VAR)
                .or_else(|| system_locale.map(|l| (l, HeaderSource::System)))
                .unwrap_or(builtin.locale),
            timezone: var(TIMEZONE_VAR)
                .or_else(|| system_timezone.map(|t| (t, HeaderSource::System))),
        }
    }

    /// `Accept-Language` as Firefox writes it: `en-US,en;q=0.5`
    fn accept_language(&self) -> String {
        let locale = &self.locale.0;
        match locale.split_once('-') {
            Some((lang, _)) => alloc::format!("{locale},{lang};q=0.5"),
            None => locale.clone(),
        }
    }

    /// The headers of a request made from the page `referer` (none when
    /// the server is not known yet), with the token
    pub fn headers(&self, token: &str, referer: Option<&str>) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut add = |k: &str, v: &str| out.push((String::from(k), String::from(v)));
        add("User-Agent", &self.user_agent.0);
        add("Accept", "*/*");
        add("Accept-Language", &self.accept_language());
        add("Authorization", token);
        if let Some(p) = &self.super_properties {
            add("X-Super-Properties", p);
        }
        add("X-Discord-Locale", &self.locale.0);
        if let Some((tz, _)) = &self.timezone {
            add("X-Discord-Timezone", tz);
        }
        if let Some(r) = referer {
            add("Referer", r);
        }
        add("Sec-Fetch-Dest", "empty");
        add("Sec-Fetch-Mode", "cors");
        add("Sec-Fetch-Site", "same-origin");
        out
    }

    /// The headers of a file download from the page `referer`: the same
    /// browser, without the token (the CDN never gets it)
    pub fn media_headers(&self, referer: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut add = |k: &str, v: &str| out.push((String::from(k), String::from(v)));
        add("User-Agent", &self.user_agent.0);
        add("Accept", "*/*");
        add("Accept-Language", &self.accept_language());
        add("Referer", referer);
        add("Sec-Fetch-Dest", "empty");
        add("Sec-Fetch-Mode", "cors");
        add("Sec-Fetch-Site", "cross-site");
        out
    }

    /// Where each header comes from, without the values
    pub fn describe(&self) -> String {
        let mut text = alloc::format!(
            "Headers: User-Agent from {}; X-Super-Properties {}; locale from {}; timezone {}",
            self.user_agent.1,
            if self.super_properties.is_some() {
                alloc::format!("from {SUPER_PROPERTIES_VAR}")
            } else {
                alloc::format!("not sent (set {SUPER_PROPERTIES_VAR} to send your browser's)")
            },
            self.locale.1,
            self.timezone.as_ref().map_or_else(
                || String::from("not sent (unknown)"),
                |(_, from)| alloc::format!("from {from}")
            ),
        );
        if self.super_properties.is_some() && self.user_agent.1 == HeaderSource::BuiltIn {
            text.push_str(&alloc::format!(
                "; {SUPER_PROPERTIES_VAR} names no browser: set {USER_AGENT_VAR} to that browser's"
            ));
        }
        text
    }
}

/// A channel as given: an id, `<server id>/<channel id>`, or a channel's
/// link (`https://discord.com/channels/<server>/<channel>[/<message>]`,
/// also on `ptb.`, `canary.` and `discordapp.com`); the server, when the
/// form names it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelRef {
    pub guild: Option<String>,
    pub channel: String,
}

impl core::str::FromStr for ChannelRef {
    type Err = String;

    fn from_str(s: &str) -> core::result::Result<Self, String> {
        let text = s.trim();
        let bad = |why: &str| {
            alloc::format!(
                "{text:?} {why}. Give the channel as one of:\n  \
                 its link   https://discord.com/channels/<server id>/<channel id>  (right-click the channel > Copy Link)\n  \
                 two ids    <server id>/<channel id>\n  \
                 its id     <channel id>  (Developer Mode, right-click the channel > Copy Channel ID)"
            )
        };
        let id = |t: &str| {
            (!t.is_empty() && t.len() <= 20 && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| String::from(t))
        };
        let parts: Vec<&str> = match text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))
        {
            Some(rest) => {
                let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
                let host = host.to_lowercase();
                let host = ["www.", "ptb.", "canary."]
                    .iter()
                    .find_map(|p| host.strip_prefix(p))
                    .unwrap_or(&host);
                if host != "discord.com" && host != "discordapp.com" {
                    return Err(bad("is not a Discord link"));
                }
                let path = path.split(['?', '#']).next().unwrap_or_default();
                let Some(path) = path.strip_prefix("channels/") else {
                    return Err(bad("is not a channel's link"));
                };
                let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
                if parts.first() == Some(&"@me") {
                    return Err(bad("is a direct message; only server channels are read"));
                }
                match parts[..] {
                    [g, c] | [g, c, _] => alloc::vec![g, c],
                    _ => return Err(bad("is not a channel's link")),
                }
            }
            None => text.split('/').collect(),
        };
        match parts[..] {
            [c] => Ok(ChannelRef {
                guild: None,
                channel: id(c).ok_or_else(|| bad("is not a channel id"))?,
            }),
            [g, c] => Ok(ChannelRef {
                guild: Some(id(g).ok_or_else(|| bad("does not start with a server id"))?),
                channel: id(c).ok_or_else(|| bad("does not end with a channel id"))?,
            }),
            _ => Err(bad("is not a channel")),
        }
    }
}

/// The server of the given channels: `--guild`, else the one their links
/// name; an error when they name different ones
pub fn guild_of(guild: Option<String>, channels: &[ChannelRef]) -> Result<Option<String>> {
    let named: BTreeSet<&str> = channels.iter().filter_map(|c| c.guild.as_deref()).collect();
    if let Some(g) = &guild
        && let Some(other) = named.iter().find(|n| **n != g)
    {
        bail!("--guild {g} but a channel's link names server {other}");
    }
    if named.len() > 1 {
        bail!("the channels are in different servers; fetch one server at a time");
    }
    Ok(guild.or_else(|| named.first().map(|g| String::from(*g))))
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
    /// The time (Unix seconds), for the expiry of attachment links
    fn now(&self) -> i64 {
        Utc::now().timestamp()
    }
}

/// A file being written, which stops when the run is told to (the partial
/// file stays for the next run)
struct Stopping<'a> {
    file: std::fs::File,
    clock: &'a dyn Clock,
}

impl Write for Stopping<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.clock.stopped() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "stopped",
            ));
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
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
    /// Only count the channels' messages (and a forum's posts); read no
    /// message ([`Count`])
    pub count: bool,
    pub pace: Pace,
    /// The headers sent
    pub browser: Browser,
    /// What to do with the messages' attachments after the messages
    pub attachments: Attachments,
    /// Files larger than this many MB are not downloaded
    pub max_file_mb: Option<u64>,
    /// A channel's media folder is not grown past this many GB
    pub max_total_gb: Option<f64>,
    /// The media root: files go to `<media>/<guild>/<channel>/`
    pub media: PathBuf,
}

/// What a run did about attachments ([`Options::attachments`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Media {
    /// What the channels' messages carry
    pub listing: Listing,
    /// Files downloaded this run
    pub downloaded: Tally,
    /// Wanted files that were already there
    pub present: usize,
    /// Wanted files larger than `--max-file-mb`
    pub skipped_size: Tally,
    /// Wanted files that would grow the folder past `--max-total-gb`
    pub skipped_total: Tally,
    /// Pages read again for fresh links
    pub refreshed: u32,
    /// Downloads that failed (reported as they happened)
    pub failed: usize,
    /// Links not on Discord's CDN, not downloaded
    pub out_of_scope: usize,
}

/// How one download ended
enum Outcome {
    /// The file is whole, `len` bytes long, `taken` of them this run
    Fetched { taken: u64, len: u64 },
    /// 403, 404 or 410: the link expired or the file is gone
    Gone,
    /// The server sent the whole file although a part was asked for; the
    /// part was removed, so the next attempt starts over
    Restart,
    /// Anything else, with the reason
    Failed(String),
}

/// What `--count` found out about a channel
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Count {
    pub id: String,
    pub name: String,
    /// A forum or media channel, whose posts are threads
    pub forum: bool,
    /// Messages Discord's search counts in the channel; `None` when it
    /// could not tell (no server, not indexed yet, not allowed)
    pub messages: Option<u64>,
    /// Posts of a forum, when its post listing tells
    pub posts: Option<u64>,
}

impl Count {
    /// Requests a whole fetch takes, about: the channel object, a page per
    /// 100 messages, and for a forum a listing page per 100 posts and at
    /// least one page per post
    pub fn requests(&self) -> Option<u64> {
        let messages = self.messages?;
        let posts = self.posts.unwrap_or(0);
        Some(1 + messages.div_ceil(PAGE as u64) + posts + posts.div_ceil(PAGE as u64))
    }
}

/// How long `requests` take at `pace`, on average: the mean delay each,
/// plus a mean pause every mean number of requests between pauses
pub fn estimate(requests: u64, pace: &Pace) -> Duration {
    let mean = |r: Range| (r.min + r.max) / 2.0;
    let pauses = (requests as f64 / mean(pace.pause_every).max(1.0)).floor();
    Duration::from_secs_f64(requests as f64 * mean(pace.delay) + pauses * mean(pace.pause))
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
    /// What `--count` found, a channel each
    pub counts: Vec<Count>,
    /// What was done about attachments; `None` with `--attachments none`,
    /// `--count`, or a run stopped by Ctrl+C
    pub media: Option<Media>,
}

/// Whether a request path is within the archive's scope: a channel or
/// thread in `allowed`, the active-threads listing of `guild`, or its
/// message search limited to channels in `allowed` (every `channel_id`
/// given is one, and at least one is given)
pub fn in_scope(path: &str, allowed: &BTreeSet<String>, guild: Option<&str>) -> bool {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let parts: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    match parts.as_slice() {
        ["channels", id, ..] => allowed.contains(*id),
        ["guilds", g, "threads", "active"] => guild == Some(*g),
        ["guilds", g, "messages", "search"] => {
            let mut channels = query
                .split('&')
                .filter_map(|kv| kv.strip_prefix("channel_id="))
                .peekable();
            guild == Some(*g) && channels.peek().is_some() && channels.all(|c| allowed.contains(c))
        }
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
    /// The last answer's HTTP status
    status: u16,
    /// The channel or thread being read, for `Referer`
    viewing: Option<String>,
    /// The channels read this run: id, server and archive folder, for
    /// their attachments
    channels_read: Vec<(String, String, PathBuf)>,
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
        status: 0,
        viewing: None,
        channels_read: Vec::new(),
        rng: seed.max(1),
        report,
    };
    f.until_pause = f.draw(f.options.pace.pause_every).round() as u32;
    let mut counts = Vec::new();
    let result = if f.options.count {
        f.count_all(&mut counts)
    } else {
        f.fetch_all()
    };
    // A stop ends the run early; any other error ends it
    let stop_of = |e: anyhow::Error| match e.downcast_ref::<Stop>() {
        Some(stop) => Ok(*stop),
        None => Err(e),
    };
    let mut stopped = match result {
        Ok(()) => None,
        Err(e) => {
            f.state.save(root)?;
            Some(stop_of(e)?)
        }
    };
    // The attachments: listed after any run but one interrupted, and
    // downloaded after a run that read everything
    let mut media = None;
    if !f.options.count
        && f.options.attachments != Attachments::None
        && stopped != Some(Stop::Interrupted)
    {
        let mut done = Media::default();
        if let Err(e) = f.media(&mut done, stopped.is_none()) {
            f.state.save(root)?;
            stopped = stopped.or(Some(stop_of(e)?));
        }
        media = Some(done);
    }
    f.state.save(root)?;
    Ok(Summary {
        requests: f.requests,
        messages: f.added,
        conversations: f.state.conversations.len(),
        stopped,
        counts,
        media,
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
            let referer = match (&self.guild, &self.viewing) {
                (Some(g), Some(c)) => Some(alloc::format!("https://discord.com/channels/{g}/{c}")),
                _ => None,
            };
            let headers = self
                .options
                .browser
                .headers(&self.token, referer.as_deref());
            let resp = match self.http.get(&url, &headers) {
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
            self.status = resp.status;
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

    /// Counts each given channel's messages through the server's search
    /// (limited to the channel), and a forum's posts through its post
    /// listing: the channel object and one or two requests more
    fn count_all(&mut self, counts: &mut Vec<Count>) -> Result<()> {
        for id in self.options.channels.clone() {
            self.viewing = Some(id.clone());
            let info = self.get(&alloc::format!("/channels/{id}"))?;
            if self.guild.is_none() {
                self.guild = info["guild_id"].as_str().map(String::from);
            }
            let forum = info["type"]
                .as_u64()
                .is_some_and(|t| FORUM_TYPES.contains(&t));
            let mut count = Count {
                name: String::from(info["name"].as_str().unwrap_or(&id)),
                id: id.clone(),
                forum,
                ..Count::default()
            };
            match self.guild.clone() {
                Some(g) => count.messages = self.search_count(&g, &id)?,
                None => (self.report)("no server known: Discord's search needs one (--guild)"),
            }
            if forum
                && let Some(posts) = self.request(
                    &alloc::format!(
                        "/channels/{id}/threads/search?archived=true&sort_by=last_message_time&sort_order=desc&limit=25&offset=0"
                    ),
                    true,
                )?
            {
                count.posts = posts["total_results"].as_u64();
            }
            counts.push(count);
        }
        Ok(())
    }

    /// Messages in a channel by the server's search; `None` when the
    /// search is not allowed or still not indexed after two more tries
    /// (Discord answers 202 with `retry_after` while it indexes)
    fn search_count(&mut self, guild: &str, id: &str) -> Result<Option<u64>> {
        let path = alloc::format!("/guilds/{guild}/messages/search?channel_id={id}");
        for attempt in 0..3 {
            let Some(body) = self.request(&path, true)? else {
                return Ok(None);
            };
            if self.status != 202
                && let Some(n) = body["total_results"].as_u64()
            {
                return Ok(Some(n));
            }
            if attempt == 2 {
                break;
            }
            let wait = body["retry_after"].as_f64().unwrap_or(2.0).clamp(1.0, 60.0);
            (self.report)(&alloc::format!(
                "Discord is still indexing the channel for search; asking again in {wait:.0} s"
            ));
            self.rate_wait = Duration::from_secs_f64(wait);
        }
        (self.report)("the channel is not indexed for search yet; try --count again later");
        Ok(None)
    }

    /// Every given channel, then its threads
    fn fetch_all(&mut self) -> Result<()> {
        for id in self.options.channels.clone() {
            self.viewing = Some(id.clone());
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
            self.channels_read.push((
                id.clone(),
                String::from(info["guild_id"].as_str().unwrap_or("guild")),
                folder.clone(),
            ));
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
                self.viewing = Some(String::from(tid));
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

    /// The attachments of every channel read this run: counted, and with
    /// `download` the wanted ones fetched into their media folder, newest
    /// message first
    fn media(&mut self, media: &mut Media, download: bool) -> Result<()> {
        let which = self.options.attachments;
        let cap = self.options.max_file_mb.map(|mb| mb * 1_000_000);
        let total_cap = self.options.max_total_gb.map(|gb| (gb * 1e9) as u64);
        for (id, guild, folder) in self.channels_read.clone() {
            let attachments = discord_media::in_channel_folder(&folder, &id)?;
            let listing = Listing::of(&attachments, which, cap);
            media.listing.extend(&listing);
            let name = self.cursor(&id).name.clone();
            (self.report)(&alloc::format!(
                "#{name}: {listing}{}",
                if listing.over_cap.files > 0 {
                    alloc::format!(
                        "; {} wanted files over {} MB",
                        listing.over_cap.files,
                        self.options.max_file_mb.unwrap_or_default()
                    )
                } else {
                    String::new()
                }
            ));
            if !download || !matches!(which, Attachments::Videos | Attachments::Media) {
                continue;
            }
            let dir = self.options.media.join(&guild).join(&id);
            let mut manifest = Manifest::load(&dir)?;
            let mut total = manifest.bytes();
            let mut pending: Vec<Attachment> = attachments
                .into_iter()
                .filter(|a| a.wanted(which))
                .collect();
            pending.sort_by_key(|a| core::cmp::Reverse(snowflake(&a.message_id)));
            let ids: BTreeSet<String> = pending.iter().map(|a| a.id.clone()).collect();
            // Links from pages read again this run, by attachment id
            let mut fresh: BTreeMap<String, String> = BTreeMap::new();
            let n = pending.len();
            for (i, a) in pending.iter().enumerate() {
                if manifest.has(a) {
                    media.present += 1;
                    continue;
                }
                if cap.is_some_and(|c| a.size > c) {
                    media.skipped_size.add(a.size);
                    continue;
                }
                if !discord_media::in_scope(&a.url) {
                    media.out_of_scope += 1;
                    (self.report)(&alloc::format!(
                        "{} of message {} is not on Discord's CDN; not downloaded",
                        a.filename,
                        a.message_id
                    ));
                    continue;
                }
                if total_cap.is_some_and(|c| total + a.size > c) {
                    media.skipped_total.add(a.size);
                    continue;
                }
                let mut url = fresh.remove(&a.id).unwrap_or_else(|| a.url.clone());
                let mut refreshed = false;
                if discord_media::expired(&url, self.clock.now()) {
                    refreshed = true;
                    media.refreshed += 1;
                    match self.refresh(a, &ids, &mut fresh)? {
                        Some(u) => url = u,
                        None => {
                            media.failed += 1;
                            (self.report)(&alloc::format!(
                                "{} is no longer on message {}; not downloaded",
                                a.filename,
                                a.message_id
                            ));
                            continue;
                        }
                    }
                }
                let mut restarted = false;
                loop {
                    match self.download(a, &url, &dir, &guild)? {
                        Outcome::Fetched { taken, len } => {
                            let path = dir.join(a.relative_path());
                            manifest.add(Entry {
                                message_id: a.message_id.clone(),
                                attachment_id: a.id.clone(),
                                filename: a.filename.clone(),
                                size: len,
                                content_type: a.content_type.clone(),
                                path: a.relative_path(),
                                sha256: discord_media::sha256_file(&path)?,
                            })?;
                            total += len;
                            media.downloaded.add(taken);
                            (self.report)(&alloc::format!(
                                "{} ({}) of message {}: {} of {} files of #{name}; delay {:.1} s",
                                a.safe_name(),
                                discord_media::size(len),
                                a.message_id,
                                i + 1,
                                n,
                                self.delay.as_secs_f64()
                            ));
                            break;
                        }
                        Outcome::Gone if !refreshed => {
                            refreshed = true;
                            media.refreshed += 1;
                            match self.refresh(a, &ids, &mut fresh)? {
                                Some(u) => url = u,
                                None => {
                                    media.failed += 1;
                                    (self.report)(&alloc::format!(
                                        "{} is no longer on message {}; not downloaded",
                                        a.filename,
                                        a.message_id
                                    ));
                                    break;
                                }
                            }
                        }
                        Outcome::Gone => {
                            media.failed += 1;
                            (self.report)(&alloc::format!(
                                "{} of message {}: gone even with a fresh link; skipped",
                                a.filename,
                                a.message_id
                            ));
                            break;
                        }
                        Outcome::Restart if !restarted => restarted = true,
                        Outcome::Restart => {
                            media.failed += 1;
                            (self.report)(&alloc::format!(
                                "{} of message {}: the server does not continue partial files; skipped",
                                a.filename,
                                a.message_id
                            ));
                            break;
                        }
                        Outcome::Failed(why) => {
                            media.failed += 1;
                            (self.report)(&alloc::format!(
                                "{} of message {}: {why}; skipped, the next run tries again",
                                a.filename,
                                a.message_id
                            ));
                            break;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Reads the page of messages ending at `a`'s message again, an
    /// ordinary paced GET, and keeps the links of every wanted attachment
    /// on it in `fresh`; returns `a`'s fresh link, none when the message or
    /// the file is no longer there
    fn refresh(
        &mut self,
        a: &Attachment,
        wanted: &BTreeSet<String>,
        fresh: &mut BTreeMap<String, String>,
    ) -> Result<Option<String>> {
        self.viewing = Some(a.conversation.clone());
        let before = snowflake(&a.message_id).saturating_add(1);
        let page = self.get(&alloc::format!(
            "/channels/{}/messages?limit={PAGE}&before={before}",
            a.conversation
        ))?;
        for m in page.as_array().into_iter().flatten() {
            for found in Attachment::from_message(m, &a.conversation) {
                if wanted.contains(&found.id) && discord_media::in_scope(&found.url) {
                    fresh.insert(found.id, found.url);
                }
            }
        }
        Ok(fresh.remove(&a.id))
    }

    /// The wait before a download: paced like a request, stopped like one
    /// (Ctrl+C, the run's minutes). Downloads are not requests to the API,
    /// so the request caps do not count them.
    fn wait(&mut self) -> Result<()> {
        if self.clock.stopped() {
            return Err(Stop::Interrupted.into());
        }
        if let Some(minutes) = self.options.pace.max_minutes
            && self.clock.elapsed().as_secs_f64() >= minutes * 60.0
        {
            return Err(Stop::MaxMinutes(minutes.round() as u32).into());
        }
        let wait = self.paced();
        self.delay = wait;
        if !wait.is_zero() && !self.clock.sleep(wait) {
            return Err(Stop::Interrupted.into());
        }
        self.since_pause += 1;
        Ok(())
    }

    /// Downloads `a` from `url` into `dir/<message id>/<file name>`,
    /// through `<file name>.part`, which a later run continues with a
    /// `Range` request. Ctrl+C stops the run and keeps the part. The file
    /// is whole when it has the length the answer announced
    /// (`Content-Length`, after the part a `Range` continued), else the
    /// attachment's size: the CDN serves an image in the format its file
    /// name says (a JPEG named `.png` as a PNG), longer or shorter than the
    /// attachment it was uploaded as.
    fn download(&mut self, a: &Attachment, url: &str, dir: &Path, guild: &str) -> Result<Outcome> {
        self.wait()?;
        let path = dir.join(a.relative_path());
        let part = dir.join(alloc::format!("{}.part", a.relative_path()));
        std::fs::create_dir_all(path.parent().context("no folder")?)?;
        let mut offset = std::fs::metadata(&part).map_or(0, |m| m.len());
        if offset > a.size {
            std::fs::remove_file(&part)?;
            offset = 0;
        }
        let referer = alloc::format!("https://discord.com/channels/{guild}/{}", a.conversation);
        let mut headers = self.options.browser.media_headers(&referer);
        if offset > 0 && offset < a.size {
            headers.push((String::from("Range"), alloc::format!("bytes={offset}-")));
        }
        let (status, announced) = if offset == a.size && a.size > 0 {
            // Left whole by an interrupted run, only the rename missing
            (206, None)
        } else {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part)
                .with_context(|| alloc::format!("opening {}", part.display()))?;
            let mut out = Stopping {
                file,
                clock: &*self.clock,
            };
            match self.http.download(url, &headers, &mut out) {
                Ok(resp) => (
                    resp.status,
                    resp.header("content-length")
                        .and_then(|n| n.trim().parse::<u64>().ok()),
                ),
                Err(_) if self.clock.stopped() => return Err(Stop::Interrupted.into()),
                Err(e) => return Ok(Outcome::Failed(alloc::format!("{e:#}"))),
            }
        };
        match status {
            200 if offset > 0 => {
                std::fs::remove_file(&part)?;
                Ok(Outcome::Restart)
            }
            200 | 206 => {
                let len = std::fs::metadata(&part).map_or(0, |m| m.len());
                let whole = announced.map_or(a.size, |n| offset + n);
                if len != whole {
                    std::fs::remove_file(&part)?;
                    return Ok(Outcome::Failed(alloc::format!(
                        "got {len} bytes, the file has {whole}"
                    )));
                }
                std::fs::rename(&part, &path)
                    .with_context(|| alloc::format!("renaming {}", part.display()))?;
                Ok(Outcome::Fetched {
                    taken: len - offset,
                    len,
                })
            }
            403 | 404 | 410 => Ok(Outcome::Gone),
            s => Ok(Outcome::Failed(alloc::format!("HTTP {s}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use serde_json::json;

    /// Answers scripted requests in order, checking each path; keeps the
    /// headers sent. Downloads are scripted apart (`downloads`: the link,
    /// the answer and the bytes of its body), with their headers kept too
    struct Fake {
        expected: VecDeque<(String, Response)>,
        log: Vec<String>,
        headers: Vec<Vec<(String, String)>>,
        downloads: VecDeque<(String, Response, Vec<u8>)>,
        download_headers: Vec<Vec<(String, String)>>,
    }

    impl Fake {
        fn new(script: Vec<(&str, Response)>) -> Self {
            Fake {
                expected: script
                    .into_iter()
                    .map(|(p, r)| (String::from(p), r))
                    .collect(),
                log: Vec::new(),
                headers: Vec::new(),
                downloads: VecDeque::new(),
                download_headers: Vec::new(),
            }
        }

        fn expect_download(&mut self, url: &str, resp: Response, body: &[u8]) {
            self.downloads
                .push_back((String::from(url), resp, body.to_vec()));
        }

        /// A header of the n-th request
        fn header(&self, n: usize, name: &str) -> Option<&str> {
            self.headers[n]
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }
    }

    impl Http for Fake {
        fn get(&mut self, url: &str, headers: &[(String, String)]) -> Result<Response> {
            let token = headers.iter().find(|(k, _)| k == "Authorization");
            assert_eq!(token.map(|(_, v)| v.as_str()), Some("sekrit-9f8e7d"));
            let path = url.strip_prefix(API).expect("the API base");
            let (want, resp) = self
                .expected
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected request {path}"));
            assert_eq!(path, want);
            self.log.push(String::from(path));
            self.headers.push(headers.to_vec());
            Ok(resp)
        }

        fn download(
            &mut self,
            url: &str,
            headers: &[(String, String)],
            out: &mut dyn Write,
        ) -> Result<Response> {
            let (want, resp, body) = self
                .downloads
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected download {url}"));
            assert_eq!(url, want);
            self.download_headers.push(headers.to_vec());
            if matches!(resp.status, 200 | 206) {
                out.write_all(&body)?;
            }
            Ok(resp)
        }
    }

    /// Records sleeps instead of sleeping; can stop at the n-th
    #[derive(Default)]
    struct FakeClock {
        slept: Vec<Duration>,
        stop_at: Option<usize>,
        day: String,
        /// The time, Unix seconds
        now: i64,
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
        fn now(&self) -> i64 {
            self.now
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

    /// One channel into a flat folder, without the longer pauses, its
    /// attachments only counted
    fn options(threads: bool) -> Options {
        Options {
            channels: alloc::vec![String::from("2")],
            guild: None,
            threads,
            flat: true,
            count: false,
            pace: Pace {
                pause_every: Range::new(1e9, 1e9),
                ..Pace::default()
            },
            browser: Browser::default(),
            attachments: Attachments::List,
            max_file_mb: None,
            max_total_gb: None,
            media: std::env::temp_dir().join("cuttlefish-fetch-unused-media"),
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
        // The search only within the given channels, on their server
        let search = |q: &str| alloc::format!("/guilds/1/messages/search{q}");
        assert!(in_scope(&search("?channel_id=2"), &allowed, Some("1")));
        assert!(!in_scope(&search("?channel_id=2"), &allowed, Some("7")));
        assert!(!in_scope(&search("?channel_id=9"), &allowed, Some("1")));
        assert!(!in_scope(
            &search("?channel_id=2&channel_id=9"),
            &allowed,
            Some("1")
        ));
        assert!(!in_scope(&search("?content=eggs"), &allowed, Some("1")));
        assert!(!in_scope(&search(""), &allowed, Some("1")));

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

    #[test]
    fn channels_as_ids_pairs_and_links() {
        let parse = |s: &str| s.parse::<ChannelRef>();
        let both = |g: &str, c: &str| ChannelRef {
            guild: Some(String::from(g)),
            channel: String::from(c),
        };
        let (g, c) = ("737359708276654121", "737962428553232465");
        assert_eq!(
            parse(c),
            Ok(ChannelRef {
                guild: None,
                channel: String::from(c)
            })
        );
        assert_eq!(parse(&alloc::format!(" {g}/{c} ")), Ok(both(g, c)));
        for link in [
            alloc::format!("https://discord.com/channels/{g}/{c}"),
            alloc::format!("https://discord.com/channels/{g}/{c}/"),
            alloc::format!("https://discord.com/channels/{g}/{c}/1300000000000000000"),
            alloc::format!("https://ptb.discord.com/channels/{g}/{c}"),
            alloc::format!("https://canary.discord.com/channels/{g}/{c}?x=1"),
            alloc::format!("https://discordapp.com/channels/{g}/{c}"),
            alloc::format!("http://www.Discord.com/channels/{g}/{c}#top"),
        ] {
            assert_eq!(parse(&link), Ok(both(g, c)), "{link}");
        }
        for bad in [
            "",
            "vod-review",
            "https://example.com/channels/1/2",
            "https://discord.com/channels/@me/2",
            "https://discord.com/invite/abc",
            "https://discord.com/channels/1",
            "1/2/3",
            "1/x",
            "123456789012345678901",
        ] {
            let err = parse(bad).unwrap_err();
            assert!(err.contains("https://discord.com/channels/<server id>/<channel id>"));
            assert!(err.contains("<server id>/<channel id>"), "{err}");
        }
        assert!(
            parse("https://discord.com/channels/@me/2")
                .unwrap_err()
                .contains("direct message")
        );

        let refs = [both("1", "2"), both("1", "3")];
        assert_eq!(guild_of(None, &refs).unwrap().as_deref(), Some("1"));
        assert_eq!(
            guild_of(Some(String::from("1")), &refs).unwrap().as_deref(),
            Some("1")
        );
        assert!(guild_of(Some(String::from("5")), &refs).is_err());
        assert!(guild_of(None, &[both("1", "2"), both("4", "3")]).is_err());
        assert_eq!(guild_of(None, &[parse("2").unwrap()]).unwrap(), None);
    }

    #[test]
    fn counts_through_the_search_waiting_for_the_index() {
        let out = temp("count");
        let indexing = Response {
            status: 202,
            headers: Vec::new(),
            body: String::from(
                r#"{"message": "Index not yet available. Try again later", "code": 110000, "retry_after": 2}"#,
            ),
        };
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 300))),
            ("/guilds/1/messages/search?channel_id=2", indexing),
            (
                "/guilds/1/messages/search?channel_id=2",
                ok(json!({"total_results": 12345, "messages": []}))
            ),
        ]);
        let mut opts = options(true);
        opts.count = true;
        opts.pace.delay = Range::new(1.0, 1.0);
        let mut clock = FakeClock::default();
        let (summary, lines) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        assert!(fake.expected.is_empty());
        assert_eq!(summary.messages, 0);
        let count = &summary.counts[0];
        assert_eq!(count.name, "c2");
        assert_eq!(count.messages, Some(12345));
        assert_eq!(count.posts, None);
        assert_eq!(count.requests(), Some(125));
        // The wait asked for (2 s, over the 1 s pace)
        let secs: Vec<f64> = clock.slept.iter().map(Duration::as_secs_f64).collect();
        assert_eq!(secs, [1.0, 2.0]);
        assert!(lines.iter().any(|l| l.contains("indexing")));
        // No messages read, nothing but the state written
        assert!(!out.join("2.messages.jsonl").exists());
        assert_eq!(State::load(&out).unwrap().requests_today, 3);

        // A forum: its posts from its listing; the index never ready
        let not_yet = || Response {
            status: 202,
            headers: Vec::new(),
            body: String::from(r#"{"retry_after": 0.5}"#),
        };
        let search = "/guilds/1/messages/search?channel_id=2";
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 15, 0))),
            (search, not_yet()),
            (search, not_yet()),
            (search, not_yet()),
            (
                "/channels/2/threads/search?archived=true&sort_by=last_message_time&sort_order=desc&limit=25&offset=0",
                ok(json!({"threads": [], "total_results": 300}))
            ),
        ]);
        let (summary, lines) = go(&mut fake, &mut FakeClock::default(), &out, opts).unwrap();
        assert!(fake.expected.is_empty());
        let count = &summary.counts[0];
        assert!(count.forum);
        assert_eq!((count.messages, count.posts), (None, Some(300)));
        assert!(lines.iter().any(|l| l.contains("not indexed")));
        // The server's search is only asked with the server known
        assert_eq!(
            fake.header(1, "Referer"),
            Some("https://discord.com/channels/1/2")
        );
        std::fs::remove_dir_all(&out).unwrap();

        // 20,000 messages at 3-8 s, a 60-300 s pause every 40-120: 201
        // requests at 5.5 s and 2 pauses of 180 s
        let pace = Pace::default();
        let n = Count {
            messages: Some(20_000),
            ..Count::default()
        };
        assert_eq!(n.requests(), Some(201));
        let secs = estimate(201, &pace).as_secs_f64();
        assert!((secs - (201.0 * 5.5 + 2.0 * 180.0)).abs() < 1e-6, "{secs}");
    }

    #[test]
    fn browser_headers() {
        let vars = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| (String::from(*k), String::from(*v)))
                .collect();
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.clone())
            }
        };
        let get = |h: &[(String, String)], name: &str| {
            h.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        };
        // Nothing given: the built-in agent, the system's locale and zone,
        // no super properties
        let b = Browser::from_vars(
            vars(&[]),
            Some(String::from("de-DE")),
            Some(String::from("Europe/Berlin")),
        );
        let h = b.headers("tok", Some("https://discord.com/channels/1/2"));
        assert_eq!(get(&h, "User-Agent").as_deref(), Some(DEFAULT_USER_AGENT));
        assert_eq!(get(&h, "Authorization").as_deref(), Some("tok"));
        assert_eq!(get(&h, "X-Super-Properties"), None);
        assert_eq!(get(&h, "X-Discord-Locale").as_deref(), Some("de-DE"));
        assert_eq!(
            get(&h, "Accept-Language").as_deref(),
            Some("de-DE,de;q=0.5")
        );
        assert_eq!(
            get(&h, "X-Discord-Timezone").as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(
            get(&h, "Referer").as_deref(),
            Some("https://discord.com/channels/1/2")
        );
        let text = b.describe();
        assert!(text.contains("User-Agent from built in"), "{text}");
        assert!(text.contains("X-Super-Properties not sent"), "{text}");
        assert!(
            !text.contains("Europe/Berlin") && !text.contains("de-DE"),
            "{text}"
        );
        // Neither locale nor zone known
        let h = Browser::from_vars(vars(&[]), None, None).headers("tok", None);
        assert_eq!(get(&h, "X-Discord-Locale").as_deref(), Some("en-US"));
        assert_eq!(get(&h, "X-Discord-Timezone"), None);
        assert_eq!(get(&h, "Referer"), None);

        // All given: sent exactly as given, over the system's
        let b = Browser::from_vars(
            vars(&[
                (
                    USER_AGENT_VAR,
                    " Mozilla/5.0 (Windows NT 10.0) Chrome/150.0 ",
                ),
                (SUPER_PROPERTIES_VAR, "eyJvcyI6IldpbmRvd3MifQ=="),
                (LOCALE_VAR, "ja"),
                (TIMEZONE_VAR, "Asia/Tokyo"),
            ]),
            Some(String::from("de-DE")),
            Some(String::from("Europe/Berlin")),
        );
        let h = b.headers("tok", None);
        assert_eq!(
            get(&h, "User-Agent").as_deref(),
            Some("Mozilla/5.0 (Windows NT 10.0) Chrome/150.0")
        );
        assert_eq!(
            get(&h, "X-Super-Properties").as_deref(),
            Some("eyJvcyI6IldpbmRvd3MifQ==")
        );
        assert_eq!(get(&h, "X-Discord-Locale").as_deref(), Some("ja"));
        assert_eq!(get(&h, "Accept-Language").as_deref(), Some("ja"));
        assert_eq!(get(&h, "X-Discord-Timezone").as_deref(), Some("Asia/Tokyo"));
        let text = b.describe();
        assert!(text.contains(&alloc::format!("User-Agent from {USER_AGENT_VAR}")));
        assert!(text.contains(&alloc::format!("locale from {LOCALE_VAR}")));
        assert!(!text.contains("Chrome") && !text.contains("eyJ"), "{text}");
        // Only the super properties: the agent they name, so both agree
        use base64::Engine;
        let props = base64::engine::general_purpose::STANDARD
            .encode(br#"{"os": "Linux", "browser_user_agent": "Mozilla/5.0 Firefox/154.0"}"#);
        let b = Browser::from_vars(vars(&[(SUPER_PROPERTIES_VAR, &props)]), None, None);
        assert_eq!(b.user_agent.0, "Mozilla/5.0 Firefox/154.0");
        assert_eq!(b.user_agent.1, HeaderSource::SuperProperties);

        assert_eq!(discord_locale("en_US.UTF-8").as_deref(), Some("en-US"));
        assert_eq!(discord_locale("ja_JP.utf8@x").as_deref(), Some("ja-JP"));
        assert_eq!(discord_locale("C.UTF-8"), None);
        assert_eq!(discord_locale("POSIX"), None);
    }

    /// A message with attachments: `files` are (attachment id, name, size,
    /// expiry as Unix seconds or none)
    fn with_files(id: u64, files: &[(&str, &str, u64, Option<i64>)]) -> Value {
        let mut m = message(id);
        m["attachments"] = files
            .iter()
            .map(|(aid, name, size, ex)| {
                let url = match ex {
                    Some(ex) => alloc::format!(
                        "https://cdn.discordapp.com/attachments/2/{aid}/{name}?ex={ex:x}&is=1&hm=abc"
                    ),
                    None => alloc::format!("https://cdn.discordapp.com/attachments/2/{aid}/{name}"),
                };
                let content_type = match name.rsplit_once('.').map(|(_, e)| e) {
                    Some("png") => "image/png",
                    Some("mp4") => "video/mp4",
                    _ => "text/plain",
                };
                json!({"id": aid, "filename": name, "size": size, "url": url,
                       "content_type": content_type})
            })
            .collect();
        m
    }

    #[test]
    fn downloads_videos_with_fresh_links_and_resume() {
        let out = temp("media");
        let media = out.join("media");
        let now: i64 = 1_700_000_000;
        let (fresh, stale) = (now + 86_400, now - 10);
        let cdn = |aid: &str, name: &str, ex: i64| {
            alloc::format!(
                "https://cdn.discordapp.com/attachments/2/{aid}/{name}?ex={ex:x}&is=1&hm=abc"
            )
        };
        let mut opts = options(false);
        opts.attachments = Attachments::Videos;
        opts.max_file_mb = Some(500);
        opts.media = media.clone();
        let mut clock = FakeClock {
            now,
            ..FakeClock::default()
        };

        // Newest first: 103's link is fresh; 102 has an image and a text
        // file (not wanted); 101's link expired, so its page is read again;
        // 100's file is over the cap; 99's link is not Discord's CDN
        let mut foreign = message(99);
        foreign["attachments"] = json!([{"id": "69", "filename": "x.mp4", "size": 3, "url": "https://evil.example/attachments/2/69/x.mp4"}]);
        let page = Value::Array(alloc::vec![
            with_files(103, &[("73", "run3.mp4", 5, Some(fresh))]),
            with_files(
                102,
                &[
                    ("72", "shot.png", 2, Some(fresh)),
                    ("74", "notes.txt", 1, None)
                ]
            ),
            with_files(101, &[("71", "run1.mp4", 6, Some(stale))]),
            with_files(100, &[("70", "big.mp4", 900_000_000, Some(fresh))]),
            foreign,
        ]);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 103))),
            ("/channels/2/messages?limit=100", ok(page)),
            (
                "/channels/2/messages?limit=100&before=102",
                ok(Value::Array(alloc::vec![with_files(
                    101,
                    &[("71", "run1.mp4", 6, Some(fresh))]
                )]))
            ),
        ]);
        fake.expect_download(&cdn("73", "run3.mp4", fresh), ok(json!(null)), b"hello");
        fake.expect_download(&cdn("71", "run1.mp4", fresh), ok(json!(null)), b"hello!");
        let (summary, lines) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        assert!(fake.expected.is_empty() && fake.downloads.is_empty());
        assert_eq!(summary.stopped, None);
        let m = summary.media.expect("media done");
        assert_eq!(
            m.listing.videos,
            Tally {
                files: 4,
                bytes: 900_000_014
            }
        );
        assert_eq!(m.listing.images, Tally { files: 1, bytes: 2 });
        assert_eq!(m.listing.other, Tally { files: 1, bytes: 1 });
        assert_eq!(
            m.downloaded,
            Tally {
                files: 2,
                bytes: 11
            }
        );
        assert_eq!(
            m.skipped_size,
            Tally {
                files: 1,
                bytes: 900_000_000
            }
        );
        assert_eq!(
            (m.present, m.refreshed, m.failed, m.out_of_scope),
            (0, 1, 0, 1)
        );
        let dir = media.join("1/2");
        assert_eq!(std::fs::read(dir.join("103/run3.mp4")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dir.join("101/run1.mp4")).unwrap(), b"hello!");
        let manifest = Manifest::load(&dir).unwrap();
        assert_eq!(manifest.len(), 2);
        let e = manifest.get("71").unwrap();
        assert_eq!(
            (e.message_id.as_str(), e.path.as_str(), e.size),
            ("101", "101/run1.mp4", 6)
        );
        assert_eq!(e.content_type.as_deref(), Some("video/mp4"));
        assert_eq!(
            e.sha256,
            discord_media::sha256_file(&dir.join("101/run1.mp4")).unwrap()
        );
        // The channel's page as referer, the browser's agent, never the
        // token; paced like the requests (two pages, one refresh, two
        // downloads: four waits)
        for h in &fake.download_headers {
            let get = |n: &str| h.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
            assert_eq!(get("Referer"), Some("https://discord.com/channels/1/2"));
            assert_eq!(get("User-Agent"), Some(DEFAULT_USER_AGENT));
            assert_eq!(get("Authorization"), None);
            assert_eq!(get("Range"), None);
        }
        assert_eq!(clock.slept.len(), 4);
        assert!(
            clock
                .slept
                .iter()
                .all(|d| (3.0..=8.0).contains(&d.as_secs_f64()))
        );
        assert!(
            lines.iter().any(|l| l.contains("4 videos (900 MB)")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("1 wanted files over 500 MB"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("run3.mp4 (0 MB) of message 103: 1 of 4 files"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("x.mp4 of message 99 is not on Discord's CDN"))
        );

        // Next run: a new message whose file is half there; its first
        // download answers 404 (the link died), the page is read again and
        // the rest of the file is asked for with a Range
        std::fs::create_dir_all(dir.join("104")).unwrap();
        std::fs::write(dir.join("104/run4.mp4.part"), b"abc").unwrap();
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 104))),
            (
                "/channels/2/messages?limit=100&after=103",
                ok(Value::Array(alloc::vec![with_files(
                    104,
                    &[("76", "run4.mp4", 8, Some(fresh))]
                )]))
            ),
            (
                "/channels/2/messages?limit=100&before=105",
                ok(Value::Array(alloc::vec![with_files(
                    104,
                    &[("76", "run4.mp4", 8, Some(fresh + 1))]
                )]))
            ),
        ]);
        fake.expect_download(&cdn("76", "run4.mp4", fresh), status(404, ""), b"");
        let partial = Response {
            status: 206,
            headers: Vec::new(),
            body: String::new(),
        };
        fake.expect_download(&cdn("76", "run4.mp4", fresh + 1), partial, b"defgh");
        let (summary, _) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        assert!(fake.expected.is_empty() && fake.downloads.is_empty());
        let m = summary.media.unwrap();
        assert_eq!(m.downloaded, Tally { files: 1, bytes: 5 });
        assert_eq!((m.present, m.refreshed, m.failed), (2, 1, 0));
        assert_eq!(
            std::fs::read(dir.join("104/run4.mp4")).unwrap(),
            b"abcdefgh"
        );
        assert!(!dir.join("104/run4.mp4.part").exists());
        for h in &fake.download_headers {
            let range = h
                .iter()
                .find(|(k, _)| k == "Range")
                .map(|(_, v)| v.as_str());
            assert_eq!(range, Some("bytes=3-"));
        }
        assert_eq!(Manifest::load(&dir).unwrap().len(), 3);

        // A total cap the folder (19 bytes) is already past: nothing more
        // is downloaded, the file is counted as skipped
        opts.max_total_gb = Some(10e-9);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 105))),
            (
                "/channels/2/messages?limit=100&after=104",
                ok(Value::Array(alloc::vec![with_files(
                    105,
                    &[("77", "run5.mp4", 4, Some(fresh))]
                )]))
            ),
        ]);
        let (summary, _) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        let m = summary.media.unwrap();
        assert_eq!(m.skipped_total, Tally { files: 1, bytes: 4 });
        assert_eq!(m.downloaded, Tally::default());
        assert_eq!(m.present, 3);

        // The default, list: counted, nothing downloaded; and none: not
        // even counted
        opts.attachments = Attachments::List;
        let mut fake = Fake::new(alloc::vec![("/channels/2", ok(channel("2", 0, 105)))]);
        let (summary, _) = go(&mut fake, &mut clock, &out, opts.clone()).unwrap();
        let m = summary.media.unwrap();
        assert_eq!(m.listing.videos.files, 6);
        assert_eq!(m.downloaded, Tally::default());
        // Every wait so far: 4, then a page, a failed download, the refresh
        // and the download, then one page; the listing waits for nothing
        assert_eq!(clock.slept.len(), 4 + 4 + 1);
        opts.attachments = Attachments::None;
        let mut fake = Fake::new(alloc::vec![("/channels/2", ok(channel("2", 0, 105)))]);
        let (summary, _) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert_eq!(summary.media, None);
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn keeps_images_the_cdn_converted() {
        let out = temp("converted");
        let media = out.join("media");
        let now: i64 = 1_700_000_000;
        let cdn = |aid: &str, name: &str| {
            alloc::format!(
                "https://cdn.discordapp.com/attachments/2/{aid}/{name}?ex={:x}&is=1&hm=abc",
                now + 86_400
            )
        };
        let mut opts = options(false);
        opts.attachments = Attachments::Media;
        opts.media = media.clone();
        let mut clock = FakeClock {
            now,
            ..FakeClock::default()
        };
        // Uploaded as JPEGs of 4 bytes, served as PNGs of 6: whole when
        // the answer announced 6, cut off when it announced 9
        let page = Value::Array(alloc::vec![
            with_files(11, &[("81", "a.png", 4, Some(now + 86_400))]),
            with_files(10, &[("80", "b.png", 4, Some(now + 86_400))]),
        ]);
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 11))),
            ("/channels/2/messages?limit=100", ok(page)),
        ]);
        let announced = |n: &str| Response {
            status: 200,
            headers: alloc::vec![(String::from("Content-Length"), String::from(n))],
            body: String::new(),
        };
        fake.expect_download(&cdn("81", "a.png"), announced("6"), b"\x89PNG!!");
        fake.expect_download(&cdn("80", "b.png"), announced("9"), b"\x89PNG!!");
        let (summary, lines) = go(&mut fake, &mut clock, &out, opts).unwrap();
        assert!(fake.downloads.is_empty());
        let m = summary.media.unwrap();
        assert_eq!(m.downloaded, Tally { files: 1, bytes: 6 });
        assert_eq!(m.failed, 1);
        let dir = media.join("1/2");
        assert_eq!(std::fs::read(dir.join("11/a.png")).unwrap(), b"\x89PNG!!");
        assert!(!dir.join("10/b.png").exists() && !dir.join("10/b.png.part").exists());
        // The manifest holds the file as it is, so the next run skips it
        let manifest = Manifest::load(&dir).unwrap();
        assert_eq!(manifest.get("81").unwrap().size, 6);
        assert!(manifest.has(&Attachment {
            message_id: String::from("11"),
            conversation: String::from("2"),
            id: String::from("81"),
            filename: String::from("a.png"),
            size: 4,
            content_type: None,
            url: cdn("81", "a.png"),
        }));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("b.png of message 10: got 6 bytes, the file has 9")),
            "{lines:?}"
        );
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn requests_carry_the_page_as_referer() {
        let out = temp("referer");
        let mut opts = options(false);
        opts.guild = Some(String::from("1"));
        let mut fake = Fake::new(alloc::vec![
            ("/channels/2", ok(channel("2", 0, 3))),
            ("/channels/2/messages?limit=100", ok(messages(1..=3))),
        ]);
        go(&mut fake, &mut FakeClock::default(), &out, opts).unwrap();
        for n in 0..2 {
            assert_eq!(
                fake.header(n, "Referer"),
                Some("https://discord.com/channels/1/2")
            );
            assert_eq!(fake.header(n, "User-Agent"), Some(DEFAULT_USER_AGENT));
        }
        std::fs::remove_dir_all(&out).unwrap();
    }
}
