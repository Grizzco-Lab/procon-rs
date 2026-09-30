//! Cuttlefish app backend: review a video with comments, drawings and a chat
//!
//! A review is a conversation with Cuttlefish and, usually, one video (a
//! recorded session's segment, a video file on this machine or a range of a
//! YouTube video) with comments at its times, each with shapes drawn on the
//! paused frame: a notebook entry to flip through later. A review started
//! from the chat has no video until one is attached. Each review is a folder
//! in `[cuttlefish] reviews`:
//!
//! ```text
//! <reviews>/<id>/review.json
//! <reviews>/<id>/video.mp4     a YouTube range, or a local file copied in
//! ```
//!
//! ```json
//! {"video": {"kind": "youtube", "ref": "https://youtu.be/…", "start_s": 60, "end_s": 90,
//!            "file": "video.mp4", "title": "…", "channel": "…", "upload_date": "2026-09-01"},
//!  "comments": [{"id": "c1", "t_s": 12.5, "t_end_s": 14.0, "author": "user", "text": "Too far forward",
//!                "shapes": [{"kind": "arrow", "points": [[0.2, 0.3], [0.5, 0.5]], "color": "#ff5c8a"}],
//!                "created_ms": 1790000000000}],
//!  "notes": [{"id": "n1", "author": "user", "text": "Too passive all game", "created_ms": 1790000000002}],
//!  "messages": [{"id": "m1", "role": "user", "text": "Why was I splatted here?", "t_s": 12.5,
//!                "created_ms": 1790000000003},
//!               {"id": "m2", "role": "assistant", "text": "The Steelhead's bomb [S1]…",
//!                "sources": [{"id": "S1", "title": "…", "heading": "…", "url": "…",
//!                             "source": "guide", "license": "…"}],
//!                "comments": ["c2"], "created_ms": 1790000000009}]}
//! ```
//!
//! `ref` is the session folder and segment file (`<session>/<file>`), a file
//! path, or the YouTube URL (with the range in `start_s`/`end_s`). `file`,
//! when set, is the video inside the review folder (a plain file name), which
//! is what plays; a session review always points at the recording, which is
//! never copied. Times are seconds into the video as played: for a YouTube
//! range, from the start of the downloaded range. Shape points are fractions
//! (0–1) of the frame's width and height: two corners of a `rect` or
//! `ellipse`, tail and head of an `arrow`, every point of a `freehand` line.
//! `author` is `user`, or `Cuttlefish` for comments from the AI. `notes`
//! are about the whole video, tied to no time or drawing. `messages` is the
//! chat, oldest first: a user message may carry the moment (`t_s`) or range
//! (`t_s`, `t_end_s`) of the video it was asked with; an assistant message
//! carries the knowledge it cited (`sources`), the expert comments it was
//! given (`experts`) and the ids of the timed comments it added
//! (`comments`). Older reviews have no `notes` or
//! `messages`; neither is written when empty, and `video` is left out of a
//! review without one. `stage`, when picked on the page, is the Salmon Run
//! stage by its glossary id (`"stage": "spawning-grounds"`); the page links
//! it to Gungee's community maps (`web/stages.js`).
//!
//! Opening a YouTube range creates its review: `yt-dlp` downloads it into a
//! new review folder on a thread of its own (the page polls the progress), and
//! the review is written with the video's title, channel and upload date when
//! it is done. A YouTube review without them (such as one migrated from the
//! older layout) gets them in the background the first time it is listed or
//! opened in a run: `yt-dlp --skip-download` prints them, and they are
//! written into `review.json`. A save from the page keeps them, even when
//! the page loaded the review before they came. A local file can be copied
//! into its review folder.
//!
//! Reviews of the older layout, `<reviews>/<id>.json`, are moved into folders
//! at startup ([`Cuttlefish::migrate`]); a YouTube video still in the old
//! download cache (`~/.cache/procon-cuttlefish/yt-<hash>.mp4`) moves along.
//!
//! Endpoints under `/api/cuttlefish/`:
//!
//! - `GET reviews`: every review, newest first (with its video, if any, its
//!   counts and the first message of its chat), `fetching`, the reviews
//!   whose YouTube title is being looked up, and `refreshing` while the
//!   list is read again ([`ReviewList`]: kept in memory, since the folder
//!   may be a network mount); `GET`, `PUT`, `DELETE
//!   reviews/<id>` read, write and delete one (deleting removes its folder,
//!   video included; `PUT` answers with the video as saved); `POST
//!   reviews/<id>/copy` copies a local file review's video into its folder
//! - `GET video?kind=&ref=&start_s=&end_s=&file=&r=`: the video's bytes, with HTTP
//!   ranges, `r` naming the review whose folder holds it; `GET meta?…`: its
//!   frame rate, duration and size; `GET thumb?…&t_ms=`: a small JPEG of
//!   the frame at that time, for the neighbours strip, cached in memory
//! - `GET stage-map?stage=<Gungee's key>&tide=<Low|Mid|High>`: Gungee's
//!   top-down map of a Salmon Run stage (salmon-learn-nw.gungee.jp), fetched
//!   once into the local cache; the page credits him wherever it shows one
//! - `GET game-items`: `{"items"}`, the Salmon Run weapons and specials of
//!   Lean's datamine in the store, for the Studio's Techniques panel (see
//!   [`cuttlefish::leanny::items`]); `GET game-icon?path=<an item's icon>`:
//!   its picture from Lean's site (leanny.github.io), fetched once into the
//!   local cache; the page credits him where it shows them
//! - `POST download` with `{"url", "start_s", "end_s", "review"?}` starts
//!   downloading a YouTube range into a new review (or finds the review that
//!   has it), or with `review` into that review, which has no video yet;
//!   answers with the download, whose `id` is the review's. `GET downloads`
//!   lists this run's downloads
//! - `POST chat` with `{"message", "history": [{"role", "text"}], "video"?,
//!   "t_s"?, "t_end_s"?, "fps"?, "height"?, "review"?}` sends a chat message
//!   to the `cuttlefish` crate with the conversation so far and, when a
//!   video and `t_s` are given, frames from ffmpeg ([`frames`], cached on
//!   disk; which ones, [`cuttlefish::sampling`]: around `t_s`, dense near
//!   it, or over the range of at most [`MAX_RANGE_S`] at `fps` and
//!   `height`, more where the picture changes; a range longer than
//!   [`sampling::TWO_PASS_S`] takes a first pass over a sparse overview
//!   that picks the key moments, then sharper frames around those), the
//!   review's comments near it and the moment as text
//!   ([`cuttlefish::situation`]: the controller input of a session's
//!   recording, else of the Predictor's newest run on the video in
//!   `[predictor] results`; the HUD when a wave table sits beside the
//!   video; boxes a person labelled on the session's frame at `t_s`);
//!   knowledge comes from `[cuttlefish] knowledge`. Answers `{"text",
//!   "sources", "experts", "comments": [{"t_s", "t_end_s"?, "text",
//!   "shapes"}], "images": {"frames", "tokens"}}` (`experts`: every expert
//!   comment the model was given; `images`: the frames sent and their
//!   image tokens, about);
//!   the page saves the message into the review and adds the comments as
//!   Cuttlefish's. `501` while the
//!   reviewer cannot start: no model backend (`ANTHROPIC_API_KEY`, the only
//!   place the key is read from, or the logged-in Claude Code CLI; see
//!   `[cuttlefish] backend`)
//! - `POST translate` with `{"text", "target"}` translates for the Translate
//!   view ([`Knowledge::translate`]: the glossary terms the text uses, a bare
//!   term's entry, the model's translation and explanation when a backend
//!   is there, `needs_key` otherwise) and appends the result, with an `id` and
//!   `created_ms`, to the history `<reviews>/translations.jsonl` (one JSON
//!   object per line, the last [`TRANSLATIONS_KEPT`] kept); `GET
//!   translations` lists it newest first, `DELETE translations` removes it
//! - `GET pedia`, `GET pedia/<term id>?quotes=`: the Overfishing Pedia;
//!   `GET source?url=&doc=&ordinal=&title=&heading=`: the context of a
//!   cited source or a quote, for the page's source popover; see
//!   [`pedia`]
//! - `knowledge/...`: the knowledge view (overview, search, imports,
//!   glossary, assets), see [`knowledge`]; its store and embedder
//!   also serve `chat`
//!
//! Errors are `{"error": "..."}` with status 400 (404 for a missing review).

mod frames;
pub mod knowledge;
pub mod pedia;

use crate::inspect::objects::write_atomic;
use crate::inspect::{Inspector, ffprobe};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use cuttlefish::corpus_reviews::{Origin, Unplaced};
use cuttlefish::game::Game;
use cuttlefish::llm::{Role, Settings, Turn};
use cuttlefish::review::{self as ai, ChatRequest, KeyMoment, SourceRef, VideoContext};
use cuttlefish::sampling::{self, MAX_RANGE_S};
use cuttlefish::situation::{self, Input, InputSource, SeenObject, Situation};
use cuttlefish::{corpus, corpus_reviews, expert};
use frames::FrameSource;
use gameplay_data::labels::{self, Label};
use gameplay_data::session::SessionInfo;
use knowledge::{AutoApply, Knowledge, Status, Translation, now_ms};
use pedia::Pedia;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::Instant;
use warp::Filter;
use warp::filters::BoxedFilter;
use warp::http::{Method, Response, StatusCode};

/// Largest part of a video sent in one reply; the player asks for more
const VIDEO_CHUNK: u64 = 4 << 20;

/// Largest review accepted
const REVIEW_LIMIT: u64 = 16 << 20;

/// Video file extensions served from paths on this machine
const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mkv", "webm", "mov", "m4v", "ogv"];

/// A review's file in its folder
const REVIEW_FILE: &str = "review.json";

/// A downloaded YouTube range in its review folder
const VIDEO_FILE: &str = "video.mp4";

/// Review files read at once when the list is read: on a network mount
/// each is a round trip of its own
const LIST_READERS: usize = 16;

/// How long the list is served as it is before it is read again in the
/// background, for reviews written by others (`cuttlefish corpus reviews`)
const LIST_FRESH: Duration = Duration::from_secs(30);

/// Gungee's community Salmon Run tools, whose stage maps the page shows
const GUNGEE: &str = "https://salmon-learn-nw.gungee.jp";

/// Gungee's stage keys (his `/map/?stage=`), as `web/stages.js` has them
const GUNGEE_STAGES: [&str; 7] = [
    "Shakeup",
    "Shakespiral",
    "Shakedent",
    "Shakeship",
    "Shakehighway",
    "Shakelift",
    "Shakerail",
];

/// The tides Gungee's maps come in
const GUNGEE_TIDES: [&str; 3] = ["Low", "Mid", "High"];

/// The Translate view's history, next to the review folders
const TRANSLATIONS_FILE: &str = "translations.jsonl";

/// Translations kept in the history
pub const TRANSLATIONS_KEPT: usize = 500;

/// Marks the line with the video's title, channel and upload date in
/// yt-dlp's output
const META_MARK: &str = "cuttlefish-meta ";

/// The fields of that line, as JSON values
const META_FIELDS: &str = "%(title)j %(channel)j %(upload_date)j";

/// Height of a thumbnail of the neighbours strip, in pixels
const THUMB_HEIGHT: u32 = 144;

/// Thumbnails kept in memory
const THUMB_CACHE: usize = 600;

/// Comments this far outside the range still go to the reviewer, seconds
const NEAR_S: f64 = 10.0;

/// Characters of a chat's first message shown in the library
const TOPIC_CHARS: usize = 120;

/// The video a review is about
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoRef {
    pub kind: VideoKind,
    /// `<session>/<segment file>`, a file path or a YouTube URL
    #[serde(rename = "ref")]
    pub reference: String,
    /// Start of the YouTube range, in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_s: Option<f64>,
    /// End of the YouTube range, in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f64>,
    /// The video file in the review folder, which then plays
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Title of a YouTube video
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its channel
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Its upload date, `YYYY-MM-DD`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_date: Option<String>,
}

impl VideoRef {
    /// The same video: kind, reference and range
    fn same(&self, other: &VideoRef) -> bool {
        self.kind == other.kind
            && self.reference == other.reference
            && self.start_s == other.start_s
            && self.end_s == other.end_s
    }

    /// A YouTube video whose title is not known yet
    fn lacks_meta(&self) -> bool {
        self.kind == VideoKind::Youtube && self.title.is_none()
    }

    /// Fill in the title, channel and upload date this one lacks; answers
    /// whether anything changed
    fn add_meta(&mut self, meta: VideoMeta) -> bool {
        let mut changed = false;
        for (field, value) in [
            (&mut self.title, meta.title),
            (&mut self.channel, meta.channel),
            (&mut self.upload_date, meta.upload_date),
        ] {
            if field.is_none() && value.is_some() {
                *field = value;
                changed = true;
            }
        }
        changed
    }

    /// Title, channel and upload date, as far as they are known
    fn meta(&self) -> VideoMeta {
        VideoMeta {
            title: self.title.clone(),
            channel: self.channel.clone(),
            upload_date: self.upload_date.clone(),
        }
    }
}

/// Where a review's video comes from
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoKind {
    /// A segment of a recorded session under the Inkspector's root
    Session,
    /// A video file on this machine
    File,
    /// A range of a YouTube video, downloaded into the cache
    Youtube,
}

/// A comment at a time of the video, or over a range of it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    /// Time in the video, in seconds
    pub t_s: f64,
    /// End of the range the comment covers, in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_end_s: Option<f64>,
    /// `user`, `Cuttlefish` for the AI, or a reviewer's name for a comment
    /// from the #vod-review archive
    pub author: String,
    pub text: String,
    /// Drawn on the frame at `t_s`
    #[serde(default)]
    pub shapes: Vec<Shape>,
    /// When it was written, in Unix ms
    pub created_ms: u64,
    /// Where an imported comment came from (`cuttlefish corpus reviews`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Origin>,
}

/// A note on the whole video, tied to no time or drawing
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    /// `user`, `Cuttlefish` for the AI, or a reviewer's name
    pub author: String,
    pub text: String,
    /// When it was written, in Unix ms
    pub created_ms: u64,
    /// When it was last changed, in Unix ms
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edited_ms: Option<u64>,
    /// Where an imported note came from
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Origin>,
    /// Moments of an imported note not placed in the video yet (wave
    /// timers waiting for the video's wave table)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unplaced: Vec<Unplaced>,
}

/// A shape drawn on a frame
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub kind: ShapeKind,
    /// `[x, y]` fractions of the frame's width and height
    pub points: Vec<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

/// What a shape is
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    Rect,
    Ellipse,
    Arrow,
    Freehand,
}

/// A message of the chat with Cuttlefish
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub role: Role,
    pub text: String,
    /// The moment of the video a user message was asked with, in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_s: Option<f64>,
    /// End of the range it was asked with, in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_end_s: Option<f64>,
    /// Knowledge an assistant message cites
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceRef>,
    /// Expert comments an assistant message was given, cited or not
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub experts: Vec<SourceRef>,
    /// Ids of the comments an assistant message added
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<String>,
    /// When it was written, in Unix ms
    pub created_ms: u64,
}

/// A review file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Review {
    /// The video; a review started from the chat has none until one is
    /// attached
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoRef>,
    /// A title (an imported review's: the poster and the day); the
    /// library names other reviews by their video or first message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The game era the video is about, when known
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<Game>,
    /// The Salmon Run stage, by its glossary id (`spawning-grounds`), when
    /// picked on the page; it links to Gungee's community maps
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// Where an imported review came from: the #vod-review conversation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Origin>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Notes on the whole video; older reviews have none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
    /// The chat, oldest first; older reviews have none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Review {
    /// Check times, shapes and the video file are usable
    pub fn validate(&self) -> Result<()> {
        if let Some(file) = self.video.as_ref().and_then(|v| v.file.as_ref()) {
            check_file_name(file)?;
        }
        for message in &self.messages {
            if let Some(t_s) = message.t_s {
                ensure!(
                    t_s.is_finite() && t_s >= 0.0,
                    "message {} has a bad time",
                    message.id
                );
                if let Some(end) = message.t_end_s {
                    ensure!(end >= t_s, "message {} ends before it starts", message.id);
                }
            }
        }
        for comment in &self.comments {
            ensure!(
                comment.t_s.is_finite() && comment.t_s >= 0.0,
                "comment {} has a bad time",
                comment.id
            );
            if let Some(end) = comment.t_end_s {
                ensure!(
                    end >= comment.t_s,
                    "comment {} ends before it starts",
                    comment.id
                );
            }
            for shape in &comment.shapes {
                let needed = if shape.kind == ShapeKind::Freehand {
                    1
                } else {
                    2
                };
                ensure!(
                    shape.points.len() >= needed,
                    "a {:?} needs {needed} points",
                    shape.kind
                );
                ensure!(
                    shape.points.iter().flatten().all(|v| v.is_finite()),
                    "a shape has a bad point"
                );
            }
        }
        Ok(())
    }
}

/// State of a YouTube download
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    Running,
    Done,
    Failed,
}

/// A YouTube range being downloaded into its review
#[derive(Clone, Debug, Serialize)]
pub struct Download {
    /// The review it goes into
    pub id: String,
    pub url: String,
    pub start_s: Option<f64>,
    pub end_s: Option<f64>,
    pub state: DownloadState,
    /// How much is done, 0–100, when known
    pub percent: Option<f64>,
    /// yt-dlp's last words: its error, or what it is doing
    pub message: String,
}

/// YouTube titles looked up for reviews that lack them
#[derive(Default)]
struct MetaLookups {
    /// Reviews tried in this run, so each is asked about once
    tried: BTreeSet<String>,
    /// Reviews being looked up now
    running: BTreeSet<String>,
}

/// Recent thumbnails by video file and time in ms, the oldest dropped first
#[derive(Default)]
struct Thumbs {
    order: VecDeque<(PathBuf, u64)>,
    images: HashMap<(PathBuf, u64), Vec<u8>>,
}

/// A review as the library lists it
#[derive(Clone)]
struct Listed {
    /// When its `review.json` changed
    modified_ms: u64,
    video: Option<VideoRef>,
    /// The library's row
    row: Value,
}

impl Listed {
    fn new(id: &str, review: &Review, modified_ms: u64) -> Self {
        let topic = review
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .map(|m| m.text.chars().take(TOPIC_CHARS).collect::<String>());
        let row = json!({
            "id": id,
            "video": review.video,
            "title": review.title,
            "game": review.game,
            "stage": review.stage,
            "from": review.source.as_ref().map(|s| &s.from),
            "eggstra_event": review.source.as_ref().and_then(|s| s.eggstra_event),
            "comments": review.comments.len(),
            "messages": review.messages.len(),
            "topic": topic,
            "modified_ms": modified_ms,
        });
        Self {
            modified_ms,
            video: review.video.clone(),
            row,
        }
    }
}

/// The reviews listing, kept in memory: the reviews folder may be a network
/// mount (rclone on Dropbox), where every review folder not looked at in
/// the last minutes costs a round trip. It is read once at startup
/// ([`Cuttlefish::warm`]), with [`LIST_READERS`] files at once; the lab's
/// own writes update it, and a listing older than [`LIST_FRESH`] is served
/// while it is read again in the background. No lock is held while files
/// are read.
struct ReviewList {
    dir: PathBuf,
    state: Mutex<ListState>,
    /// Signalled when a reading ends
    read: Condvar,
}

#[derive(Default)]
struct ListState {
    /// Every review by id, once read
    reviews: Option<BTreeMap<String, Listed>>,
    read_at: Option<Instant>,
    /// A reading is under way
    reading: bool,
    /// The lab's changes while a reading is under way, applied over its
    /// result (`None`: deleted)
    changed: Vec<(String, Option<Listed>)>,
}

impl ReviewList {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            state: Mutex::default(),
            read: Condvar::new(),
        }
    }

    /// Every review and whether the list is being read again; the first
    /// call waits for the list to be read
    fn rows(self: &Arc<Self>) -> Result<(Vec<Listed>, bool)> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(reviews) = &state.reviews {
                let rows = reviews.values().cloned().collect();
                if !state.reading && state.read_at.is_none_or(|t| t.elapsed() > LIST_FRESH) {
                    state.reading = true;
                    state.changed.clear();
                    let list = Arc::clone(self);
                    std::thread::spawn(move || {
                        if let Err(e) = list.read_all() {
                            log::warn!("Could not read the reviews again: {:#}", e);
                        }
                    });
                }
                return Ok((rows, state.reading));
            }
            if state.reading {
                state = self.read.wait(state).unwrap();
                continue;
            }
            state.reading = true;
            state.changed.clear();
            drop(state);
            self.read_all()?;
            state = self.state.lock().unwrap();
        }
    }

    /// Read the list now, unless a reading is under way
    fn refresh(&self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            if state.reading {
                return Ok(());
            }
            state.reading = true;
            state.changed.clear();
        }
        self.read_all()
    }

    /// Read every review (the caller has set `reading`)
    fn read_all(&self) -> Result<()> {
        let started = Instant::now();
        let result = read_listing(&self.dir);
        let mut state = self.state.lock().unwrap();
        state.reading = false;
        self.read.notify_all();
        let changed = core::mem::take(&mut state.changed);
        let mut reviews = result?;
        log::debug!(
            "Read {} reviews in {} ms",
            reviews.len(),
            started.elapsed().as_millis()
        );
        for (id, listed) in changed {
            match listed {
                Some(listed) => reviews.insert(id, listed),
                None => reviews.remove(&id),
            };
        }
        state.reviews = Some(reviews);
        state.read_at = Some(Instant::now());
        Ok(())
    }

    /// The lab wrote review `id` (`None`: deleted it)
    fn put(&self, id: &str, review: Option<&Review>) {
        let listed = review.map(|review| Listed::new(id, review, now_ms()));
        let mut state = self.state.lock().unwrap();
        if state.reading {
            state.changed.push((id.to_string(), listed.clone()));
        }
        if let Some(reviews) = state.reviews.as_mut() {
            match listed {
                Some(listed) => reviews.insert(id.to_string(), listed),
                None => reviews.remove(id),
            };
        }
    }

    /// Others may have written reviews: the next listing reads them again
    fn stale(&self) {
        self.state.lock().unwrap().read_at = None;
    }
}

/// Reviews and the downloads under way
pub struct Cuttlefish {
    /// Sessions are read from the Inkspector's root
    inspector: Arc<Inspector>,
    reviews: PathBuf,
    /// The reviews as the library lists them
    list: Arc<ReviewList>,
    downloads: Arc<Mutex<BTreeMap<String, Download>>>,
    /// Held while a review file is written
    writing: Arc<Mutex<()>>,
    lookups: Arc<Mutex<MetaLookups>>,
    thumbs: Mutex<Thumbs>,
    /// Held while a picture of another site (Gungee's stage maps, Lean's
    /// icons) is fetched, so each is fetched once
    pictures: Mutex<()>,
    /// The `cuttlefish` crate's store, shared by the reviewer and the
    /// knowledge view
    knowledge: Arc<Knowledge>,
    /// The Predictor's runs (`[predictor] results`): their predictions give
    /// the controller input of videos without a recording
    predictions: PathBuf,
    /// The Overfishing Pedia's mentions and fact cards
    pedia: Pedia,
    /// The reviewer's frames, cached per video ([`frames`])
    frame_cache: PathBuf,
}

/// What a chat message about the video looks at
struct Watch {
    context: VideoContext,
    source: FrameSource,
    /// A long range: the frames are an overview for the first pass
    two_pass: bool,
}

/// A reply before it becomes an HTTP response
struct Reply {
    status: StatusCode,
    body: Vec<u8>,
    content_type: &'static str,
    content_range: Option<String>,
    /// Whether the browser may keep it
    cacheable: bool,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self::status(StatusCode::OK, value)
    }

    fn status(status: StatusCode, value: Value) -> Self {
        Self {
            status,
            body: value.to_string().into_bytes(),
            content_type: "application/json",
            content_range: None,
            cacheable: false,
        }
    }
}

impl Cuttlefish {
    pub fn new(
        inspector: Arc<Inspector>,
        reviews: PathBuf,
        knowledge: PathBuf,
        predictions: PathBuf,
        settings: Settings,
        translate_model: Option<String>,
        auto_apply: AutoApply,
    ) -> Self {
        Self {
            inspector,
            list: Arc::new(ReviewList::new(reviews.clone())),
            reviews,
            downloads: Arc::default(),
            writing: Arc::default(),
            lookups: Arc::default(),
            thumbs: Mutex::default(),
            pictures: Mutex::default(),
            knowledge: Arc::new(
                Knowledge::new(knowledge, settings, translate_model).with_auto_apply(auto_apply),
            ),
            predictions,
            pedia: Pedia::default(),
            frame_cache: cuttlefish::store::cache_dir().join("frames"),
        }
    }

    // ------------------------------------------------------------ reviews

    /// A review's folder
    fn review_dir(&self, id: &str) -> Result<PathBuf> {
        check_id(id)?;
        Ok(self.reviews.join(id))
    }

    /// A review's `review.json`
    fn review_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.review_dir(id)?.join(REVIEW_FILE))
    }

    /// Every review: its id, video, comment and message counts, the first
    /// message of its chat and its last change, newest first; from memory
    /// ([`ReviewList`])
    pub fn reviews(&self) -> Result<Value> {
        let (mut rows, refreshing) = self.list.rows()?;
        for listed in &rows {
            if let (Some(id), Some(video)) = (listed.row["id"].as_str(), &listed.video) {
                self.look_up_meta(id, video);
            }
        }
        rows.sort_by_key(|listed| core::cmp::Reverse(listed.modified_ms));
        let reviews: Vec<Value> = rows.into_iter().map(|listed| listed.row).collect();
        let fetching = self.lookups.lock().unwrap().running.clone();
        Ok(json!({
            "dir": self.reviews,
            "reviews": reviews,
            "fetching": fetching,
            "refreshing": refreshing,
        }))
    }

    /// Read the reviews list now, so that the library's first listing does
    /// not wait for the folder; the lab calls it on a thread at startup
    pub fn warm(&self) -> Result<()> {
        self.list.refresh()
    }

    /// One review
    pub fn review(&self, id: &str) -> Result<Review> {
        read_review(&self.review_path(id)?)
    }

    /// Write a review into its folder, replacing the file atomically; the
    /// video's title, channel and date stay when the new version lacks them
    /// (the page may have loaded it before they were looked up). Answers
    /// with the review as written.
    pub fn save_review(&self, id: &str, review: &Review) -> Result<Review> {
        review.validate()?;
        let path = self.review_path(id)?;
        let mut review = review.clone();
        let _writing = self.writing.lock().unwrap();
        if let (Ok(stored), Some(video)) = (read_review(&path), review.video.as_mut())
            && let Some(known) = stored.video
            && known.same(video)
        {
            video.add_meta(known.meta());
        }
        write_atomic(&path, &serde_json::to_vec_pretty(&review)?)?;
        self.list.put(id, Some(&review));
        Ok(review)
    }

    /// Look up the title, channel and upload date of a YouTube review that
    /// lacks them, on a thread of its own, once per run; they are written
    /// into its `review.json`
    fn look_up_meta(&self, id: &str, video: &VideoRef) {
        if !video.lacks_meta() {
            return;
        }
        let Ok(path) = self.review_path(id) else {
            return;
        };
        {
            let mut lookups = self.lookups.lock().unwrap();
            if !lookups.tried.insert(id.to_string()) {
                return;
            }
            lookups.running.insert(id.to_string());
        }
        let (id, url) = (id.to_string(), video.reference.clone());
        let writing = Arc::clone(&self.writing);
        let lookups = Arc::clone(&self.lookups);
        let list = Arc::clone(&self.list);
        std::thread::spawn(move || {
            let result = ytdlp_meta(&url).and_then(|meta| {
                let _writing = writing.lock().unwrap();
                let mut review = read_review(&path)?;
                let video = review.video.as_mut().context("the review lost its video")?;
                if video.add_meta(meta) {
                    let title = video.title.clone();
                    write_atomic(&path, &serde_json::to_vec_pretty(&review)?)?;
                    list.put(&id, Some(&review));
                    return Ok(title);
                }
                Ok(video.title.clone())
            });
            match result {
                Ok(title) => log::info!("Review {id} is {:?}", title.unwrap_or_default()),
                Err(e) => log::warn!("No title for review {id}: {:#}", e),
            }
            lookups.lock().unwrap().running.remove(&id);
        });
    }

    /// Delete a review's folder, its video included
    pub fn delete_review(&self, id: &str) -> Result<()> {
        let dir = self.review_dir(id)?;
        ensure!(dir.join(REVIEW_FILE).is_file(), "no review {id}");
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("cannot delete {}", dir.display()))?;
        self.list.put(id, None);
        Ok(())
    }

    /// A new review id from the local time, unused in the reviews folder
    fn new_id(&self) -> String {
        let base = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let downloads = self.downloads.lock().unwrap();
        (1..)
            .map(|n| {
                if n == 1 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .find(|id| !self.reviews.join(id).exists() && !downloads.contains_key(id))
            .unwrap()
    }

    /// The review of the same video whose folder holds it, if any
    fn review_with_video(&self, video: &VideoRef) -> Option<String> {
        let (rows, _) = self.list.rows().ok()?;
        rows.into_iter().find_map(|listed| {
            let id = listed.row["id"].as_str()?;
            let stored = listed.video.as_ref().filter(|v| v.same(video))?;
            let file = stored.file.as_ref()?;
            self.review_dir(id)
                .ok()?
                .join(file)
                .is_file()
                .then(|| id.to_string())
        })
    }

    /// Copy a local file review's video into its folder as `video.<ext>`;
    /// answers with the review as saved
    pub fn copy_into_review(&self, id: &str) -> Result<Review> {
        let mut review = self.review(id)?;
        let video = review.video.as_mut().context("the review has no video")?;
        ensure!(
            video.kind == VideoKind::File,
            "only a video file on this machine is copied in"
        );
        ensure!(video.file.is_none(), "the video is in the review already");
        let source = self.video_path(video, None)?;
        let extension = source
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mp4")
            .to_lowercase();
        let file = format!("video.{extension}");
        let target = self.review_dir(id)?.join(&file);
        let mut partial = target.as_os_str().to_owned();
        partial.push(".part");
        std::fs::copy(&source, &partial)
            .with_context(|| format!("cannot copy {}", source.display()))?;
        std::fs::rename(&partial, &target)?;
        video.file = Some(file);
        self.save_review(id, &review)?;
        Ok(review)
    }

    /// Start the job that turns the #vod-review archive into reviews, as
    /// `cuttlefish corpus align`, `corpus reviews` then `corpus index`: the
    /// corpus is built from the knowledge folder, every VOD video on disk
    /// without a wave table has its HUD read (1-2 s per minute of video;
    /// Stop ends it after the video under way), a review is created or
    /// updated for every VOD whose video is on disk, and every reviewer
    /// comment is indexed as an expert comment ([`cuttlefish::expert`];
    /// under the store's write lock, one VOD's document at a time, so chats
    /// go on meanwhile; Stop keeps what is embedded). Runs as a knowledge
    /// job, one at a time with the imports; answers with the job.
    pub fn community_reviews(&self) -> Result<Value, Status> {
        let reviews = self.reviews.clone();
        let writing = Arc::clone(&self.writing);
        let list = Arc::clone(&self.list);
        let job = self.knowledge.start_job(
            "Reviews from #vod-review".to_string(),
            move |knowledge, id| {
                let root = knowledge.root();
                let built = corpus::build(root)?;
                knowledge.log(id, corpus::Stats::of(&built, root).to_string());
                let aligned = corpus::align(
                    &built,
                    root,
                    false,
                    &|| knowledge.cancelled(),
                    &mut |line| knowledge.log(id, line.to_string()),
                )?;
                knowledge.log(id, aligned.to_string());
                ensure!(!aligned.stopped, "stopped before the reviews were written");
                let built = corpus::build(root)?;
                corpus::write(root, &built)?;
                let stats = corpus::Stats::of(&built, root);
                let written = {
                    let _writing = writing.lock().unwrap();
                    let written = corpus_reviews::write(&built, root, &reviews);
                    list.stale();
                    written?
                };
                knowledge.log(id, stats.to_string());
                knowledge.log(id, format!("reviews: {written}"));
                let _lock = cuttlefish::lock::acquire(root, "grizzco-lab expert comments")?;
                let loaded = knowledge.loaded()?;
                let plan = expert::plan(&loaded.store.read().unwrap(), &built)?;
                let mut embedded = 0;
                for doc in &plan.add {
                    if knowledge.cancelled() {
                        break;
                    }
                    loaded.store.write().unwrap().add(doc, &loaded.embedder)?;
                    embedded += 1;
                    if embedded % 10 == 0 {
                        loaded.store.read().unwrap().save()?;
                        knowledge.log(
                            id,
                            format!("expert comments: {embedded} of {} VODs embedded", plan.add.len()),
                        );
                    }
                }
                for doc_id in &plan.remove {
                    loaded.store.write().unwrap().delete(doc_id)?;
                }
                loaded.store.read().unwrap().save()?;
                ensure!(
                    embedded == plan.add.len(),
                    "stopped after {embedded} of {} VODs' expert comments; run it again for the rest",
                    plan.add.len()
                );
                Ok(format!("done: {written}; {}", plan.counts))
            },
        )?;
        Ok(json!(job))
    }

    /// Move reviews of the older layout, `<reviews>/<id>.json`, into
    /// folders, with their YouTube videos from the old download cache
    /// `legacy_cache`; answers with the number moved
    pub fn migrate(&self, legacy_cache: &Path) -> Result<usize> {
        let entries = match std::fs::read_dir(&self.reviews) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e).context("cannot list the reviews"),
        };
        let mut moved = 0;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|id| check_id(id).is_ok())
            else {
                continue;
            };
            if !path.is_file() {
                continue;
            }
            let dir = self.reviews.join(id);
            if dir.join(REVIEW_FILE).exists() {
                log::warn!("Not migrating {}: {id}/ exists", path.display());
                continue;
            }
            let mut review = read_review(&path)?;
            std::fs::create_dir_all(&dir)?;
            if let Some(video) = review
                .video
                .as_mut()
                .filter(|v| v.kind == VideoKind::Youtube && v.file.is_none())
            {
                let old = legacy_cache.join(format!(
                    "{}.mp4",
                    cache_id(&video.reference, video.start_s, video.end_s)
                ));
                if old.is_file() {
                    move_file(&old, &dir.join(VIDEO_FILE))?;
                    video.file = Some(VIDEO_FILE.to_string());
                    log::info!("Moved {} into review {id}", old.display());
                }
            }
            write_atomic(&dir.join(REVIEW_FILE), &serde_json::to_vec_pretty(&review)?)?;
            std::fs::remove_file(&path)?;
            moved += 1;
        }
        if moved > 0 {
            self.list.stale();
            log::info!(
                "Moved {moved} reviews into folders in {}",
                self.reviews.display()
            );
        }
        Ok(moved)
    }

    // ------------------------------------------------------------- videos

    /// The file a video reference plays: the one in the review folder of
    /// `review` if it has one, else the session's segment or the file
    pub fn video_path(&self, video: &VideoRef, review: Option<&str>) -> Result<PathBuf> {
        if let (Some(file), Some(id)) = (&video.file, review.filter(|r| !r.is_empty())) {
            check_file_name(file)?;
            let path = self.review_dir(id)?.join(file);
            ensure!(path.is_file(), "{file} is missing from the review {id}");
            return Ok(path);
        }
        match video.kind {
            VideoKind::Session => {
                let (session, file) = match video.reference.split_once('/') {
                    Some((session, file)) => (session, Some(file)),
                    None => (video.reference.as_str(), None),
                };
                for name in [Some(session), file].into_iter().flatten() {
                    ensure!(
                        !name.is_empty() && !name.contains(['/', '\\']) && !name.starts_with('.'),
                        "bad session reference {:?}",
                        video.reference
                    );
                }
                let dir = self.inspector.root().join(session);
                let file = match file {
                    Some(file) => file.to_string(),
                    None => SessionInfo::read(&dir)?
                        .video
                        .segments
                        .first()
                        .with_context(|| format!("{session} has no video"))?
                        .file
                        .clone(),
                };
                Ok(dir.join(file))
            }
            VideoKind::File => {
                let path = PathBuf::from(&video.reference);
                ensure!(path.is_absolute(), "give the file's full path");
                let extension = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_lowercase)
                    .unwrap_or_default();
                ensure!(
                    VIDEO_EXTENSIONS.contains(&extension.as_str()),
                    "not a video file ({})",
                    VIDEO_EXTENSIONS.join(", ")
                );
                ensure!(path.is_file(), "no file {}", path.display());
                Ok(path)
            }
            VideoKind::Youtube => {
                bail!("the video is not downloaded into a review; open it from the YouTube form")
            }
        }
    }

    /// Frame rate, duration and size of a video
    pub fn meta(&self, video: &VideoRef, review: Option<&str>) -> Result<Value> {
        let path = self.video_path(video, review)?;
        let probe = ffprobe(
            &path,
            "stream=width,height,r_frame_rate,avg_frame_rate:format=duration",
        )?;
        let stream = &probe["streams"][0];
        let rate = |key: &str| {
            let (num, den) = stream[key].as_str()?.split_once('/')?;
            let fps = num.parse::<f64>().ok()? / den.parse::<f64>().ok()?;
            (fps.is_finite() && fps > 0.0).then_some(fps)
        };
        Ok(json!({
            "fps": rate("avg_frame_rate").or_else(|| rate("r_frame_rate")),
            "duration_s": probe["format"]["duration"].as_str().and_then(|d| d.parse::<f64>().ok()),
            "width": stream["width"],
            "height": stream["height"],
            "file": path,
        }))
    }

    /// Part of a video file: the range asked for, at most [`VIDEO_CHUNK`]
    fn video(&self, video: &VideoRef, review: Option<&str>, range: Option<&str>) -> Result<Reply> {
        let path = self.video_path(video, review)?;
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let total = file.metadata()?.len();
        let (start, end) = byte_range(range, total).context("bad range")?;
        let end = end.min(start + VIDEO_CHUNK - 1);
        let mut body = vec![0; (end + 1 - start) as usize];
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut body)?;
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        Ok(Reply {
            status: StatusCode::PARTIAL_CONTENT,
            body,
            content_type: match extension.to_lowercase().as_str() {
                // Chrome plays Matroska through its WebM demuxer
                "mkv" | "webm" => "video/webm",
                "ogv" => "video/ogg",
                _ => "video/mp4",
            },
            content_range: Some(format!("bytes {start}-{end}/{total}")),
            cacheable: false,
        })
    }

    /// A small JPEG of the frame at `t_ms` of a video, from the cache or
    /// ffmpeg
    fn thumb(&self, video: &VideoRef, review: Option<&str>, t_ms: u64) -> Result<Reply> {
        let path = self.video_path(video, review)?;
        let key = (path, t_ms);
        let cached = self.thumbs.lock().unwrap().images.get(&key).cloned();
        let body = match cached {
            Some(body) => body,
            None => {
                let body = jpeg_thumb(&key.0, t_ms as f64 / 1000.0)?;
                let mut thumbs = self.thumbs.lock().unwrap();
                if thumbs.images.insert(key.clone(), body.clone()).is_none() {
                    thumbs.order.push_back(key);
                }
                while thumbs.order.len() > THUMB_CACHE {
                    if let Some(old) = thumbs.order.pop_front() {
                        thumbs.images.remove(&old);
                    }
                }
                body
            }
        };
        Ok(Reply {
            status: StatusCode::OK,
            body,
            content_type: "image/jpeg",
            content_range: None,
            cacheable: true,
        })
    }

    /// Gungee's top-down picture of a stage at a tide (his
    /// `/assets/img/map/model/<key>_<tide>.png`), fetched once into
    /// `gungee/` in the cache ([`cuttlefish::store::cache_dir`]) and served
    /// from there; never kept in the repository
    fn stage_map(&self, key: &str, tide: &str) -> Result<Reply> {
        ensure!(GUNGEE_STAGES.contains(&key), "no stage {key}");
        ensure!(GUNGEE_TIDES.contains(&tide), "no tide {tide}");
        let dir = cuttlefish::store::cache_dir().join("gungee");
        let file = dir.join(format!("{key}_{tide}.png"));
        {
            let _one = self.pictures.lock().unwrap();
            if !file.is_file() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("cannot create {}", dir.display()))?;
                let url = format!("{GUNGEE}/assets/img/map/model/{key}_{tide}.png");
                log::info!("Fetching Gungee's stage map {url}");
                cuttlefish::crawl::download(&url, &file)?;
            }
        }
        let body =
            std::fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        Ok(Reply {
            status: StatusCode::OK,
            body,
            content_type: "image/png",
            content_range: None,
            cacheable: true,
        })
    }

    /// The Salmon Run weapons and specials of Lean's datamine for the
    /// Studio's Techniques panel ([`cuttlefish::leanny::items`]: from the
    /// store's raw copies, named by its glossary, each with its Pedia term
    /// and picture); empty until the Game data (Lean) import has run
    fn game_items(&self) -> Result<Value> {
        let root = self.knowledge.root();
        let glossary = cuttlefish::store::Store::load_glossary(root)?;
        let items = cuttlefish::leanny::items(root, &glossary)?;
        Ok(json!({ "items": items }))
    }

    /// A picture of Lean's site an item shows (its `icon`, see
    /// [`cuttlefish::leanny::icon_url`]), fetched once into `leanny/` in the
    /// cache ([`cuttlefish::store::cache_dir`]) and served from there; never
    /// kept in the repository
    fn game_icon(&self, path: &str) -> Result<Reply> {
        let url =
            cuttlefish::leanny::icon_url(path).with_context(|| format!("no picture {path}"))?;
        let file = cuttlefish::store::cache_dir().join("leanny").join(path);
        {
            let _one = self.pictures.lock().unwrap();
            if !file.is_file() {
                let dir = file.parent().context("no folder")?;
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("cannot create {}", dir.display()))?;
                log::info!("Fetching Lean's picture {url}");
                cuttlefish::crawl::download(&url, &file)?;
            }
        }
        let body =
            std::fs::read(&file).with_context(|| format!("cannot read {}", file.display()))?;
        Ok(Reply {
            status: StatusCode::OK,
            body,
            content_type: "image/png",
            content_range: None,
            cacheable: true,
        })
    }

    // ---------------------------------------------------------- downloads

    /// Every download of this run
    pub fn downloads(&self) -> Value {
        let downloads: Vec<Download> = self.downloads.lock().unwrap().values().cloned().collect();
        json!({ "reviews": self.reviews, "downloads": downloads })
    }

    /// Start downloading a range of a YouTube video into a new review,
    /// unless a review has it already or it is under way; or, with `into`,
    /// into that review, which has no video yet
    pub fn download(
        &self,
        url: &str,
        start_s: Option<f64>,
        end_s: Option<f64>,
        into: Option<&str>,
    ) -> Result<Download> {
        let url = url.trim();
        ensure!(
            url.starts_with("https://") || url.starts_with("http://"),
            "give a video URL (https://…)"
        );
        for time in [start_s, end_s].into_iter().flatten() {
            ensure!(time.is_finite() && time >= 0.0, "bad time {time}");
        }
        if let (Some(start), Some(end)) = (start_s, end_s) {
            ensure!(end > start, "the range ends before it starts");
        }
        let video = VideoRef {
            kind: VideoKind::Youtube,
            reference: url.to_string(),
            start_s,
            end_s,
            file: None,
            title: None,
            channel: None,
            upload_date: None,
        };
        let running = self
            .downloads
            .lock()
            .unwrap()
            .values()
            .find(|d| {
                d.state == DownloadState::Running
                    && d.url == url
                    && d.start_s == start_s
                    && d.end_s == end_s
            })
            .cloned();
        if let Some(download) = running {
            return Ok(download);
        }
        let id = match into {
            Some(id) => {
                let stored = self.review(id)?;
                ensure!(stored.video.is_none(), "the review {id} has a video");
                id.to_string()
            }
            None => {
                if let Some(id) = self.review_with_video(&video) {
                    return Ok(Download {
                        id,
                        url: url.to_string(),
                        start_s,
                        end_s,
                        state: DownloadState::Done,
                        percent: Some(100.0),
                        message: "in a review already".to_string(),
                    });
                }
                self.new_id()
            }
        };
        let dir = self.review_dir(&id)?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;
        let download = Download {
            id: id.clone(),
            url: url.to_string(),
            start_s,
            end_s,
            state: DownloadState::Running,
            percent: None,
            message: "starting yt-dlp".to_string(),
        };
        self.downloads
            .lock()
            .unwrap()
            .insert(id.clone(), download.clone());
        let job = download.clone();
        let downloads = Arc::clone(&self.downloads);
        let writing = Arc::clone(&self.writing);
        let list = Arc::clone(&self.list);
        let review_file = dir.join(REVIEW_FILE);
        let existing = into.is_some();
        std::thread::spawn(move || {
            let result = run_ytdlp(&job, &dir, &downloads).and_then(|meta| {
                let video = VideoRef {
                    file: Some(VIDEO_FILE.to_string()),
                    title: meta.title,
                    channel: meta.channel,
                    upload_date: meta.upload_date,
                    ..video
                };
                // An existing review keeps its chat and notes
                let _writing = writing.lock().unwrap();
                let mut review = match existing {
                    true => read_review(&review_file)?,
                    false => Review {
                        video: None,
                        title: None,
                        game: None,
                        stage: None,
                        source: None,
                        comments: Vec::new(),
                        notes: Vec::new(),
                        messages: Vec::new(),
                        extra: Map::new(),
                    },
                };
                review.video = Some(video);
                write_atomic(&review_file, &serde_json::to_vec_pretty(&review)?)?;
                list.put(&job.id, Some(&review));
                Ok(())
            });
            let mut downloads = downloads.lock().unwrap();
            if let Some(download) = downloads.get_mut(&job.id) {
                match result {
                    Ok(()) => {
                        download.state = DownloadState::Done;
                        download.percent = Some(100.0);
                        download.message = "downloaded".to_string();
                    }
                    Err(e) => {
                        log::warn!("Download of {} failed: {:#}", job.url, e);
                        if existing {
                            // The review stays; only what yt-dlp left goes
                            let _ = std::fs::remove_file(dir.join(VIDEO_FILE));
                        } else {
                            // Nothing of the review was written yet
                            let _ = std::fs::remove_dir_all(&dir);
                        }
                        download.state = DownloadState::Failed;
                        download.message = format!("{e:#}");
                    }
                }
            }
        });
        Ok(download)
    }

    // --------------------------------------------------------------- chat

    /// The video context of a chat message: the frames
    /// ([`cuttlefish::sampling`]: of the moment around `t_s`, dense near it;
    /// of the range `t_s`..`t_end_s` at `fps` and `height`, more where the
    /// picture changes, or its overview for the first pass when it is long),
    /// the review's comments near it and the moment as text
    /// ([`Cuttlefish::situation`])
    fn video_context(
        &self,
        video: &VideoRef,
        t_s: f64,
        t_end_s: Option<f64>,
        review: Option<&str>,
        (fps, height): (f64, u32),
    ) -> Result<Watch> {
        let t_s = sampling::on_grid(t_s);
        let (start_s, end_s) = sampling::span(t_s, t_end_s);
        ensure!(end_s > start_s, "the range must end after it starts");
        ensure!(
            end_s - start_s <= MAX_RANGE_S + 0.05,
            "a range is at most {MAX_RANGE_S} s long"
        );
        let path = self.video_path(video, review)?;
        let source = FrameSource::open(&path, &self.frame_cache)?;
        let two_pass = t_end_s.is_some() && end_s - start_s > sampling::TWO_PASS_S;
        let frames = match t_end_s {
            None => source.frames(
                &sampling::moment_times(t_s, source.duration_s),
                sampling::MOMENT_HEIGHT,
            )?,
            Some(_) => {
                let (fps, height) = if two_pass {
                    (sampling::SCOUT_FPS, sampling::SCOUT_HEIGHT)
                } else {
                    (fps, height)
                };
                let count = sampling::frame_count(end_s - start_s, fps);
                source.frames(&source.range_times(start_s, end_s, count), height)?
            }
        };
        ensure!(!frames.is_empty(), "no frames in that range");
        let comments = match review {
            Some(id) if !id.is_empty() => self.review(id)?.comments,
            _ => Vec::new(),
        };
        let situation = self.situation(video, review, &path, t_s, (start_s, end_s), t_end_s);
        let context = VideoContext {
            video: match video.kind {
                VideoKind::Youtube => format!(
                    "{} (from {} s)",
                    video.reference,
                    video.start_s.unwrap_or(0.0)
                ),
                _ => video.reference.clone(),
            },
            start_s,
            end_s,
            frames,
            moment_s: t_end_s.is_none().then_some(t_s),
            key_moments: Vec::new(),
            comments: comments
                .iter()
                .filter(|c| c.t_s >= start_s - NEAR_S && c.t_s <= end_s + NEAR_S)
                .map(|c| ai::ExistingComment {
                    t_s: c.t_s,
                    text: c.text.clone(),
                    author: Some(c.author.clone()),
                })
                .collect(),
            situation,
        };
        Ok(Watch {
            context,
            source,
            two_pass,
        })
    }

    /// The moment as text: the controller input over `range` (a session's
    /// recording, else the Predictor's newest run on the video), the HUD at
    /// `t_s` (at both ends of a range, `t_end_s`) when the video has a wave
    /// table, and the boxes a person labelled on the session's frame at
    /// `t_s`. What cannot be read is logged and left out; `None` when
    /// nothing is known.
    fn situation(
        &self,
        video: &VideoRef,
        review: Option<&str>,
        path: &Path,
        t_s: f64,
        (start_s, end_s): (f64, f64),
        t_end_s: Option<f64>,
    ) -> Option<Situation> {
        let mut out = Situation::default();
        let session = (video.kind == VideoKind::Session && video.file.is_none())
            .then(|| {
                let name = video.reference.split('/').next()?;
                let segment = path.file_name()?.to_str()?;
                Some((name.to_string(), segment.to_string()))
            })
            .flatten();
        let input = match &session {
            Some((name, segment)) => self.recorded_input(name, segment, start_s, end_s).map(Some),
            None => self.predicted_input(video, review, start_s, end_s),
        };
        match input {
            Ok(input) => out.input = input,
            Err(e) => log::warn!("No controller input for the chat: {e:#}"),
        }
        let moments = match t_end_s {
            Some(end) => vec![t_s, end],
            None => vec![t_s],
        };
        for t in moments {
            match situation::hud_at(path, t) {
                Ok(hud) => out.hud.extend(hud),
                Err(e) => log::warn!("No HUD for the chat: {e:#}"),
            }
        }
        if let Some((name, segment)) = &session {
            match self.labelled_objects(name, segment, t_s) {
                Ok(objects) if !objects.is_empty() => {
                    out.labelled = objects;
                    out.objects_t_s = Some(t_s);
                }
                Ok(_) => {}
                Err(e) => log::warn!("No labelled objects for the chat: {e:#}"),
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// A session segment's recorded input over `start_s`..`end_s`
    fn recorded_input(
        &self,
        session: &str,
        segment: &str,
        start_s: f64,
        end_s: f64,
    ) -> Result<Input> {
        let fps = self.inspector.info(session, Some(segment))?["fps"]
            .as_f64()
            .context("no frame rate")?;
        let frames = (start_s * fps).floor() as usize..(end_s * fps).ceil() as usize;
        let mut value = self
            .inspector
            .labels(session, Some(segment), frames, None, None)?;
        let truth: Vec<Label> = serde_json::from_value(value["truth"].take())?;
        Ok(situation::summarize_input(
            &truth,
            fps,
            InputSource::Recorded,
        ))
    }

    /// The input the Predictor's newest finished run on this video predicts
    /// over `start_s`..`end_s`; `None` without a run covering it
    fn predicted_input(
        &self,
        video: &VideoRef,
        review: Option<&str>,
        start_s: f64,
        end_s: f64,
    ) -> Result<Option<Input>> {
        let Some((dir, job)) = newest_prediction(&self.predictions, video, review) else {
            return Ok(None);
        };
        let fps = job["fps"].as_f64().context("the run has no frame rate")?;
        let offset = job["frame_offset"].as_u64().unwrap_or(0);
        let frames = (start_s * fps).floor() as u64..(end_s * fps).ceil() as u64;
        let predicted: Vec<Label> = labels::read_labels(&dir.join("pred.jsonl"))?
            .into_iter()
            .map(|label| Label {
                frame: label.frame + offset,
                ..label
            })
            .filter(|label| frames.contains(&label.frame))
            .collect();
        if predicted.is_empty() {
            return Ok(None);
        }
        let model = job["checkpoint"].as_str().unwrap_or("?").to_string();
        Ok(Some(situation::summarize_input(
            &predicted,
            fps,
            InputSource::Predicted { model },
        )))
    }

    /// Boxes a person drew on the session segment's frame at `t_s`
    fn labelled_objects(&self, session: &str, segment: &str, t_s: f64) -> Result<Vec<SeenObject>> {
        let fps = self.inspector.info(session, Some(segment))?["fps"]
            .as_f64()
            .context("no frame rate")?;
        let frame = (t_s * fps).round() as u64;
        let frames = self.inspector.annotations().read(session, segment)?;
        Ok(frames
            .get(&frame)
            .map(|f| {
                f.boxes
                    .iter()
                    .filter(|b| b.by != "model")
                    .map(|b| SeenObject {
                        class: b.class.clone(),
                        x: b.x + b.w / 2.0,
                        y: b.y + b.h / 2.0,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Answer a chat message (`POST chat`, see the module doc): the reply's
    /// text, its sources and its comments as the page stores them
    fn chat(&self, body: &Value) -> Result<Value, Status> {
        let bad = |e: anyhow::Error| Status(StatusCode::BAD_REQUEST, e);
        let message = body["message"].as_str().unwrap_or_default().trim();
        if message.is_empty() {
            return Err(bad(anyhow::anyhow!("say something")));
        }
        let history: Vec<Turn> = match body.get("history") {
            Some(h) if !h.is_null() => serde_json::from_value(h.clone())
                .map_err(|e| bad(anyhow::anyhow!("bad history: {e}")))?,
            _ => Vec::new(),
        };
        let video: Option<VideoRef> = match body.get("video") {
            Some(v) if !v.is_null() => Some(
                serde_json::from_value(v.clone())
                    .map_err(|e| bad(anyhow::anyhow!("bad video: {e}")))?,
            ),
            _ => None,
        };
        // No key: say so before extracting frames
        self.knowledge.client()?;
        let review = body["review"].as_str();
        let fps = body["fps"]
            .as_f64()
            .unwrap_or(sampling::RANGE_FPS)
            .clamp(0.1, sampling::MAX_FPS);
        let height = body["height"]
            .as_u64()
            .map_or(sampling::RANGE_HEIGHT, |h| h as u32)
            .clamp(sampling::MIN_HEIGHT, sampling::MAX_HEIGHT);
        let watch = match (&video, body["t_s"].as_f64()) {
            (Some(video), Some(t_s)) => Some(
                self.video_context(video, t_s, body["t_end_s"].as_f64(), review, (fps, height))
                    .map_err(bad)?,
            ),
            _ => None,
        };
        let mut request = ChatRequest {
            history,
            message: message.to_string(),
            video: None,
        };
        // Image tokens sent, about: each call's frames at their size
        let mut images = (0, 0);
        let mut count = |frames: usize, size: (u32, u32)| {
            images.0 += frames;
            images.1 += frames as u64 * sampling::image_tokens(size);
        };
        let reply = match watch {
            None => self.knowledge.chat(&request)?,
            Some(Watch {
                context,
                source,
                two_pass,
            }) => {
                let first = context.frames.len();
                request.video = Some(context);
                if two_pass {
                    count(first, source.size_at(sampling::SCOUT_HEIGHT));
                    let (start_s, end_s) = (
                        request.video.as_ref().map_or(0.0, |v| v.start_s),
                        request.video.as_ref().map_or(0.0, |v| v.end_s),
                    );
                    let mut detail = |moments: &[KeyMoment]| {
                        let times: Vec<f64> = moments.iter().map(|m| m.t_s).collect();
                        let frames =
                            source.frames(&sampling::key_times(&times, start_s, end_s), height)?;
                        count(frames.len(), source.size_at(height));
                        Ok(frames)
                    };
                    self.knowledge.chat_in_two_passes(&request, &mut detail)?
                } else {
                    let h = if request.video.as_ref().is_some_and(|v| v.moment_s.is_some()) {
                        sampling::MOMENT_HEIGHT
                    } else {
                        height
                    };
                    count(first, source.size_at(h));
                    self.knowledge.chat(&request)?
                }
            }
        };
        if images.0 > 0 {
            log::info!(
                "Cuttlefish looked at {} frames, about {} image tokens",
                images.0,
                images.1
            );
        }
        let comments: Vec<Value> = reply.comments.into_iter().map(page_comment).collect();
        Ok(json!({
            "text": reply.text,
            "sources": reply.sources,
            "experts": reply.experts,
            "comments": comments,
            "images": {"frames": images.0, "tokens": images.1},
        }))
    }

    // ------------------------------------------------------- translations

    /// The history file, `<reviews>/translations.jsonl`
    fn translations_path(&self) -> PathBuf {
        self.reviews.join(TRANSLATIONS_FILE)
    }

    /// The history's lines, oldest first; lines that do not parse are left
    /// out
    fn translation_lines(&self) -> Result<Vec<String>> {
        let text = match std::fs::read_to_string(self.translations_path()) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("cannot read {}", self.translations_path().display())
                });
            }
        };
        Ok(text
            .lines()
            .filter(|line| serde_json::from_str::<Value>(line).is_ok())
            .map(String::from)
            .collect())
    }

    /// The translation history, newest first
    pub fn translations(&self) -> Result<Value> {
        let entries: Vec<Value> = self
            .translation_lines()?
            .iter()
            .rev()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(json!({ "file": self.translations_path(), "entries": entries }))
    }

    /// Translate (`POST translate`, see the module doc) and remember the
    /// result: the entry as written into the history
    fn translate(&self, body: &Value) -> Result<Value, Status> {
        let translation = self.knowledge.translate(
            body["text"].as_str().unwrap_or_default(),
            body["target"].as_str().unwrap_or("en"),
        )?;
        Ok(self.record_translation(&translation)?)
    }

    /// Append a translation to the history, keeping the last
    /// [`TRANSLATIONS_KEPT`]; the file is rewritten whole
    pub fn record_translation(&self, translation: &Translation) -> Result<Value> {
        let created_ms = now_ms();
        let mut entry = json!(translation);
        entry["id"] = json!(format!("t{created_ms:x}"));
        entry["created_ms"] = json!(created_ms);
        let _writing = self.writing.lock().unwrap();
        let mut lines = self.translation_lines()?;
        lines.push(entry.to_string());
        let skip = lines.len().saturating_sub(TRANSLATIONS_KEPT);
        let mut text = lines[skip..].join("\n");
        text.push('\n');
        std::fs::create_dir_all(&self.reviews)
            .with_context(|| format!("cannot create {}", self.reviews.display()))?;
        write_atomic(&self.translations_path(), text.as_bytes())?;
        Ok(entry)
    }

    /// Forget the translation history
    pub fn clear_translations(&self) -> Result<()> {
        let _writing = self.writing.lock().unwrap();
        match std::fs::remove_file(self.translations_path()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e)
                .with_context(|| format!("cannot remove {}", self.translations_path().display())),
        }
    }

    // --------------------------------------------------------------- HTTP

    /// Answer a `GET` under `/api/cuttlefish/`
    fn get(
        &self,
        path: &str,
        query: &HashMap<String, String>,
        range: Option<&str>,
    ) -> Result<Reply, Status> {
        let bad = |e: anyhow::Error| Status(StatusCode::BAD_REQUEST, e);
        let video = || video_query(query).map_err(bad);
        match path.split_once('/') {
            None if path == "reviews" => Ok(Reply::json(self.reviews().map_err(bad)?)),
            None if path == "downloads" => Ok(Reply::json(self.downloads())),
            None if path == "translations" => Ok(Reply::json(self.translations().map_err(bad)?)),
            None if path == "video" => self
                .video(&video()?, query.get("r").map(String::as_str), range)
                .map_err(bad),
            None if path == "thumb" => {
                let t_ms = query
                    .get("t_ms")
                    .and_then(|t| t.parse().ok())
                    .ok_or_else(|| bad(anyhow::anyhow!("give t_ms")))?;
                self.thumb(&video()?, query.get("r").map(String::as_str), t_ms)
                    .map_err(bad)
            }
            None if path == "stage-map" => {
                let arg = |name: &str| query.get(name).map_or("", String::as_str);
                self.stage_map(arg("stage"), arg("tide")).map_err(bad)
            }
            None if path == "game-items" => Ok(Reply::json(self.game_items().map_err(bad)?)),
            None if path == "game-icon" => self
                .game_icon(query.get("path").map_or("", String::as_str))
                .map_err(bad),
            None if path == "meta" => Ok(Reply::json(
                self.meta(&video()?, query.get("r").map(String::as_str))
                    .map_err(bad)?,
            )),
            Some(("knowledge", rest)) => Ok(Reply::json(self.knowledge.get(rest, query)?)),
            None if path == "pedia" => Ok(Reply::json(
                self.pedia.list(self.knowledge.root()).map_err(bad)?,
            )),
            None if path == "source" => Ok(Reply::json(
                self.pedia
                    .source(self.knowledge.root(), &self.reviews, query)
                    .map_err(bad)?,
            )),
            Some(("pedia", id)) => {
                let quotes = query
                    .get("quotes")
                    .and_then(|q| q.parse().ok())
                    .unwrap_or(crate::cuttlefish::pedia::QUOTES);
                let root = self.knowledge.root();
                Ok(Reply::json(
                    self.pedia
                        .entry(root, &self.reviews, &self.knowledge.catalogue(), id, quotes)
                        .map_err(bad)?,
                ))
            }
            Some(("reviews", id)) => {
                let path = self.review_path(id).map_err(bad)?;
                if !path.is_file() {
                    return Err(Status(
                        StatusCode::NOT_FOUND,
                        anyhow::anyhow!("no review {id}"),
                    ));
                }
                let review = read_review(&path).map_err(bad)?;
                if let Some(video) = &review.video {
                    self.look_up_meta(id, video);
                }
                Ok(Reply::json(json!(review)))
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {path}"),
            )),
        }
    }

    /// Answer a `POST`, `PUT` or `DELETE` under `/api/cuttlefish/`
    fn change(&self, method: &Method, path: &str, body: &[u8]) -> Result<Reply, Status> {
        let bad = |e: anyhow::Error| Status(StatusCode::BAD_REQUEST, e);
        let json_body = || -> Result<Value, Status> {
            serde_json::from_slice(body).map_err(|e| bad(anyhow::anyhow!("bad JSON: {e}")))
        };
        match (method, path.split_once('/')) {
            (&Method::PUT, Some(("reviews", id))) => {
                let review: Review = serde_json::from_slice(body)
                    .map_err(|e| bad(anyhow::anyhow!("not a review: {e}")))?;
                let saved = self.save_review(id, &review).map_err(bad)?;
                Ok(Reply::json(json!({ "id": id, "video": saved.video })))
            }
            (&Method::POST, Some(("reviews", rest))) if rest.ends_with("/copy") => {
                let id = rest.trim_end_matches("/copy");
                Ok(Reply::json(json!(self.copy_into_review(id).map_err(bad)?)))
            }
            (&Method::DELETE, Some(("reviews", id))) => {
                self.delete_review(id).map_err(bad)?;
                Ok(Reply::json(json!({ "deleted": id })))
            }
            (&Method::POST, None) if path == "download" => {
                let body = json_body()?;
                let time = |key: &str| body[key].as_f64();
                let download = self
                    .download(
                        body["url"].as_str().unwrap_or_default(),
                        time("start_s"),
                        time("end_s"),
                        body["review"].as_str().filter(|r| !r.is_empty()),
                    )
                    .map_err(bad)?;
                Ok(Reply::json(json!(download)))
            }
            (&Method::POST, None) if path == "chat" => Ok(Reply::json(self.chat(&json_body()?)?)),
            (&Method::POST, None) if path == "community-reviews" => {
                Ok(Reply::json(self.community_reviews()?))
            }
            (&Method::POST, None) if path == "translate" => {
                Ok(Reply::json(self.translate(&json_body()?)?))
            }
            (&Method::DELETE, None) if path == "translations" => {
                self.clear_translations().map_err(bad)?;
                Ok(Reply::json(json!({ "cleared": true })))
            }
            (&Method::POST, Some(("knowledge", rest))) => {
                Ok(Reply::json(self.knowledge.post(rest, body)?))
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint {method} {path}"),
            )),
        }
    }
}

/// The routes under `/api/cuttlefish/`; files and yt-dlp are handled off the
/// async workers
pub fn routes(cuttlefish: Arc<Cuttlefish>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || warp::path("api").and(warp::path("cuttlefish"));
    // Uploads and thumbnails first: they stream or answer images
    let knowledge = crate::cuttlefish::knowledge::routes(Arc::clone(&cuttlefish.knowledge));
    let reader = Arc::clone(&cuttlefish);
    let get = warp::get()
        .and(base())
        .and(warp::path::tail())
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::header::optional::<String>("range"))
        .and_then(
            move |tail: warp::path::Tail, query: HashMap<String, String>, range: Option<String>| {
                let cuttlefish = Arc::clone(&reader);
                blocking(move || cuttlefish.get(tail.as_str(), &query, range.as_deref()))
            },
        );
    let change = warp::method()
        .and(base())
        .and(warp::path::tail())
        .and(warp::body::content_length_limit(REVIEW_LIMIT))
        .and(warp::body::bytes())
        .and_then(
            move |method: Method, tail: warp::path::Tail, body: warp::hyper::body::Bytes| {
                let cuttlefish = Arc::clone(&cuttlefish);
                blocking(move || cuttlefish.change(&method, tail.as_str(), &body))
            },
        );
    knowledge.or(get).unify().or(change).unify().boxed()
}

/// Run `answer` on a blocking thread and turn it into a response
async fn blocking(
    answer: impl FnOnce() -> Result<Reply, Status> + Send + 'static,
) -> Result<Response<Vec<u8>>, core::convert::Infallible> {
    let reply = match tokio::task::spawn_blocking(answer).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(Status(status, e))) => Reply::status(status, json!({ "error": format!("{e:#}") })),
        Err(e) => Reply::status(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": e.to_string() }),
        ),
    };
    let builder = Response::builder()
        .status(reply.status)
        .header("content-type", reply.content_type);
    let builder = if reply.cacheable {
        builder.header("cache-control", "private, max-age=3600")
    } else {
        builder
    };
    let builder = match reply.content_range {
        Some(range) => builder
            .header("accept-ranges", "bytes")
            .header("content-range", range),
        None => builder,
    };
    Ok(builder.body(reply.body).unwrap())
}

/// A JPEG of the frame at `t_s` of a video, [`THUMB_HEIGHT`] lines high
fn jpeg_thumb(path: &Path, t_s: f64) -> Result<Vec<u8>> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{t_s:.3}"), "-i"])
        .arg(path)
        .args(["-an", "-sn", "-frames:v", "1", "-vf"])
        .arg(format!("scale=-2:{THUMB_HEIGHT}"))
        .args(["-f", "image2pipe", "-c:v", "mjpeg", "-q:v", "6", "-"])
        .stdin(Stdio::null())
        .output()
        .context("cannot run ffmpeg")?;
    ensure!(
        output.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    ensure!(!output.stdout.is_empty(), "no frame at {t_s:.3} s");
    Ok(output.stdout)
}

/// The JPEG images in a stream of them, split at each start of image marker
fn split_jpegs(data: &[u8]) -> Vec<&[u8]> {
    const START: [u8; 3] = [0xff, 0xd8, 0xff];
    let starts: Vec<usize> = (0..data.len().saturating_sub(2))
        .filter(|&i| data[i..i + 3] == START)
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| &data[start..starts.get(n + 1).copied().unwrap_or(data.len())])
        .collect()
}

/// A reviewer comment as the page stores comments: boxes become `rect`s,
/// sources a line at the end of the text
fn page_comment(comment: ai::AiComment) -> Value {
    let shapes: Vec<Shape> = comment
        .shapes
        .iter()
        .map(|s| Shape {
            kind: match s.kind {
                ai::ShapeKind::Box => ShapeKind::Rect,
                ai::ShapeKind::Arrow => ShapeKind::Arrow,
            },
            points: vec![[s.x0.into(), s.y0.into()], [s.x1.into(), s.y1.into()]],
            color: None,
        })
        .collect();
    let mut text = comment.text;
    if !comment.sources.is_empty() {
        let sources: Vec<String> = comment
            .sources
            .iter()
            .map(|s| {
                let title = if s.heading.is_empty() {
                    s.title.clone()
                } else {
                    format!("{} › {}", s.title, s.heading)
                };
                match &s.url {
                    Some(url) => format!("{title} ({url})"),
                    None => title,
                }
            })
            .collect();
        text = format!("{text}\n\nSources: {}", sources.join("; "));
    }
    json!({
        "t_s": comment.t_s,
        "t_end_s": comment.t_end_s,
        "text": text,
        "shapes": shapes,
    })
}

/// The folder and `run.json` of the newest finished Predictor run on a
/// video (`<results>/<video key>/<checkpoint>/`): its `play` names the same
/// kind and reference, and the same review when it names one
fn newest_prediction(
    results: &Path,
    video: &VideoRef,
    review: Option<&str>,
) -> Option<(PathBuf, Value)> {
    let kind = serde_json::to_value(video.kind).ok()?;
    let mut best: Option<(u64, PathBuf, Value)> = None;
    for key in std::fs::read_dir(results).ok()?.flatten() {
        let Ok(checkpoints) = std::fs::read_dir(key.path()) else {
            continue;
        };
        for dir in checkpoints.flatten().map(|e| e.path()) {
            let Some(job) = std::fs::read(dir.join("run.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                continue;
            };
            let play = &job["play"];
            let same = job["state"] == "done"
                && play["kind"] == kind
                && play["ref"].as_str() == Some(video.reference.as_str())
                && play["r"].as_str().is_none_or(|r| Some(r) == review)
                && dir.join("pred.jsonl").is_file();
            let finished = job["finished_ms"].as_u64().unwrap_or(0);
            if same && best.as_ref().is_none_or(|(t, ..)| finished > *t) {
                best = Some((finished, dir, job));
            }
        }
    }
    best.map(|(_, dir, job)| (dir, job))
}

/// Every review in `dir` by id, [`LIST_READERS`] files at once; folders
/// without a readable `review.json` are left out
fn read_listing(dir: &Path) -> Result<BTreeMap<String, Listed>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot list {}", dir.display())),
    };
    let ids: Vec<String> = entries
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|id| check_id(id).is_ok())
        .collect();
    let next = AtomicUsize::new(0);
    let found = Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..LIST_READERS.min(ids.len()) {
            scope.spawn(|| {
                while let Some(id) = ids.get(next.fetch_add(1, Ordering::Relaxed)) {
                    if let Some(listed) = read_listed(dir, id) {
                        found.lock().unwrap().insert(id.clone(), listed);
                    }
                }
            });
        }
    });
    Ok(found.into_inner().unwrap())
}

/// Review `id` in `dir` as the library lists it, if it has a readable
/// `review.json`
fn read_listed(dir: &Path, id: &str) -> Option<Listed> {
    let path = dir.join(id).join(REVIEW_FILE);
    let modified_ms = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let review = read_review(&path)
        .inspect_err(|e| log::warn!("Skipping review {id}: {:#}", e))
        .ok()?;
    Some(Listed::new(id, &review, modified_ms))
}

/// A review file
fn read_review(path: &Path) -> Result<Review> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("bad review {}", path.display()))
}

/// The video named by `kind`, `ref`, `start_s` and `end_s` of a query
fn video_query(query: &HashMap<String, String>) -> Result<VideoRef> {
    let text = |key: &str| query.get(key).map(String::as_str).filter(|v| !v.is_empty());
    let time = |key: &str| -> Result<Option<f64>> {
        text(key)
            .map(|v| v.parse().with_context(|| format!("bad {key}")))
            .transpose()
    };
    Ok(VideoRef {
        kind: serde_json::from_value(json!(text("kind").context("no kind given")?))
            .context("kind is session, file or youtube")?,
        reference: text("ref").context("no ref given")?.to_string(),
        start_s: time("start_s")?,
        end_s: time("end_s")?,
        file: text("file").map(String::from),
        title: None,
        channel: None,
        upload_date: None,
    })
}

/// First and last byte of a `Range: bytes=start-end` header, or of the
/// whole file without one
fn byte_range(range: Option<&str>, total: u64) -> Option<(u64, u64)> {
    let last = total.checked_sub(1)?;
    let Some(range) = range else {
        return Some((0, last));
    };
    let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
    let (start, end) = match (start, end) {
        // The last `end` bytes
        ("", end) => (total.saturating_sub(end.parse().ok()?), last),
        (start, "") => (start.parse().ok()?, last),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(last)),
    };
    (start <= end).then_some((start, end))
}

/// File stem of a URL and range in the old download cache: FNV-1a of
/// them, stable across runs; only for [`Cuttlefish::migrate`]
fn cache_id(url: &str, start_s: Option<f64>, end_s: Option<f64>) -> String {
    let key = format!("{url}|{start_s:?}|{end_s:?}");
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("yt-{hash:016x}")
}

/// `--download-sections` value of a range, such as `*60-90`
fn section(start_s: Option<f64>, end_s: Option<f64>) -> Option<String> {
    if start_s.is_none() && end_s.is_none() {
        return None;
    }
    let end = end_s.map_or("inf".to_string(), |end| end.to_string());
    Some(format!("*{}-{end}", start_s.unwrap_or(0.0)))
}

/// Percent done in a line of yt-dlp (`[download]  42.0% of …`) or of the
/// ffmpeg it runs for a range (`… time=00:00:12.50 …`, out of `span_s`)
fn progress_percent(line: &str, span_s: Option<f64>) -> Option<f64> {
    if let Some(rest) = line.trim_start().strip_prefix("[download]") {
        let percent = rest.trim_start().split('%').next()?;
        return percent.trim().parse().ok();
    }
    let time = line.split("time=").nth(1)?.split_whitespace().next()?;
    let mut seconds = 0.0;
    for part in time.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    let span = span_s.filter(|s| *s > 0.0)?;
    Some((100.0 * seconds / span).clamp(0.0, 100.0))
}

/// What yt-dlp says about a video
#[derive(Clone, Debug, Default, PartialEq)]
struct VideoMeta {
    title: Option<String>,
    channel: Option<String>,
    /// `YYYY-MM-DD`
    upload_date: Option<String>,
}

/// The video's title, channel and upload date from the line yt-dlp prints
/// for `--print "before_dl:cuttlefish-meta %(title)j %(channel)j
/// %(upload_date)j"` (three JSON values; `null` or "NA" when unknown)
fn parse_meta(line: &str) -> Option<VideoMeta> {
    let rest = line.trim().strip_prefix(META_MARK)?;
    let mut values = serde_json::Deserializer::from_str(rest)
        .into_iter::<Value>()
        .map(|v| {
            v.ok()
                .and_then(|v| v.as_str().map(String::from))
                .filter(|s| !s.is_empty() && s != "NA")
        });
    let (title, channel, date) = (values.next()?, values.next()?, values.next()?);
    let upload_date = date.map(|d| match (d.get(..4), d.get(4..6), d.get(6..8)) {
        (Some(y), Some(m), Some(day)) if d.len() == 8 => format!("{y}-{m}-{day}"),
        _ => d,
    });
    Some(VideoMeta {
        title,
        channel,
        upload_date,
    })
}

/// A video's title, channel and upload date from yt-dlp, without
/// downloading it
fn ytdlp_meta(url: &str) -> Result<VideoMeta> {
    let output = Command::new("yt-dlp")
        .args([
            "--skip-download",
            "--no-playlist",
            "--no-warnings",
            "--print",
        ])
        .arg(format!("{META_MARK}{META_FIELDS}"))
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .context("cannot run yt-dlp")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success(),
        "yt-dlp failed: {}",
        stderr
            .lines()
            .rfind(|l| l.starts_with("ERROR"))
            .unwrap_or(stderr.trim())
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(parse_meta)
        .context("yt-dlp printed nothing about the video")
}

/// Download a range with yt-dlp into `<dir>/video.mp4`, updating its
/// progress in `downloads`; answers with the video's title, channel and date
fn run_ytdlp(
    job: &Download,
    dir: &Path,
    downloads: &Mutex<BTreeMap<String, Download>>,
) -> Result<VideoMeta> {
    let mut command = Command::new("yt-dlp");
    command
        .args(["--newline", "--no-playlist", "--no-mtime", "--progress"])
        // H.264 and AAC in MP4 if offered, which every browser plays
        .args(["-f", "bv*[height<=1080]+ba/b[height<=1080]/bv*+ba/b"])
        .args([
            "-S",
            "vcodec:h264,acodec:aac",
            "--merge-output-format",
            "mp4",
        ])
        .args(["--no-simulate", "--print"])
        .arg(format!("before_dl:{META_MARK}{META_FIELDS}"))
        .arg("-o")
        .arg(dir.join("video.%(ext)s"));
    if let Some(section) = section(job.start_s, job.end_s) {
        command.args(["--download-sections", &section]);
    }
    let mut child = command
        .arg("--")
        .arg(&job.url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("cannot run yt-dlp")?;
    let span_s = match (job.start_s, job.end_s) {
        (start, Some(end)) => Some(end - start.unwrap_or(0.0)),
        _ => None,
    };
    let meta = Mutex::new(VideoMeta::default());
    let update = |line: &str| {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if let Some(found) = parse_meta(line) {
            *meta.lock().unwrap() = found;
            return;
        }
        let mut downloads = downloads.lock().unwrap();
        if let Some(download) = downloads.get_mut(&job.id) {
            if let Some(percent) = progress_percent(line, span_s) {
                download.percent = Some(percent);
            }
            download.message = line.chars().take(300).collect();
        }
    };
    // Both streams at once, so neither pipe fills up; errors come on stderr
    let stderr = child.stderr.take().context("no stderr")?;
    let errors = std::thread::scope(|scope| {
        let errors = scope.spawn(|| {
            let mut errors = Vec::new();
            for_each_line(stderr, |line| {
                if line.starts_with("ERROR") {
                    errors.push(line.to_string());
                }
                update(line);
            });
            errors
        });
        if let Some(stdout) = child.stdout.take() {
            for_each_line(stdout, update);
        }
        errors.join().unwrap_or_default()
    });
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "yt-dlp failed: {}",
            errors.last().map_or("see the lab's log", String::as_str)
        );
    }
    ensure!(
        dir.join(VIDEO_FILE).is_file(),
        "yt-dlp did not write an MP4"
    );
    Ok(meta.into_inner().unwrap())
}

/// Check a review id: a plain folder name
fn check_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 120
            && !id.starts_with('.')
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "bad review id {id:?}"
    );
    Ok(())
}

/// Check a video file name in a review folder: a plain name, so the path
/// stays inside the folder
fn check_file_name(file: &str) -> Result<()> {
    ensure!(
        !file.is_empty()
            && !file.starts_with('.')
            && !file.contains(['/', '\\'])
            && file != REVIEW_FILE,
        "bad video file {file:?}: a file name in the review folder"
    );
    Ok(())
}

/// Move a file, copying it when it is on another filesystem
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let mut partial = to.as_os_str().to_owned();
    partial.push(".part");
    std::fs::copy(from, &partial).with_context(|| format!("cannot copy {}", from.display()))?;
    std::fs::rename(&partial, to)?;
    std::fs::remove_file(from).with_context(|| format!("cannot remove {}", from.display()))
}

/// Call `f` with every line of `reader`, split at `\n` or `\r` (progress
/// lines end with `\r`)
pub(crate) fn for_each_line(mut reader: impl Read, mut f: impl FnMut(&str)) {
    let mut buffer = [0; 4096];
    let mut line = Vec::new();
    while let Ok(n) = reader.read(&mut buffer) {
        if n == 0 {
            break;
        }
        for &byte in &buffer[..n] {
            if byte == b'\n' || byte == b'\r' {
                f(&String::from_utf8_lossy(&line));
                line.clear();
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        f(&String::from_utf8_lossy(&line));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVIEW: &str = r##"{
        "video": {"kind": "youtube", "ref": "https://youtu.be/x", "start_s": 60.0, "end_s": 90.5},
        "comments": [
            {"id": "c1", "t_s": 12.5, "t_end_s": 14.0, "author": "user", "text": "Too far forward",
             "shapes": [{"kind": "arrow", "points": [[0.2, 0.3], [0.5, 0.5]], "color": "#ff5c8a"},
                        {"kind": "freehand", "points": [[0.1, 0.1], [0.2, 0.15], [0.3, 0.1]]}],
             "created_ms": 1790000000000},
            {"id": "c2", "t_s": 20.0, "author": "Cuttlefish", "text": "Nice splat",
             "created_ms": 1790000000001}
        ]
    }"##;

    /// The video of a review that has one
    fn video_of(review: &Review) -> &VideoRef {
        review.video.as_ref().expect("the review has a video")
    }

    #[test]
    fn review_round_trip() {
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        review.validate().unwrap();
        assert_eq!(video_of(&review).kind, VideoKind::Youtube);
        assert_eq!(review.comments[0].shapes[0].kind, ShapeKind::Arrow);
        assert!(review.comments[1].shapes.is_empty());
        let written = serde_json::to_value(&review).unwrap();
        let mut original: Value = serde_json::from_str(REVIEW).unwrap();
        // Shapes are always written, even when there are none
        original["comments"][1]["shapes"] = json!([]);
        assert_eq!(written, original);
        assert_eq!(written["video"]["ref"], "https://youtu.be/x");
    }

    /// A Cuttlefish over a fresh folder of `name` under the temporary folder
    fn scratch(name: &str) -> (PathBuf, Cuttlefish) {
        let dir =
            std::env::temp_dir().join(format!("procon-reviews-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let inspector = Arc::new(Inspector::new(
            Some(dir.join("sessions")),
            procon_core::recorder::Recorder::new("/tmp/procon-test-"),
            dir.join("calibration.json"),
            dir.join("annotations"),
        ));
        let cuttlefish = Cuttlefish::new(
            inspector,
            dir.join("reviews"),
            dir.join("knowledge"),
            dir.join("predictions"),
            Settings::default(),
            None,
            AutoApply::default(),
        );
        (dir, cuttlefish)
    }

    #[test]
    fn chats_find_the_newest_prediction_of_their_video() {
        let (dir, cuttlefish) = scratch("predictions");
        let video = VideoRef {
            kind: VideoKind::File,
            reference: "/videos/run.mp4".into(),
            start_s: None,
            end_s: None,
            file: None,
            title: None,
            channel: None,
            upload_date: None,
        };
        // Two runs on the file, the older one done, the newer failed; one
        // on a review of the same file
        let run = |key: &str, ckpt: &str, state: &str, finished: u64, play: Value| {
            let run_dir = dir.join("predictions").join(key).join(ckpt);
            std::fs::create_dir_all(&run_dir).unwrap();
            let job = json!({"state": state, "finished_ms": finished, "fps": 30.0,
                "frame_offset": 300, "checkpoint": ckpt, "play": play});
            std::fs::write(run_dir.join("run.json"), job.to_string()).unwrap();
            // Frames 0-59 of the prediction are 10-12 s of the video, ZR held
            let lines: String = (0..60)
                .map(|n| format!("{{\"frame\":{n},\"valid\":true,\"buttons\":[\"zr\"]}}\n"))
                .collect();
            std::fs::write(run_dir.join("pred.jsonl"), lines).unwrap();
        };
        let file = json!({"kind": "file", "ref": "/videos/run.mp4"});
        run("file-run-1", "v1", "done", 10, file.clone());
        run("file-run-1", "v2", "failed", 20, file);
        run(
            "review-x",
            "v3",
            "done",
            30,
            json!({"kind": "file", "ref": "/videos/run.mp4", "r": "x"}),
        );
        let (found, job) = newest_prediction(&dir.join("predictions"), &video, None).unwrap();
        assert!(found.ends_with("file-run-1/v1"));
        assert_eq!(job["checkpoint"], "v1");
        let (_, job) = newest_prediction(&dir.join("predictions"), &video, Some("x")).unwrap();
        assert_eq!(job["checkpoint"], "v3");
        let input = cuttlefish
            .predicted_input(&video, None, 10.5, 11.0)
            .unwrap()
            .unwrap();
        assert_eq!(input.source, InputSource::Predicted { model: "v1".into() });
        assert_eq!(input.lines, ["10.5\u{2013}11.0 s: ZR held (shooting)"]);
        // Outside the predicted frames: nothing
        assert!(
            cuttlefish
                .predicted_input(&video, None, 30.0, 31.0)
                .unwrap()
                .is_none()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reviews_are_folders() {
        let (dir, cuttlefish) = scratch("folders");
        assert_eq!(cuttlefish.reviews().unwrap()["reviews"], json!([]));
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        cuttlefish.save_review("r-1", &review).unwrap();
        assert!(dir.join("reviews/r-1/review.json").is_file());
        assert_eq!(cuttlefish.review("r-1").unwrap(), review);
        let list = cuttlefish.reviews().unwrap();
        assert_eq!(list["reviews"][0]["id"], "r-1");
        assert_eq!(list["reviews"][0]["comments"], 2);
        assert!(cuttlefish.save_review("../x", &review).is_err());

        let mut bad = review.clone();
        bad.comments[0].t_end_s = Some(1.0);
        assert!(cuttlefish.save_review("r-2", &bad).is_err());

        // A folder without review.json is not a review
        std::fs::create_dir_all(dir.join("reviews/half")).unwrap();
        assert_eq!(
            cuttlefish.reviews().unwrap()["reviews"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(cuttlefish.delete_review("half").is_err());

        // Deleting takes the folder, video and all
        std::fs::write(dir.join("reviews/r-1/video.mp4"), b"video").unwrap();
        cuttlefish.delete_review("r-1").unwrap();
        assert!(!dir.join("reviews/r-1").exists());
        assert!(cuttlefish.review("r-1").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_list_is_kept_in_memory() {
        let (dir, cuttlefish) = scratch("list");
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        cuttlefish.save_review("r-1", &review).unwrap();
        cuttlefish.warm().unwrap();
        let ids = |cuttlefish: &Cuttlefish| {
            let list = cuttlefish.reviews().unwrap();
            let ids: Vec<String> = list["reviews"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["id"].as_str().unwrap().to_string())
                .collect();
            ids
        };
        assert_eq!(ids(&cuttlefish), ["r-1"]);

        // The lab's own writes show at once, newest first
        cuttlefish.save_review("r-2", &review).unwrap();
        assert_eq!(ids(&cuttlefish), ["r-2", "r-1"]);
        cuttlefish.delete_review("r-1").unwrap();
        assert_eq!(ids(&cuttlefish), ["r-2"]);

        // Another program's review shows once the list is read again
        std::fs::create_dir_all(dir.join("reviews/r-3")).unwrap();
        std::fs::write(dir.join("reviews/r-3/review.json"), REVIEW).unwrap();
        assert_eq!(ids(&cuttlefish), ["r-2"]);
        cuttlefish.warm().unwrap();
        assert_eq!(ids(&cuttlefish).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_files_stay_in_their_review_folder() {
        for bad in [
            "",
            "../video.mp4",
            "a/b.mp4",
            "..",
            ".hidden.mp4",
            "review.json",
            "a\\b",
        ] {
            assert!(check_file_name(bad).is_err(), "{bad:?}");
        }
        check_file_name("video.mp4").unwrap();

        let (dir, cuttlefish) = scratch("paths");
        let mut review: Review = serde_json::from_str(REVIEW).unwrap();
        review.video.as_mut().unwrap().file = Some("../../etc/passwd".into());
        assert!(cuttlefish.save_review("r", &review).is_err());
        assert!(cuttlefish.video_path(video_of(&review), Some("r")).is_err());

        review.video.as_mut().unwrap().file = Some("video.mp4".into());
        cuttlefish.save_review("r", &review).unwrap();
        // Missing, then there
        assert!(cuttlefish.video_path(video_of(&review), Some("r")).is_err());
        std::fs::write(dir.join("reviews/r/video.mp4"), b"video").unwrap();
        assert_eq!(
            cuttlefish.video_path(video_of(&review), Some("r")).unwrap(),
            dir.join("reviews/r/video.mp4")
        );
        assert!(
            cuttlefish
                .video_path(video_of(&review), Some("../r"))
                .is_err()
        );
        // A YouTube range without its file does not play
        assert!(cuttlefish.video_path(video_of(&review), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn translations_are_kept_next_to_the_reviews() {
        let (dir, cuttlefish) = scratch("translations");
        assert_eq!(cuttlefish.translations().unwrap()["entries"], json!([]));
        let glossary = cuttlefish::glossary::Glossary::seed();
        let one = Translation {
            text: "熊刷".into(),
            target: "en".into(),
            term: true,
            terms: vec![glossary.lookup("熊刷").unwrap().clone()],
            translation: Some("Grizzco Roller".into()),
            explanation: None,
            needs_key: true,
        };
        let written = cuttlefish.record_translation(&one).unwrap();
        assert!(written["id"].as_str().unwrap().starts_with('t'));
        assert!(written["created_ms"].as_u64().unwrap() > 0);
        assert_eq!(written["terms"][0]["id"], "grizzco-roller");
        let file = dir.join("reviews/translations.jsonl");
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        // A broken line is skipped; the history comes newest first and is
        // bounded
        let two = Translation {
            text: "gg".into(),
            ..one.clone()
        };
        std::fs::write(
            &file,
            format!("{}\nnot json\n", std::fs::read_to_string(&file).unwrap()),
        )
        .unwrap();
        for _ in 0..TRANSLATIONS_KEPT {
            cuttlefish.record_translation(&two).unwrap();
        }
        let list = cuttlefish.translations().unwrap();
        let entries = list["entries"].as_array().unwrap();
        assert_eq!(entries.len(), TRANSLATIONS_KEPT);
        assert!(entries.iter().all(|e| e["text"] == "gg"));
        assert_eq!(list["file"], json!(file));
        cuttlefish.clear_translations().unwrap();
        assert!(!file.exists());
        cuttlefish.clear_translations().unwrap();
        assert_eq!(cuttlefish.translations().unwrap()["entries"], json!([]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chats_are_saved_with_the_review() {
        let (dir, cuttlefish) = scratch("chat");
        // A review started from the chat: no video
        let mut review: Review = serde_json::from_value(json!({
            "comments": [],
            "messages": [
                {"id": "m1", "role": "user", "text": "What is a Steelhead?", "created_ms": 1},
                {"id": "m2", "role": "assistant", "text": "A boss [S1].",
                 "sources": [{"id": "S1", "title": "Bosses", "heading": "", "source": "guide"}],
                 "created_ms": 2}
            ]
        }))
        .unwrap();
        assert!(review.video.is_none());
        assert_eq!(review.messages[1].role, Role::Assistant);
        assert_eq!(review.messages[1].sources[0].title, "Bosses");
        cuttlefish.save_review("c", &review).unwrap();
        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("reviews/c/review.json")).unwrap(),
        )
        .unwrap();
        assert!(written.get("video").is_none());
        assert!(written["messages"][0].get("sources").is_none());
        assert_eq!(cuttlefish.review("c").unwrap(), review);
        let list = cuttlefish.reviews().unwrap();
        assert_eq!(list["reviews"][0]["video"], Value::Null);
        assert_eq!(list["reviews"][0]["messages"], 2);
        assert_eq!(list["reviews"][0]["topic"], "What is a Steelhead?");
        assert!(cuttlefish.copy_into_review("c").is_err());

        // A message asked at a moment of a video attached later
        review.video = Some(VideoRef {
            kind: VideoKind::Session,
            reference: "s/video-01.mkv".into(),
            start_s: None,
            end_s: None,
            file: None,
            title: None,
            channel: None,
            upload_date: None,
        });
        review.messages.push(Message {
            id: "m3".into(),
            role: Role::User,
            text: "And here?".into(),
            t_s: Some(12.0),
            t_end_s: Some(20.0),
            sources: Vec::new(),
            experts: Vec::new(),
            comments: Vec::new(),
            created_ms: 3,
        });
        cuttlefish.save_review("c", &review).unwrap();
        assert_eq!(cuttlefish.review("c").unwrap(), review);
        review.messages[2].t_end_s = Some(1.0);
        assert!(cuttlefish.save_review("c", &review).is_err());

        // An older review without messages is written back without them
        let old: Review = serde_json::from_str(REVIEW).unwrap();
        assert!(old.messages.is_empty());
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("messages")
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flat_reviews_move_into_folders() {
        let (dir, cuttlefish) = scratch("migrate");
        let reviews = dir.join("reviews");
        let cache = dir.join("cache");
        std::fs::create_dir_all(&reviews).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        // A YouTube review whose video is in the old cache, and a session's
        std::fs::write(reviews.join("2026-09-26_00-38-20.json"), REVIEW).unwrap();
        let old = cache.join(format!(
            "{}.mp4",
            cache_id("https://youtu.be/x", Some(60.0), Some(90.5))
        ));
        std::fs::write(&old, b"video").unwrap();
        let session = r#"{"video": {"kind": "session", "ref": "s/video-01.mkv"}, "comments": []}"#;
        std::fs::write(reviews.join("s-1.json"), session).unwrap();
        std::fs::write(reviews.join("notes.txt"), b"not a review").unwrap();

        assert_eq!(cuttlefish.migrate(&cache).unwrap(), 2);
        let moved = cuttlefish.review("2026-09-26_00-38-20").unwrap();
        assert_eq!(video_of(&moved).file.as_deref(), Some("video.mp4"));
        assert_eq!(moved.comments.len(), 2);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(reviews.join("2026-09-26_00-38-20/video.mp4")).unwrap(),
            b"video"
        );
        let session = cuttlefish.review("s-1").unwrap();
        assert_eq!(video_of(&session).file, None);
        assert!(!reviews.join("s-1.json").exists());
        assert!(reviews.join("notes.txt").exists());
        // Nothing left to move
        assert_eq!(cuttlefish.migrate(&cache).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_cache_names_match() {
        // The one video of the real data in the old cache
        assert_eq!(
            cache_id(
                "https://www.youtube.com/watch?v=2W4CstCiuAE",
                Some(296.0),
                Some(356.0)
            ),
            "yt-04986bb8954dfa5e"
        );
    }

    #[test]
    fn local_files_are_copied_in() {
        let (dir, cuttlefish) = scratch("copy");
        let source = dir.join("clip.MKV");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&source, b"clip").unwrap();
        let review: Review = serde_json::from_value(json!({
            "video": {"kind": "file", "ref": source},
            "comments": []
        }))
        .unwrap();
        cuttlefish.save_review("f", &review).unwrap();
        let copied = cuttlefish.copy_into_review("f").unwrap();
        assert_eq!(video_of(&copied).file.as_deref(), Some("video.mkv"));
        assert_eq!(
            std::fs::read(dir.join("reviews/f/video.mkv")).unwrap(),
            b"clip"
        );
        assert!(source.exists());
        assert_eq!(cuttlefish.review("f").unwrap(), copied);
        assert!(cuttlefish.copy_into_review("f").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notes_are_optional() {
        // An older review has no notes, and is written back without them
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        assert!(review.notes.is_empty());
        assert!(
            serde_json::to_value(&review)
                .unwrap()
                .get("notes")
                .is_none()
        );

        let mut review = review;
        review.notes.push(Note {
            id: "n1".into(),
            author: "user".into(),
            text: "Too passive all game".into(),
            created_ms: 1790000000002,
            edited_ms: None,
            source: None,
            unplaced: Vec::new(),
        });
        let written = serde_json::to_value(&review).unwrap();
        assert_eq!(
            written["notes"],
            json!([{"id": "n1", "author": "user", "text": "Too passive all game",
                    "created_ms": 1790000000002_u64}])
        );
        let read: Review = serde_json::from_value(written).unwrap();
        assert_eq!(read, review);
    }

    #[test]
    fn imported_reviews_keep_their_origin() {
        // As `cuttlefish corpus reviews` writes one
        let text = r#"{
          "video": {"kind": "file", "ref": "/k/media/discord/1/2/300/wipe.mp4", "title": "wipe.mp4", "upload_date": "2023-06-01"},
          "title": "Cy, 2023-06-01",
          "game": "S3",
          "stage": "marooners-bay",
          "source": {"from": "discord", "url": "https://discord.com/channels/1/2/300", "video": "https://cdn.discordapp.com/attachments/2/77/wipe.mp4?ex=1"},
          "comments": [{"id": "discord-301-0", "t_s": 12.0, "author": "Dee", "text": "0:12 nobody had the Flyfish", "shapes": [], "created_ms": 1685614200000, "source": {"from": "discord", "url": "https://discord.com/channels/1/2/301"}}],
          "notes": [{"id": "discord-300-note", "author": "Cy", "text": "wipe on W3", "created_ms": 1685613600000, "source": {"from": "discord", "url": "https://discord.com/channels/1/2/300"}, "unplaced": [{"raw": "W3", "kind": "unknown", "wave": 3}]}]
        }"#;
        let review: Review = serde_json::from_str(text).unwrap();
        assert_eq!(review.title.as_deref(), Some("Cy, 2023-06-01"));
        assert_eq!(review.game, Some(Game::S3));
        assert_eq!(review.stage.as_deref(), Some("marooners-bay"));
        assert_eq!(review.source.as_ref().unwrap().from, "discord");
        assert_eq!(review.comments[0].author, "Dee");
        assert_eq!(
            review.comments[0].source.as_ref().unwrap().url,
            "https://discord.com/channels/1/2/301"
        );
        assert_eq!(review.notes[0].unplaced[0].wave, Some(3));
        review.validate().unwrap();
        // Written back with everything the corpus gave
        let written = serde_json::to_value(&review).unwrap();
        let again: Value = serde_json::from_str(text).unwrap();
        assert_eq!(written, again);
        // The listing tells the origin, title, era and stage
        let (dir, cuttlefish) = scratch("imported");
        std::fs::create_dir_all(dir.join("reviews/discord-300")).unwrap();
        std::fs::write(dir.join("reviews/discord-300/review.json"), text).unwrap();
        let listed = cuttlefish.reviews().unwrap();
        assert_eq!(listed["reviews"][0]["from"], "discord");
        assert_eq!(listed["reviews"][0]["title"], "Cy, 2023-06-01");
        assert_eq!(listed["reviews"][0]["game"], "S3");
        assert_eq!(listed["reviews"][0]["stage"], "marooners-bay");
        // The job runs on a thread; an empty knowledge folder gives no
        // VODs and no reviews
        let job = cuttlefish.community_reviews().map_err(|e| e.1).unwrap();
        assert_eq!(job["what"], "Reviews from #vod-review");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let last = loop {
            let jobs = cuttlefish.knowledge.jobs();
            let job = &jobs["jobs"][0];
            if job["state"] != "running" || std::time::Instant::now() > deadline {
                break job.clone();
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert_eq!(last["state"], "done", "{last}");
        assert!(
            last["lines"].as_array().unwrap().iter().any(|l| l
                .as_str()
                .is_some_and(|l| l.starts_with("done: 0 reviews created")
                    && l.ends_with("0 expert comments of 0 VODs: 0 documents embedded, 0 unchanged, 0 removed"))),
            "{last}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_keeps_the_video_title() {
        let (dir, cuttlefish) = scratch("meta");
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        assert!(video_of(&review).lacks_meta());
        cuttlefish.save_review("y", &review).unwrap();

        // The title comes while the page holds the review without it
        let mut stored = cuttlefish.review("y").unwrap();
        let video = stored.video.as_mut().unwrap();
        assert!(video.add_meta(VideoMeta {
            title: Some("Big Run".into()),
            channel: Some("Azu".into()),
            upload_date: None,
        }));
        assert!(!video.add_meta(video.meta()));
        write_atomic(
            &dir.join("reviews/y/review.json"),
            &serde_json::to_vec(&stored).unwrap(),
        )
        .unwrap();

        let mut edited = review.clone();
        edited.comments.pop();
        let saved = cuttlefish.save_review("y", &edited).unwrap();
        assert_eq!(video_of(&saved).title.as_deref(), Some("Big Run"));
        assert_eq!(video_of(&saved).channel.as_deref(), Some("Azu"));
        assert!(!video_of(&saved).lacks_meta());
        let read = cuttlefish.review("y").unwrap();
        assert_eq!(read, saved);
        assert_eq!(read.comments.len(), 1);

        // Another video does not inherit it
        let mut other = edited.clone();
        other.video.as_mut().unwrap().start_s = Some(1.0);
        let saved = cuttlefish.save_review("y", &other).unwrap();
        assert_eq!(video_of(&saved).title, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_meta_from_ytdlp() {
        assert_eq!(
            parse_meta(r#"cuttlefish-meta "Big Run \"tips\"" "Some Channel" "20260901""#),
            Some(VideoMeta {
                title: Some("Big Run \"tips\"".into()),
                channel: Some("Some Channel".into()),
                upload_date: Some("2026-09-01".into()),
            })
        );
        assert_eq!(
            parse_meta(r#"cuttlefish-meta "x" null "NA""#),
            Some(VideoMeta {
                title: Some("x".into()),
                ..Default::default()
            })
        );
        assert_eq!(parse_meta("[download] 50%"), None);
    }

    #[test]
    fn byte_ranges() {
        assert_eq!(byte_range(None, 10), Some((0, 9)));
        assert_eq!(byte_range(Some("bytes=0-"), 10), Some((0, 9)));
        assert_eq!(byte_range(Some("bytes=2-4"), 10), Some((2, 4)));
        assert_eq!(byte_range(Some("bytes=5-99"), 10), Some((5, 9)));
        assert_eq!(byte_range(Some("bytes=-3"), 10), Some((7, 9)));
        assert_eq!(byte_range(Some("bytes=8-2"), 10), None);
        assert_eq!(byte_range(Some("bytes=0-"), 0), None);
    }

    #[test]
    fn download_progress() {
        assert_eq!(
            progress_percent("[download]  42.5% of ~ 12.30MiB at 1MiB/s", None),
            Some(42.5)
        );
        assert_eq!(
            progress_percent(
                "frame=  300 fps=0 q=-1.0 size=1kB time=00:00:15.00 bitrate=1",
                Some(30.0)
            ),
            Some(50.0)
        );
        assert_eq!(
            progress_percent("[youtube] x: Downloading webpage", None),
            None
        );
        assert_eq!(section(Some(60.0), Some(90.5)).as_deref(), Some("*60-90.5"));
        assert_eq!(section(None, Some(30.0)).as_deref(), Some("*0-30"));
        assert_eq!(section(Some(5.0), None).as_deref(), Some("*5-inf"));
        assert_eq!(section(None, None), None);
        // Stable, and different for another range
        assert_eq!(cache_id("u", None, None), cache_id("u", None, None));
        assert_ne!(cache_id("u", Some(1.0), None), cache_id("u", None, None));
    }

    #[test]
    fn lines_split_at_carriage_returns() {
        let mut lines = Vec::new();
        for_each_line(&b"a\rb\nc"[..], |line| lines.push(line.to_string()));
        assert_eq!(lines, ["a", "b", "c"]);
    }

    #[test]
    fn jpegs_split_at_each_start() {
        let stream = [
            0xff, 0xd8, 0xff, 1, 2, 0xff, 0xd9, 0xff, 0xd8, 0xff, 3, 0xff, 0xd9,
        ];
        let parts = split_jpegs(&stream);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], &stream[..7]);
        assert_eq!(parts[1], &stream[7..]);
        assert!(split_jpegs(b"no images").is_empty());
    }

    #[test]
    fn reviewer_comments_become_page_comments() {
        let comment = ai::AiComment {
            t_s: 3.0,
            t_end_s: None,
            text: String::from("Bank the eggs"),
            shapes: vec![ai::Shape {
                kind: ai::ShapeKind::Box,
                x0: 0.25,
                y0: 0.5,
                x1: 0.75,
                y1: 1.0,
                label: None,
            }],
            sources: vec![ai::SourceRef {
                id: String::from("S1"),
                title: String::from("Guide"),
                heading: String::from("Eggs"),
                url: Some(String::from("https://example.com")),
                source: cuttlefish::doc::SourceKind::Guide,
                license: None,
                expert: None,
                doc: None,
                ordinal: None,
            }],
        };
        let page = page_comment(comment);
        assert_eq!(page["t_s"], 3.0);
        assert_eq!(
            page["text"],
            "Bank the eggs\n\nSources: Guide › Eggs (https://example.com)"
        );
        let shapes: Vec<Shape> = serde_json::from_value(page["shapes"].clone()).unwrap();
        assert_eq!(shapes[0].kind, ShapeKind::Rect);
        assert_eq!(shapes[0].points, vec![[0.25, 0.5], [0.75, 1.0]]);
    }

    #[test]
    fn stage_maps_only_for_known_stages_and_tides() {
        let (_dir, cuttlefish) = scratch("stage-map");
        // Refused before anything is fetched
        assert!(cuttlefish.stage_map("../Shakeup", "Mid").is_err());
        assert!(cuttlefish.stage_map("Shakeup", "Mid/../x").is_err());
        assert!(cuttlefish.stage_map("", "").is_err());
    }
}
