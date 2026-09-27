//! The corpus's YouTube VODs, downloaded at 480p with yt-dlp into
//! `<knowledge>/media/youtube/<id>.mp4`, each with `<id>.info.json`
//! ([`Info`]: title, channel, duration, upload date, size, or why it
//! cannot be downloaded).
//!
//! Politely: one video at a time, a random pause between videos
//! ([`DEFAULT_DELAY`]). Resumable: a video whose file is there is skipped,
//! a partial download continues (yt-dlp's `.part`), a deleted or private
//! video is remembered as unavailable and not asked for again unless told
//! to. [`list`] asks yt-dlp for the metadata only (cached in the same
//! `info.json`) and adds up the size a run would download.

use crate::corpus::{Corpus, VideoKind, YOUTUBE_DIR};
use crate::discord_fetch::Range;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// yt-dlp's format: the best video up to 480 lines with the best audio,
/// else the best single file up to 480 lines
pub const FORMAT: &str = "bv*[height<=480]+ba/b[height<=480]";
/// Seconds between two downloads
pub const DEFAULT_DELAY: Range = Range::new(5.0, 5.0);
/// Seconds between two metadata requests of a listing
const LIST_DELAY: Range = Range::new(1.0, 3.0);
/// Transient failures in a row that end a run (a block, the network)
const MAX_FAILURES: usize = 3;
/// How often a download's progress is reported
const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// What is known about a video: `<id>.info.json`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    /// `YYYY-MM-DD`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_date: Option<String>,
    /// The chosen format's size as yt-dlp estimates it, or the file's once
    /// downloaded
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Why it cannot be downloaded (deleted, private, members only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloaded_at: Option<DateTime<Utc>>,
}

impl Info {
    /// The info file of a video
    pub fn path(dir: &Path, id: &str) -> PathBuf {
        dir.join(alloc::format!("{id}.info.json"))
    }

    /// The info written for a video, if any
    pub fn load(dir: &Path, id: &str) -> Option<Info> {
        let bytes = std::fs::read(Self::path(dir, id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes the info file
    pub fn save(&self, dir: &Path) -> Result<()> {
        crate::store::write_atomic(
            &Self::path(dir, &self.id),
            &serde_json::to_vec_pretty(self)?,
        )
    }

    /// The parts of yt-dlp's info dict that are kept
    pub fn from_ytdlp(id: &str, v: &Value) -> Info {
        let text = |key: &str| v[key].as_str().map(String::from);
        let upload_date = text("upload_date").map(|d| {
            if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) {
                alloc::format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..])
            } else {
                d
            }
        });
        Info {
            id: String::from(id),
            title: text("title"),
            channel: text("channel").or_else(|| text("uploader")),
            duration_s: v["duration"].as_f64(),
            upload_date,
            bytes: estimated_bytes(v),
            width: v["width"].as_u64().map(|w| w as u32),
            height: v["height"].as_u64().map(|h| h as u32),
            unavailable: None,
            downloaded_at: None,
        }
    }

    /// The video is downloaded
    pub fn downloaded(&self) -> bool {
        self.downloaded_at.is_some()
    }
}

/// Bytes the chosen format takes, as yt-dlp estimates: the info's own
/// size, else the sum of the formats it would merge
fn estimated_bytes(v: &Value) -> Option<u64> {
    let size = |f: &Value| f["filesize"].as_u64().or(f["filesize_approx"].as_u64());
    if let Some(n) = size(v) {
        return Some(n);
    }
    let formats = v["requested_formats"].as_array()?;
    let mut total = 0;
    for f in formats {
        total += size(f)?;
    }
    Some(total)
}

/// The video file of an id
pub fn video_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(alloc::format!("{id}.mp4"))
}

/// The knowledge folder's YouTube folder
pub fn dir(knowledge: &Path) -> PathBuf {
    knowledge.join(YOUTUBE_DIR)
}

/// The YouTube ids of a corpus, each once, in the order they were posted
pub fn ids(corpus: &Corpus) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for vod in &corpus.vods {
        for v in core::iter::once(&vod.video).chain(&vod.other_videos) {
            if v.kind == VideoKind::Youtube
                && let Some(id) = &v.id
                && !out.contains(id)
            {
                out.push(id.clone());
            }
        }
    }
    out
}

/// Why yt-dlp could not get a video, when the video itself is the reason
/// (gone, private, members only, blocked here); none for what may pass
/// later (the network, a bot check, a rate limit)
pub fn unavailable_reason(stderr: &str) -> Option<String> {
    const PERMANENT: [&str; 10] = [
        "Video unavailable",
        "video is unavailable",
        "Private video",
        "This video is private",
        "has been removed",
        "This video is not available",
        "members-only",
        "has been terminated",
        "This video is no longer available",
        "not available in your country",
    ];
    stderr
        .lines()
        .filter(|l| l.starts_with("ERROR"))
        .find(|l| PERMANENT.iter().any(|p| l.contains(p)))
        .map(|l| String::from(l.trim_start_matches("ERROR:").trim()))
}

/// Runs yt-dlp; stdout on success, else the error with yt-dlp's last
/// stderr line
fn yt_dlp(args: &[&str]) -> Result<(bool, String, String)> {
    let out = Command::new("yt-dlp")
        .args(args)
        .output()
        .context("running yt-dlp (is it installed?)")?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Asks yt-dlp about a video, without downloading: its info, or why it is
/// unavailable; an error for what may pass later
pub fn probe(id: &str) -> Result<Info> {
    let url = alloc::format!("https://www.youtube.com/watch?v={id}");
    let (ok, stdout, stderr) = yt_dlp(&["-J", "-f", FORMAT, "--no-playlist", &url])?;
    if ok {
        let v: Value = serde_json::from_str(&stdout).context("yt-dlp's JSON")?;
        return Ok(Info::from_ytdlp(id, &v));
    }
    match unavailable_reason(&stderr) {
        Some(reason) => Ok(Info {
            id: String::from(id),
            unavailable: Some(reason),
            ..Info::default()
        }),
        None => bail!(
            "yt-dlp: {}",
            stderr.lines().last().unwrap_or("failed").trim()
        ),
    }
}

/// Downloads a video into `dir` as `<id>.mp4`, continuing a partial
/// download; `report` hears the progress now and then. The info written
/// afterwards has the file's size and the download time; an unavailable
/// video's info says why.
pub fn download(id: &str, dir: &Path, report: &mut dyn FnMut(&str)) -> Result<Info> {
    std::fs::create_dir_all(dir)?;
    let url = alloc::format!("https://www.youtube.com/watch?v={id}");
    let template = dir.join("%(id)s.%(ext)s");
    let mut child = Command::new("yt-dlp")
        .args([
            "-f",
            FORMAT,
            "--merge-output-format",
            "mp4",
            "--remux-video",
            "mp4",
            "--no-playlist",
            "--continue",
            "--newline",
            "--progress",
            "--print",
            "after_move:%()j",
            "-o",
        ])
        .arg(&template)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running yt-dlp (is it installed?)")?;
    let mut stdout = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let stderr = child.stderr.take().context("no stderr")?;
    let mut errors = String::new();
    let mut last = std::time::Instant::now() - PROGRESS_EVERY;
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.starts_with("[download]") && line.contains('%') {
            if last.elapsed() >= PROGRESS_EVERY {
                report(&alloc::format!("  {id}: {line}"));
                last = std::time::Instant::now();
            }
        } else if line.starts_with("ERROR") || line.starts_with("WARNING") {
            report(&alloc::format!("  {id}: {line}"));
            errors.push_str(line);
            errors.push('\n');
        }
    }
    let status = child.wait()?;
    let printed = reader.join().unwrap_or_default();
    if !status.success() {
        return match unavailable_reason(&errors) {
            Some(reason) => Ok(Info {
                id: String::from(id),
                unavailable: Some(reason),
                ..Info::default()
            }),
            None => bail!(
                "yt-dlp: {}",
                errors.lines().last().unwrap_or("failed").trim()
            ),
        };
    }
    let info_json = printed
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .context("yt-dlp printed no info")?;
    let v: Value = serde_json::from_str(info_json).context("yt-dlp's JSON")?;
    let mut info = Info::from_ytdlp(id, &v);
    let file = video_path(dir, id);
    let meta = std::fs::metadata(&file)
        .with_context(|| alloc::format!("{} is not there after the download", file.display()))?;
    info.bytes = Some(meta.len());
    info.downloaded_at = Some(Utc::now());
    Ok(info)
}

/// Waits, in steps, unless told to stop; true when stopped
fn wait(seconds: f64, stop: &AtomicBool) -> bool {
    let end = std::time::Instant::now() + Duration::from_secs_f64(seconds.max(0.0));
    while std::time::Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    stop.load(Ordering::Relaxed)
}

/// A number drawn evenly from the range (xorshift64)
fn draw(rng: &mut u64, range: Range) -> f64 {
    *rng ^= *rng << 13;
    *rng ^= *rng >> 7;
    *rng ^= *rng << 17;
    let unit = (*rng >> 11) as f64 / (1u64 << 53) as f64;
    range.min + (range.max - range.min) * unit
}

fn seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0x9e37_79b9_7f4a_7c15, |d| d.as_nanos() as u64)
        | 1
}

/// Gigabytes or megabytes, as [`crate::discord_media::size`]
fn size(bytes: u64) -> String {
    crate::discord_media::size(bytes)
}

/// What a listing found: every video with what is known about it
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listing {
    pub infos: Vec<Info>,
    /// Ids yt-dlp could not be asked about this time (the network)
    pub failed: Vec<String>,
    pub stopped: bool,
}

impl Listing {
    /// Videos to download: known, available, not downloaded
    pub fn todo(&self) -> impl Iterator<Item = &Info> {
        self.infos
            .iter()
            .filter(|i| i.unavailable.is_none() && !i.downloaded())
    }

    /// Bytes a run would download, as far as known
    pub fn todo_bytes(&self) -> u64 {
        self.todo().filter_map(|i| i.bytes).sum()
    }

    pub fn downloaded(&self) -> impl Iterator<Item = &Info> {
        self.infos.iter().filter(|i| i.downloaded())
    }

    pub fn unavailable(&self) -> impl Iterator<Item = &Info> {
        self.infos.iter().filter(|i| i.unavailable.is_some())
    }

    /// The summary lines
    pub fn summary(&self) -> String {
        let todo: Vec<&Info> = self.todo().collect();
        let unknown = todo.iter().filter(|i| i.bytes.is_none()).count();
        alloc::format!(
            "{} videos: {} downloaded ({}), {} to download ({}{}), {} unavailable{}",
            self.infos.len(),
            self.downloaded().count(),
            size(self.downloaded().filter_map(|i| i.bytes).sum()),
            todo.len(),
            size(self.todo_bytes()),
            if unknown > 0 {
                alloc::format!(" plus {unknown} of unknown size")
            } else {
                String::new()
            },
            self.unavailable().count(),
            if self.failed.is_empty() {
                String::new()
            } else {
                alloc::format!(", {} not answered (see above)", self.failed.len())
            }
        )
    }
}

/// Lists the corpus's videos with yt-dlp's metadata, asking only about
/// those without an info file (all of them with `refresh`) and writing
/// what it learns; the size of a run comes from that. Prints a line per
/// video through `report`.
pub fn list(
    corpus: &Corpus,
    dir: &Path,
    refresh: bool,
    stop: &AtomicBool,
    report: &mut dyn FnMut(&str),
) -> Result<Listing> {
    let mut out = Listing::default();
    let mut rng = seed();
    let mut asked = false;
    for id in ids(corpus) {
        if stop.load(Ordering::Relaxed) {
            out.stopped = true;
            break;
        }
        let known = Info::load(dir, &id);
        let file = video_path(dir, &id);
        let info = match known {
            Some(i) if i.downloaded() && file.is_file() => i,
            Some(i) if !refresh => i,
            _ => {
                if asked && wait(draw(&mut rng, LIST_DELAY), stop) {
                    out.stopped = true;
                    break;
                }
                asked = true;
                match probe(&id) {
                    Ok(i) => {
                        i.save(dir)?;
                        i
                    }
                    Err(e) => {
                        report(&alloc::format!("{id}: {e:#}"));
                        out.failed.push(id);
                        continue;
                    }
                }
            }
        };
        report(&describe(&info));
        out.infos.push(info);
    }
    Ok(out)
}

/// One line about a video
fn describe(i: &Info) -> String {
    let minutes = i
        .duration_s
        .map(|d| alloc::format!("{}:{:02}", (d / 60.0) as u64, (d % 60.0) as u64))
        .unwrap_or_else(|| String::from("?:??"));
    let state = match (&i.unavailable, i.downloaded()) {
        (Some(why), _) => alloc::format!("unavailable: {why}"),
        (None, true) => String::from("downloaded"),
        (None, false) => String::from("to download"),
    };
    alloc::format!(
        "{} {minutes} {:>8} {state} - {} ({})",
        i.id,
        i.bytes.map(size).unwrap_or_else(|| String::from("?")),
        i.title.as_deref().unwrap_or("?"),
        i.channel.as_deref().unwrap_or("?")
    )
}

/// How a run goes
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// Seconds between two downloads
    pub delay: Range,
    /// At most this many downloads
    pub max: Option<usize>,
    /// Ask again for videos remembered as unavailable
    pub retry_unavailable: bool,
}

/// What a run did
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub downloaded: usize,
    pub bytes: u64,
    pub present: usize,
    pub unavailable: usize,
    pub failed: usize,
    pub stopped: bool,
}

impl core::fmt::Display for Summary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "downloaded {} videos ({}); {} were there already, {} unavailable, {} failed{}",
            self.downloaded,
            size(self.bytes),
            self.present,
            self.unavailable,
            self.failed,
            if self.stopped { "; stopped" } else { "" }
        )
    }
}

/// Downloads the corpus's videos that are not there yet, one at a time
/// with a pause between them, until done, stopped, `max` reached or
/// [`MAX_FAILURES`] transient failures in a row
pub fn run(
    corpus: &Corpus,
    dir: &Path,
    options: &Options,
    stop: &AtomicBool,
    report: &mut dyn FnMut(&str),
) -> Result<Summary> {
    let mut out = Summary::default();
    let mut rng = seed();
    let mut failures = 0;
    let mut started = false;
    for id in ids(corpus) {
        if stop.load(Ordering::Relaxed) {
            out.stopped = true;
            break;
        }
        if options.max.is_some_and(|m| out.downloaded >= m) {
            break;
        }
        let known = Info::load(dir, &id);
        if known.as_ref().is_some_and(|i| i.downloaded()) && video_path(dir, &id).is_file() {
            out.present += 1;
            continue;
        }
        if let Some(why) = known.as_ref().and_then(|i| i.unavailable.as_deref())
            && !options.retry_unavailable
        {
            report(&alloc::format!("{id}: unavailable ({why}), skipped"));
            out.unavailable += 1;
            continue;
        }
        if started {
            let pause = draw(&mut rng, options.delay);
            report(&alloc::format!("waiting {pause:.0} s"));
            if wait(pause, stop) {
                out.stopped = true;
                break;
            }
        }
        started = true;
        let title = known
            .as_ref()
            .and_then(|i| i.title.clone())
            .unwrap_or_default();
        report(&alloc::format!("{id}: downloading {title}"));
        match download(&id, dir, report) {
            Ok(info) => {
                info.save(dir)?;
                match &info.unavailable {
                    Some(why) => {
                        report(&alloc::format!("{id}: unavailable: {why}"));
                        out.unavailable += 1;
                    }
                    None => {
                        report(&alloc::format!(
                            "{id}: done, {} ({})",
                            size(info.bytes.unwrap_or_default()),
                            info.title.as_deref().unwrap_or("")
                        ));
                        out.downloaded += 1;
                        out.bytes += info.bytes.unwrap_or_default();
                    }
                }
                failures = 0;
            }
            Err(e) => {
                report(&alloc::format!("{id}: failed: {e:#}"));
                out.failed += 1;
                failures += 1;
                if stop.load(Ordering::Relaxed) {
                    out.stopped = true;
                    break;
                }
                if failures >= MAX_FAILURES {
                    report(&alloc::format!(
                        "{failures} failures in a row: stopping (run again later)"
                    ));
                    out.stopped = true;
                    break;
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn info_from_ytdlp() {
        let v = json!({
            "title": "Run 3", "channel": "souper", "duration": 1234.5,
            "upload_date": "20230501", "width": 854, "height": 480,
            "requested_formats": [
                {"filesize": 100_000_000},
                {"filesize_approx": 5_000_000}
            ]
        });
        let i = Info::from_ytdlp("abc", &v);
        assert_eq!(i.title.as_deref(), Some("Run 3"));
        assert_eq!(i.upload_date.as_deref(), Some("2023-05-01"));
        assert_eq!(i.bytes, Some(105_000_000));
        assert_eq!(i.height, Some(480));
        assert!(!i.downloaded());
        // A single file's own size; one without any
        assert_eq!(
            Info::from_ytdlp("x", &json!({"filesize_approx": 7})).bytes,
            Some(7)
        );
        assert_eq!(
            Info::from_ytdlp("x", &json!({"requested_formats": [{}]})).bytes,
            None
        );
        assert!(describe(&i).starts_with("abc 20:34   105 MB to download - Run 3 (souper)"));
    }

    #[test]
    fn unavailable_reasons() {
        assert_eq!(
            unavailable_reason("WARNING: x\nERROR: [youtube] abc: Video unavailable\n").as_deref(),
            Some("[youtube] abc: Video unavailable")
        );
        assert_eq!(
            unavailable_reason(
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access"
            )
            .as_deref(),
            Some("[youtube] abc: Private video. Sign in if you've been granted access")
        );
        assert!(unavailable_reason("ERROR: [youtube] abc: This video is unavailable").is_some());
        assert_eq!(
            unavailable_reason("ERROR: [youtube] abc: Sign in to confirm you're not a bot"),
            None
        );
        assert_eq!(
            unavailable_reason("ERROR: Unable to download webpage"),
            None
        );
    }

    #[test]
    fn listings_add_up_and_infos_round_trip() {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-corpus-listing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut done = Info::from_ytdlp("done", &json!({"filesize": 10}));
        done.downloaded_at = Some(Utc::now());
        done.save(&dir).unwrap();
        assert_eq!(Info::load(&dir, "done"), Some(done.clone()));
        let listing = Listing {
            infos: alloc::vec![
                done,
                Info::from_ytdlp("a", &json!({"filesize": 100})),
                Info::from_ytdlp("b", &json!({})),
                Info {
                    id: String::from("gone"),
                    unavailable: Some(String::from("Private video")),
                    ..Info::default()
                },
            ],
            failed: alloc::vec![String::from("x")],
            stopped: false,
        };
        assert_eq!(listing.todo().count(), 2);
        assert_eq!(listing.todo_bytes(), 100);
        assert_eq!(
            listing.summary(),
            "4 videos: 1 downloaded (0 MB), 2 to download (0 MB plus 1 of unknown size), 1 unavailable, 1 not answered (see above)"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn draws_stay_in_range() {
        let mut rng = seed();
        for _ in 0..1000 {
            let d = draw(&mut rng, DEFAULT_DELAY);
            assert!((5.0..=5.0).contains(&d));
        }
        assert_eq!(draw(&mut rng, Range::new(4.0, 4.0)), 4.0);
    }

    #[test]
    fn ids_of_a_corpus() {
        let root = crate::corpus::tests::fixture("videos");
        let corpus = crate::corpus::build(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(ids(&corpus), ["ftjLO--ch5w", "abcdefghijk"]);
    }
}
