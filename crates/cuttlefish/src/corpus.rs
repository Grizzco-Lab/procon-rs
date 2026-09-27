//! The VOD-review corpus: every reviewed video of the #vod-review archive
//! with the comments on it, as one JSON line per conversation
//! (`<knowledge>/corpus/vod-review.jsonl`, [`build`], [`write`]).
//!
//! A conversation of the channel ([`crate::discord::conversations`]) is a
//! [`Vod`] when someone posted a video in it: the video (a YouTube id with
//! the link's start, or an attachment with its downloaded file), the
//! poster, the date and the game era ([`crate::game`]), and the messages
//! with the moments they point at ([`crate::moments`]). Each moment is
//! either `aligned` (a time of the video: `1:20`, a link's `t=`) or
//! `needs_hud` (the wave timer: `W2 :50`), which only a reading of the
//! video's HUD can place: a wave-start table per video
//! ([`WaveStarts`], `<knowledge>/corpus/wave-starts/<video key>.json`),
//! which a HUD reader writes and the next build reads, placing the
//! timer moments and, when it names the game, correcting the era.
//!
//! Building is deterministic: the same archive, media and tables give the
//! same file. [`crate::corpus_videos`] downloads the YouTube videos,
//! [`crate::corpus_reviews`] turns the VODs into reviews of the studio.

use crate::discord::{self, Channel, Message, MessageRow, VideoFrom, attachment_id};
use crate::discord_media::{MEDIA, Manifest};
use crate::doc::doc_id;
use crate::game::{self, Game};
use crate::inbox::INBOX;
use crate::moments::{Moment, MomentKind, WAVE_S};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use core::fmt;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The corpus folder in the knowledge folder
pub const CORPUS_DIR: &str = "corpus";
/// The corpus file in it
pub const CORPUS_FILE: &str = "vod-review.jsonl";
/// The wave-start tables in it, one per video key
pub const WAVE_STARTS_DIR: &str = "wave-starts";
/// Downloaded YouTube videos, `<id>.mp4` with `<id>.info.json`, in the
/// knowledge folder
pub const YOUTUBE_DIR: &str = "media/youtube";

/// Where a video is hosted
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoKind {
    /// A YouTube video, by id
    Youtube,
    /// A file uploaded to Discord, by attachment id
    Attachment,
    /// Anything else (Twitch, a drive link)
    Other,
}

/// A video a conversation is about
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Video {
    /// `youtube-<id>`, `discord-<attachment id>` or `other-<hash>`: the
    /// name of its wave-start table and of its downloaded file
    pub key: String,
    pub kind: VideoKind,
    /// The link as posted
    pub url: String,
    /// The YouTube id or the attachment id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Where the link starts (`t=`), seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_s: Option<f32>,
    /// An attachment's file name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The file in the knowledge folder, when downloaded
    /// (`media/discord/<guild>/<channel>/<message>/<name>` or
    /// `media/youtube/<id>.mp4`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<String>,
}

/// The YouTube id in a link (`youtu.be/<id>`, `watch?v=<id>`,
/// `live|shorts|embed/<id>`)
pub fn youtube_id(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .or_else(|| host.strip_prefix("music."))
        .unwrap_or(&host);
    let id_chars = |s: &str| {
        let id: String = s
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        (8..=16).contains(&id.len()).then_some(id)
    };
    if host == "youtu.be" {
        return id_chars(path);
    }
    if host != "youtube.com" {
        return None;
    }
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    for prefix in ["live/", "shorts/", "embed/"] {
        if let Some(rest) = route.strip_prefix(prefix) {
            return id_chars(rest);
        }
    }
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("v="))
        .and_then(id_chars)
}

impl Video {
    /// A video from its link; `local` knows the downloaded file of an
    /// attachment id, and `youtube_dir` holds the downloaded YouTube
    /// videos
    pub fn from_url(
        url: &str,
        t_s: Option<f32>,
        local: &dyn Fn(&str) -> Option<String>,
        youtube_dir: &Path,
    ) -> Video {
        if let Some(id) = youtube_id(url) {
            let file = alloc::format!("{id}.mp4");
            let local = youtube_dir
                .join(&file)
                .is_file()
                .then(|| alloc::format!("{YOUTUBE_DIR}/{file}"));
            return Video {
                key: alloc::format!("youtube-{id}"),
                kind: VideoKind::Youtube,
                url: String::from(url),
                id: Some(id),
                t_s,
                name: None,
                local,
            };
        }
        if let Some(id) = attachment_id(url) {
            let name = url
                .split(['?', '#'])
                .next()
                .and_then(|p| p.rsplit('/').next())
                .filter(|n| !n.is_empty())
                .map(String::from);
            return Video {
                key: alloc::format!("discord-{id}"),
                kind: VideoKind::Attachment,
                url: String::from(url),
                id: Some(String::from(id)),
                t_s: None,
                name,
                local: local(id),
            };
        }
        let bare = url.split(['?', '#']).next().unwrap_or(url);
        Video {
            key: alloc::format!("other-{}", doc_id(bare)),
            kind: VideoKind::Other,
            url: String::from(url),
            id: None,
            t_s,
            name: None,
            local: None,
        }
    }
}

/// What placed an aligned moment
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacedBy {
    /// A time of the video written in the text
    Text,
    /// A link's start time
    Link,
    /// The wave-start table of the video ([`WaveStarts`])
    Hud,
}

/// A moment of a message, with whether it is placed in the video
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorpusMoment {
    /// As written
    pub raw: String,
    pub kind: MomentKind,
    /// Seconds into the video, or left on the wave timer, as written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f32>,
    /// The wave: named in the text, else the last one named before it in
    /// the same message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<u8>,
    /// Seconds into the VOD's video, when placed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_s: Option<f32>,
    /// Placed in the video
    pub aligned: bool,
    /// A wave-timer moment waiting for the video's wave-start table
    pub needs_hud: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placed_by: Option<PlacedBy>,
}

/// A message of a VOD conversation
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReviewMessage {
    pub id: String,
    /// Link to the message
    pub url: String,
    pub author: String,
    pub time: DateTime<Utc>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_author: Option<String>,
    /// The video this message is about when it is not the VOD's
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moments: Vec<CorpusMoment>,
}

/// Where a VOD's era comes from
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameFrom {
    /// The date of the post ([`crate::game::era`])
    Date,
    /// The video's wave-start table named it ([`WaveStarts::game`])
    Hud,
}

/// A reviewed video: one conversation of #vod-review with a video in it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vod {
    /// The conversation's first message id
    pub id: String,
    /// Link to that message
    pub url: String,
    /// Channel name (`vod-review`)
    pub channel: String,
    /// The thread's name when the conversation is one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// Who posted the video
    pub poster: String,
    /// The day it was posted (UTC)
    pub date: NaiveDate,
    /// When it was posted
    pub posted: DateTime<Utc>,
    pub game: Game,
    pub game_from: GameFrom,
    pub video: Video,
    /// More videos of the same post
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_videos: Vec<Video>,
    /// Every message of the conversation, the post first
    pub messages: Vec<ReviewMessage>,
}

impl Vod {
    /// The messages after the post: the comments on it
    pub fn comments(&self) -> usize {
        self.messages.len().saturating_sub(1)
    }

    /// The video's file in the knowledge folder, when it is there
    pub fn local_video(&self, knowledge: &Path) -> Option<PathBuf> {
        let path = knowledge.join(self.video.local.as_ref()?);
        path.is_file().then_some(path)
    }
}

/// The start of each wave in a video, read from its HUD (by the HUD
/// reader, outside this crate): `<knowledge>/corpus/wave-starts/<video
/// key>.json`. Places a wave-timer moment: the wave starts with 100 s on
/// the timer, so `W2 :50` is 50 s after wave 2 started.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WaveStarts {
    /// The video key ([`Video::key`])
    pub video: String,
    /// The game the HUD shows, when the reader tells it apart; overrides
    /// the era from the date
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<Game>,
    /// Who wrote the table (`hud`, `manual`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    #[serde(default)]
    pub waves: Vec<WaveStart>,
}

/// One wave of a video
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveStart {
    /// 1 to 3, 4 or 5 for extra waves
    pub wave: u8,
    /// Seconds into the video the timer shows 100
    pub start_s: f32,
    /// Seconds into the video the wave ended, when known
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f32>,
}

impl WaveStarts {
    /// The table's file for a video key
    pub fn path(knowledge: &Path, key: &str) -> PathBuf {
        knowledge
            .join(CORPUS_DIR)
            .join(WAVE_STARTS_DIR)
            .join(alloc::format!("{key}.json"))
    }

    /// The table of a video, when one was written
    pub fn load(knowledge: &Path, key: &str) -> Result<Option<Self>> {
        let path = Self::path(knowledge, key);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| alloc::format!("reading {}", path.display()))?;
        let table: WaveStarts =
            serde_json::from_str(&text).with_context(|| alloc::format!("in {}", path.display()))?;
        Ok(Some(table))
    }

    /// Writes the table
    pub fn save(&self, knowledge: &Path) -> Result<()> {
        crate::store::write_atomic(
            &Self::path(knowledge, &self.video),
            &serde_json::to_vec_pretty(self)?,
        )
    }

    /// Seconds into the video at which `wave` had `timer_s` left; none
    /// without that wave, or past its known end
    pub fn place(&self, wave: u8, timer_s: f32) -> Option<f32> {
        let w = self.waves.iter().find(|w| w.wave == wave)?;
        let t = w.start_s + (WAVE_S - timer_s);
        (t >= 0.0 && w.end_s.is_none_or(|end| t <= end)).then_some(t)
    }
}

/// Moments of a message as corpus moments: a wave named earlier in the
/// message carries to the timers after it; a time is aligned when it is
/// the VOD's video's (`vod_key`), a timer when `starts` places it. Bare
/// video links are dropped, and in the post (`is_post`) so are links to
/// the VOD's video with a start: that start is the video's
/// ([`Video::t_s`]), not a moment.
pub fn corpus_moments(
    moments: &[Moment],
    vod_key: &str,
    is_post: bool,
    starts: Option<&WaveStarts>,
    key_of: &dyn Fn(&str) -> String,
) -> Vec<CorpusMoment> {
    let mut wave_context: Option<u8> = None;
    let mut out = Vec::new();
    for m in moments {
        if m.wave.is_some() {
            wave_context = m.wave;
        }
        let is_link = m.url.as_deref().is_some_and(|u| m.raw == u);
        let same_video = m.url.as_deref().is_none_or(|u| key_of(u) == vod_key);
        if is_link && (m.seconds.is_none() || (is_post && same_video)) {
            continue;
        }
        let wave = m.wave.or(wave_context);
        let mut cm = CorpusMoment {
            raw: m.raw.clone(),
            kind: m.kind,
            seconds: m.seconds,
            wave,
            t_s: None,
            aligned: false,
            needs_hud: false,
            placed_by: None,
        };
        match (m.kind, m.seconds) {
            (MomentKind::VideoTime, Some(s)) if same_video => {
                cm.t_s = Some(s);
                cm.aligned = true;
                cm.placed_by = Some(if is_link {
                    PlacedBy::Link
                } else {
                    PlacedBy::Text
                });
            }
            (MomentKind::WaveTimer, Some(s)) if same_video => {
                match wave.and_then(|w| starts?.place(w, s)) {
                    Some(t) => {
                        cm.t_s = Some(t);
                        cm.aligned = true;
                        cm.placed_by = Some(PlacedBy::Hud);
                    }
                    None => cm.needs_hud = true,
                }
            }
            // A wave named without a time: where it starts, once known
            (MomentKind::Unknown, None) if same_video => {
                if let Some(t) = m.wave.and_then(|w| starts?.place(w, WAVE_S)) {
                    cm.t_s = Some(t);
                    cm.aligned = true;
                    cm.placed_by = Some(PlacedBy::Hud);
                }
            }
            _ => {}
        }
        out.push(cm);
    }
    out
}

/// One channel or thread of the archive with what its videos need
struct Source {
    channel: Channel,
    messages: Vec<Message>,
    rows: Vec<MessageRow>,
    /// `media/discord/<guild>/<channel>` from the knowledge folder
    media_below: String,
    manifest: Manifest,
}

/// The #vod-review channels and threads of the archive
/// (`<knowledge>/inbox/discord/<guild>/<channel>/`), oldest message first
fn sources(knowledge: &Path) -> Result<Vec<Source>> {
    let mut out = Vec::new();
    let discord = knowledge.join(INBOX).join("discord");
    let Ok(guilds) = std::fs::read_dir(&discord) else {
        return Ok(out);
    };
    let mut folders: Vec<PathBuf> = Vec::new();
    for guild in guilds.flatten().filter(|e| e.path().is_dir()) {
        for channel in std::fs::read_dir(guild.path())?
            .flatten()
            .filter(|e| e.path().is_dir())
        {
            folders.push(channel.path());
        }
    }
    folders.sort();
    let name_of = |p: &Path| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    for folder in folders {
        let channel_id = name_of(&folder);
        let guild_id = folder.parent().map(name_of).unwrap_or_default();
        let mut files = alloc::vec![folder.join(alloc::format!("{channel_id}.messages.jsonl"))];
        if let Ok(threads) = std::fs::read_dir(folder.join("threads")) {
            let mut t: Vec<PathBuf> = threads
                .flatten()
                .map(|e| e.path())
                .filter(|p| name_of(p).ends_with(".messages.jsonl"))
                .collect();
            t.sort();
            files.extend(t);
        }
        let media_below = alloc::format!("{MEDIA}/discord/{guild_id}/{channel_id}");
        for file in files.into_iter().filter(|f| f.is_file()) {
            let (channel, messages) = discord::read_archive(&file)?;
            if !channel.is_vod_review() || messages.is_empty() {
                continue;
            }
            let manifest = Manifest::load(&knowledge.join(&media_below))?;
            let mut rows = discord::rows(&channel, &messages);
            discord::link_media_rows(&mut rows, &|id| {
                manifest
                    .get(id)
                    .map(|e| alloc::format!("{media_below}/{}", e.path))
            });
            out.push(Source {
                channel,
                messages,
                rows,
                media_below: media_below.clone(),
                manifest,
            });
        }
    }
    Ok(out)
}

/// The corpus
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Corpus {
    pub vods: Vec<Vod>,
}

/// Builds the corpus from the archive in the knowledge folder: the
/// #vod-review channel and its threads, their downloaded attachments
/// (`media/discord/.../media.jsonl`), the downloaded YouTube videos
/// (`media/youtube/`) and the wave-start tables (`corpus/wave-starts/`)
pub fn build(knowledge: &Path) -> Result<Corpus> {
    let youtube_dir = knowledge.join(YOUTUBE_DIR);
    let mut vods = Vec::new();
    for src in sources(knowledge)? {
        let local = |id: &str| {
            src.manifest
                .get(id)
                .map(|e| alloc::format!("{}/{}", src.media_below, e.path))
        };
        let video_of = |url: &str, t: Option<f32>| Video::from_url(url, t, &local, &youtube_dir);
        let key_of = |url: &str| video_of(url, None).key;
        for g in discord::groups(&src.messages, src.channel.thread) {
            // The post: the first message with a video of its own, else
            // the first about one
            let Some(&post) = g
                .iter()
                .find(|&&i| src.rows[i].video_from == Some(VideoFrom::Own))
                .or_else(|| g.iter().find(|&&i| src.rows[i].video_url.is_some()))
            else {
                continue;
            };
            // Each video once: a link is in the text and its embed both
            let mut videos: Vec<Video> = Vec::new();
            if src.rows[post].video_from == Some(VideoFrom::Own) {
                for (url, t) in discord::videos_in(&src.messages[post]) {
                    let v = video_of(&url, t);
                    if !videos.iter().any(|x| x.key == v.key) {
                        videos.push(v);
                    }
                }
            }
            if videos.is_empty()
                && let Some(url) = &src.rows[post].video_url
            {
                videos.push(video_of(url, src.rows[post].video_t));
            }
            let video = videos.remove(0);
            let starts = WaveStarts::load(knowledge, &video.key)?;
            let first = &src.messages[g[0]];
            let poster = &src.messages[post];
            let (game, game_from) = match starts.as_ref().and_then(|s| s.game) {
                Some(g) => (g, GameFrom::Hud),
                None => (game::era(poster.timestamp), GameFrom::Date),
            };
            let messages = g
                .iter()
                .map(|&i| {
                    let (m, row) = (&src.messages[i], &src.rows[i]);
                    let about = row
                        .video_url
                        .as_deref()
                        .filter(|u| key_of(u) != video.key)
                        .map(String::from);
                    ReviewMessage {
                        id: m.id.clone(),
                        url: discord::message_link(&src.channel, &m.id),
                        author: m.author.clone(),
                        time: m.timestamp,
                        text: String::from(m.content.trim()),
                        reply_to: row.reply_to.clone(),
                        reply_author: row.reply_author.clone(),
                        video: about,
                        moments: corpus_moments(
                            &row.moments,
                            &video.key,
                            i == post,
                            starts.as_ref(),
                            &key_of,
                        ),
                    }
                })
                .collect();
            vods.push(Vod {
                id: first.id.clone(),
                url: discord::message_link(&src.channel, &first.id),
                channel: src
                    .channel
                    .parent
                    .clone()
                    .unwrap_or_else(|| src.channel.name.clone()),
                thread: src.channel.thread.then(|| src.channel.name.clone()),
                poster: poster.author.clone(),
                date: poster.timestamp.date_naive(),
                posted: poster.timestamp,
                game,
                game_from,
                video,
                other_videos: videos,
                messages,
            });
        }
    }
    vods.sort_by(|a, b| (a.posted, &a.id).cmp(&(b.posted, &b.id)));
    Ok(Corpus { vods })
}

/// The corpus file
pub fn path(knowledge: &Path) -> PathBuf {
    knowledge.join(CORPUS_DIR).join(CORPUS_FILE)
}

/// Writes the corpus, one VOD per line; answers with the file
pub fn write(knowledge: &Path, corpus: &Corpus) -> Result<PathBuf> {
    let mut out = Vec::new();
    for vod in &corpus.vods {
        serde_json::to_writer(&mut out, vod)?;
        out.push(b'\n');
    }
    let file = path(knowledge);
    crate::store::write_atomic(&file, &out)?;
    Ok(file)
}

/// Reads a written corpus
pub fn read(knowledge: &Path) -> Result<Corpus> {
    let file = path(knowledge);
    let text = std::fs::read_to_string(&file)
        .with_context(|| alloc::format!("reading {} (run `corpus build`)", file.display()))?;
    let mut vods = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        vods.push(serde_json::from_str(line).context("bad corpus line")?);
    }
    Ok(Corpus { vods })
}

/// Counts over a corpus
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub vods: usize,
    /// VODs per era
    pub s2: usize,
    pub s3: usize,
    /// VODs whose video is on disk
    pub with_local_video: usize,
    /// Unique YouTube videos, and how many are downloaded
    pub youtube_videos: usize,
    pub youtube_downloaded: usize,
    /// VODs uploaded as attachments, and how many are downloaded
    pub attachments: usize,
    pub attachments_downloaded: usize,
    /// Messages after the posts
    pub comments: usize,
    /// Most comments on one VOD
    pub max_comments: usize,
    /// Moments by kind
    pub video_time: usize,
    pub wave_timer: usize,
    pub unknown: usize,
    /// Moments placed in their video now, and how many by the HUD tables
    pub aligned: usize,
    pub aligned_by_hud: usize,
    /// Wave-timer moments waiting for a table
    pub needs_hud: usize,
    /// VODs with a wave-start table
    pub with_wave_starts: usize,
}

impl Stats {
    /// Counts `corpus`; `knowledge` tells which videos are on disk
    pub fn of(corpus: &Corpus, knowledge: &Path) -> Stats {
        let mut s = Stats {
            vods: corpus.vods.len(),
            ..Stats::default()
        };
        let mut youtube: BTreeMap<&str, bool> = BTreeMap::new();
        for vod in &corpus.vods {
            match vod.game {
                Game::S2 => s.s2 += 1,
                Game::S3 => s.s3 += 1,
            }
            let local = vod.local_video(knowledge).is_some();
            s.with_local_video += local as usize;
            match vod.video.kind {
                VideoKind::Youtube => {
                    let id = vod.video.id.as_deref().unwrap_or_default();
                    let e = youtube.entry(id).or_insert(false);
                    *e |= local;
                }
                VideoKind::Attachment => {
                    s.attachments += 1;
                    s.attachments_downloaded += local as usize;
                }
                VideoKind::Other => {}
            }
            if vod.game_from == GameFrom::Hud
                || WaveStarts::path(knowledge, &vod.video.key).is_file()
            {
                s.with_wave_starts += 1;
            }
            s.comments += vod.comments();
            s.max_comments = s.max_comments.max(vod.comments());
            for m in vod.messages.iter().flat_map(|m| &m.moments) {
                match m.kind {
                    MomentKind::VideoTime => s.video_time += 1,
                    MomentKind::WaveTimer => s.wave_timer += 1,
                    MomentKind::Unknown => s.unknown += 1,
                }
                s.aligned += m.aligned as usize;
                s.aligned_by_hud += (m.placed_by == Some(PlacedBy::Hud)) as usize;
                s.needs_hud += m.needs_hud as usize;
            }
        }
        s.youtube_videos = youtube.len();
        s.youtube_downloaded = youtube.values().filter(|d| **d).count();
        s
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mean = if self.vods == 0 {
            0.0
        } else {
            self.comments as f64 / self.vods as f64
        };
        writeln!(
            f,
            "{} VODs: {} Splatoon 2 era, {} Splatoon 3 era; {} with the video on disk ({} of {} YouTube videos downloaded, {} of {} attachments)",
            self.vods,
            self.s2,
            self.s3,
            self.with_local_video,
            self.youtube_downloaded,
            self.youtube_videos,
            self.attachments_downloaded,
            self.attachments
        )?;
        writeln!(
            f,
            "{} comments ({mean:.1} per VOD, at most {})",
            self.comments, self.max_comments
        )?;
        writeln!(
            f,
            "{} moments: {} video times, {} wave timers, {} waves without a time",
            self.video_time + self.wave_timer + self.unknown,
            self.video_time,
            self.wave_timer,
            self.unknown
        )?;
        write!(
            f,
            "{} aligned now ({} through wave-start tables, {} VODs have one); {} wave timers wait for the HUD",
            self.aligned, self.aligned_by_hud, self.with_wave_starts, self.needs_hud
        )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A knowledge folder with a #vod-review archive: souper's YouTube VOD
    /// with two comments (one aligned, one on the wave timer), Cy's
    /// attachment (downloaded) with a comment and a pointer to another
    /// video, a Splatoon 2 era post with a link start, and a chat without
    /// a video
    pub(crate) fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-corpus-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let channel = root.join("inbox/discord/1/2");
        std::fs::create_dir_all(&channel).unwrap();
        std::fs::write(
            channel.join("2.channel.json"),
            r#"{"id": "2", "type": 0, "guild_id": "1", "name": "vod-review"}"#,
        )
        .unwrap();
        let line = |id: &str, time: &str, who: &str, text: &str, extra: serde_json::Value| {
            let mut m = json!({"id": id, "timestamp": time, "content": text,
                "author": {"id": who, "username": who}});
            for (k, v) in extra.as_object().into_iter().flatten() {
                m[k] = v.clone();
            }
            alloc::format!("{m}\n")
        };
        let reply = |to: &str| json!({"type": 19, "message_reference": {"message_id": to}});
        let mut text = String::new();
        text += &line(
            "100",
            "2020-09-05T10:00:00Z",
            "JX",
            "knight run https://www.youtube.com/watch?v=ftjLO--ch5w&t=6926",
            json!({"embeds": [{"title": "Knight POV", "url": "https://www.youtube.com/watch?v=ftjLO--ch5w"}]}),
        );
        text += &line(
            "200",
            "2023-05-01T10:00:00Z",
            "souper",
            "my run https://youtu.be/abcdefghijk\nfeedback welcome",
            json!({}),
        );
        text += &line(
            "201",
            "2023-05-01T10:05:00Z",
            "Ben",
            "at 1:20 go left, the basket starved",
            reply("200"),
        );
        text += &line(
            "202",
            "2023-05-01T10:06:00Z",
            "Ben",
            "W2 was rough\n:50 you were alone on the far side\n:30 good save",
            reply("200"),
        );
        text += &line(
            "203",
            "2023-05-01T10:07:00Z",
            "souper",
            "thanks!",
            reply("201"),
        );
        text += &line(
            "300",
            "2023-06-01T10:00:00Z",
            "Cy",
            "wipe on W3",
            json!({"attachments": [{"id": "77", "filename": "wipe.mp4", "size": 5, "content_type": "video/mp4", "url": "https://cdn.discordapp.com/attachments/2/77/wipe.mp4?ex=1"}]}),
        );
        text += &line(
            "301",
            "2023-06-01T10:10:00Z",
            "Dee",
            "0:12 nobody had the Flyfish",
            reply("300"),
        );
        text += &line(
            "302",
            "2023-06-01T10:11:00Z",
            "Dee",
            "see https://youtu.be/abcdefghijk?t=83 for a cleaner one",
            reply("300"),
        );
        text += &line(
            "400",
            "2023-07-01T10:00:00Z",
            "Eve",
            "anyone up for a shift tonight?",
            json!({}),
        );
        std::fs::write(channel.join("2.messages.jsonl"), text).unwrap();
        // Cy's attachment, downloaded
        let media = root.join("media/discord/1/2");
        std::fs::create_dir_all(media.join("300")).unwrap();
        std::fs::write(media.join("300/wipe.mp4"), b"12345").unwrap();
        std::fs::write(
            media.join(crate::discord_media::MANIFEST),
            r#"{"message_id":"300","attachment_id":"77","filename":"wipe.mp4","size":5,"content_type":"video/mp4","path":"300/wipe.mp4","sha256":"x"}
"#,
        )
        .unwrap();
        root
    }

    #[test]
    fn youtube_ids() {
        assert_eq!(
            youtube_id("https://youtu.be/abcdefghijk?t=83").as_deref(),
            Some("abcdefghijk")
        );
        assert_eq!(
            youtube_id("https://www.youtube.com/watch?v=ftjLO--ch5w&feature=youtu.be").as_deref(),
            Some("ftjLO--ch5w")
        );
        assert_eq!(
            youtube_id("https://m.youtube.com/watch?feature=x&v=abcdefghijk").as_deref(),
            Some("abcdefghijk")
        );
        assert_eq!(
            youtube_id("https://youtube.com/live/abcdefghijk?si=1").as_deref(),
            Some("abcdefghijk")
        );
        assert_eq!(youtube_id("https://www.twitch.tv/videos/1073919074"), None);
        assert_eq!(youtube_id("https://youtu.be/"), None);
    }

    #[test]
    fn groups_vods_with_their_moments() {
        let root = fixture("build");
        let corpus = build(&root).unwrap();
        assert_eq!(corpus.vods.len(), 3, "{corpus:?}");
        let [jx, souper, cy] = corpus.vods.as_slice() else {
            unreachable!()
        };
        assert_eq!(jx.poster, "JX");
        assert_eq!(jx.game, Game::S2);
        assert_eq!(jx.game_from, GameFrom::Date);
        assert_eq!(jx.video.key, "youtube-ftjLO--ch5w");
        assert_eq!(jx.video.t_s, Some(6926.0));
        assert!(
            jx.messages[0].moments.is_empty(),
            "the post's link is not a moment"
        );
        assert!(jx.other_videos.is_empty(), "the embed is the same video");
        assert_eq!(jx.comments(), 0);

        assert_eq!(souper.id, "200");
        assert_eq!(souper.url, "https://discord.com/channels/1/2/200");
        assert_eq!(souper.game, Game::S3);
        assert_eq!(souper.date.to_string(), "2023-05-01");
        assert_eq!(souper.channel, "vod-review");
        assert_eq!(souper.video.kind, VideoKind::Youtube);
        assert_eq!(souper.video.local, None);
        assert_eq!(souper.comments(), 3);
        let ben = &souper.messages[1];
        assert_eq!(ben.author, "Ben");
        assert_eq!(ben.reply_author.as_deref(), Some("souper"));
        assert_eq!(ben.url, "https://discord.com/channels/1/2/201");
        assert_eq!(ben.moments.len(), 1);
        assert!(ben.moments[0].aligned && !ben.moments[0].needs_hud);
        assert_eq!(ben.moments[0].t_s, Some(80.0));
        assert_eq!(ben.moments[0].placed_by, Some(PlacedBy::Text));
        // A wave named on its own line carries to the timers after it
        let timers = &souper.messages[2].moments;
        assert_eq!(timers.len(), 3);
        assert_eq!(timers[0].kind, MomentKind::Unknown);
        assert_eq!(timers[0].wave, Some(2));
        assert!(!timers[0].aligned && !timers[0].needs_hud);
        assert_eq!(timers[1].raw, ":50");
        assert_eq!(timers[1].wave, Some(2));
        assert!(timers[1].needs_hud && !timers[1].aligned);
        assert_eq!(timers[2].seconds, Some(30.0));
        assert!(timers[2].needs_hud);
        assert!(souper.messages[3].moments.is_empty());

        assert_eq!(cy.video.kind, VideoKind::Attachment);
        assert_eq!(cy.video.key, "discord-77");
        assert_eq!(cy.video.name.as_deref(), Some("wipe.mp4"));
        assert_eq!(
            cy.video.local.as_deref(),
            Some("media/discord/1/2/300/wipe.mp4")
        );
        assert!(cy.local_video(&root).is_some());
        // Cy's own "W3" is a wave without a time
        assert_eq!(cy.messages[0].moments.len(), 1);
        assert_eq!(cy.messages[0].moments[0].wave, Some(3));
        // Dee's 0:12 is in Cy's clip; her message with a link to another
        // video is about that one, so its time is not a moment of the clip
        let dee = &cy.messages[1].moments;
        assert_eq!(dee.len(), 1);
        assert!(dee[0].aligned);
        assert_eq!(dee[0].t_s, Some(12.0));
        assert_eq!(cy.messages[1].video, None);
        let other = &cy.messages[2];
        assert_eq!(
            other.video.as_deref(),
            Some("https://youtu.be/abcdefghijk?t=83")
        );
        assert_eq!(other.moments.len(), 1);
        assert_eq!(other.moments[0].raw, "https://youtu.be/abcdefghijk?t=83");
        assert!(!other.moments[0].aligned && !other.moments[0].needs_hud);
        assert_eq!(cy.comments(), 2);

        let stats = Stats::of(&corpus, &root);
        assert_eq!(stats.vods, 3);
        assert_eq!((stats.s2, stats.s3), (1, 2));
        assert_eq!(stats.with_local_video, 1);
        assert_eq!(stats.youtube_videos, 2);
        assert_eq!(stats.attachments, 1);
        assert_eq!(stats.comments, 5);
        assert_eq!(stats.max_comments, 3);
        assert_eq!(
            (stats.video_time, stats.wave_timer, stats.unknown),
            (3, 2, 2)
        );
        assert_eq!(stats.aligned, 2);
        assert_eq!(stats.needs_hud, 2);
        let text = stats.to_string();
        assert!(
            text.starts_with(
                "3 VODs: 1 Splatoon 2 era, 2 Splatoon 3 era; 1 with the video on disk"
            ),
            "{text}"
        );

        // Written and read back the same, twice the same bytes
        let file = write(&root, &corpus).unwrap();
        let first = std::fs::read(&file).unwrap();
        assert_eq!(read(&root).unwrap(), corpus);
        write(&root, &build(&root).unwrap()).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), first);
        assert_eq!(first.iter().filter(|b| **b == b'\n').count(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn wave_starts_place_timers_and_name_the_game() {
        let root = fixture("hud");
        let table = WaveStarts {
            video: String::from("youtube-abcdefghijk"),
            game: Some(Game::S2),
            by: Some(String::from("hud")),
            waves: alloc::vec![
                WaveStart {
                    wave: 1,
                    start_s: 10.0,
                    end_s: Some(115.0)
                },
                WaveStart {
                    wave: 2,
                    start_s: 130.0,
                    end_s: None
                },
            ],
        };
        assert_eq!(table.place(2, 50.0), Some(180.0));
        assert_eq!(table.place(1, 0.0), Some(110.0));
        // Past the wave's end, or a wave not in the table
        assert_eq!(table.place(1, -10.0), None);
        assert_eq!(table.place(3, 50.0), None);
        table.save(&root).unwrap();
        assert!(WaveStarts::path(&root, "youtube-abcdefghijk").is_file());
        let corpus = build(&root).unwrap();
        let souper = &corpus.vods[1];
        assert_eq!(souper.game, Game::S2);
        assert_eq!(souper.game_from, GameFrom::Hud);
        let timers = &souper.messages[2].moments;
        // The wave named on its own lands where it starts
        assert_eq!(timers[0].t_s, Some(130.0));
        assert!(timers[0].aligned);
        assert!(timers[1].aligned && !timers[1].needs_hud);
        assert_eq!(timers[1].t_s, Some(180.0));
        assert_eq!(timers[1].placed_by, Some(PlacedBy::Hud));
        assert_eq!(timers[2].t_s, Some(200.0));
        let stats = Stats::of(&corpus, &root);
        assert_eq!(stats.aligned, 5);
        assert_eq!(stats.aligned_by_hud, 3);
        assert_eq!(stats.needs_hud, 0);
        assert_eq!(stats.with_wave_starts, 1);
        assert_eq!((stats.s2, stats.s3), (2, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_without_an_archive_gives_an_empty_corpus() {
        let root = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-corpus-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(build(&root).unwrap().vods.is_empty());
        assert!(read(&root).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
