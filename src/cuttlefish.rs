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
//! carries the knowledge it cited (`sources`) and the ids of the timed
//! comments it added (`comments`). Older reviews have no `notes` or
//! `messages`; neither is written when empty, and `video` is left out of a
//! review without one.
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
//!   counts and the first message of its chat), and `fetching`, the reviews
//!   whose YouTube title is being looked up; `GET`, `PUT`, `DELETE
//!   reviews/<id>` read, write and delete one (deleting removes its folder,
//!   video included; `PUT` answers with the video as saved); `POST
//!   reviews/<id>/copy` copies a local file review's video into its folder
//! - `GET video?kind=&ref=&start_s=&end_s=&file=&r=`: the video's bytes, with HTTP
//!   ranges, `r` naming the review whose folder holds it; `GET meta?…`: its
//!   frame rate, duration and size; `GET thumb?…&t_ms=`: a small JPEG of
//!   the frame at that time, for the neighbours strip, cached in memory
//! - `POST download` with `{"url", "start_s", "end_s", "review"?}` starts
//!   downloading a YouTube range into a new review (or finds the review that
//!   has it), or with `review` into that review, which has no video yet;
//!   answers with the download, whose `id` is the review's. `GET downloads`
//!   lists this run's downloads
//! - `POST chat` with `{"message", "history": [{"role", "text"}], "video"?,
//!   "t_s"?, "t_end_s"?, "review"?}` sends a chat message to the `cuttlefish`
//!   crate with the conversation so far and, when a video and `t_s` are
//!   given, the frames of the range (or of a few seconds around `t_s`) from
//!   ffmpeg and the review's comments near it; knowledge comes from
//!   `[cuttlefish] knowledge`. Answers `{"text", "sources", "comments":
//!   [{"t_s", "t_end_s"?, "text", "shapes"}]}`; the page saves the message
//!   into the review and adds the comments as Cuttlefish's. `501` while the
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
//! - `knowledge/...`: the knowledge view (overview, search, imports,
//!   glossary, assets), see [`crate::knowledge`]; its store and embedder
//!   also serve `chat`
//!
//! Errors are `{"error": "..."}` with status 400 (404 for a missing review).

use crate::inspect::{Inspector, ffprobe};
use crate::knowledge::{Knowledge, Status, Translation, now_ms};
use crate::objects::write_atomic;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use cuttlefish::corpus_reviews::{Origin, Unplaced};
use cuttlefish::game::Game;
use cuttlefish::llm::{Role, Settings, Turn};
use cuttlefish::review::{self as ai, ChatRequest, SourceRef, VideoContext};
use cuttlefish::{corpus, corpus_reviews};
use gameplay_data::session::SessionInfo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
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

/// Seconds before and after `t_s` a question about a moment covers
const MOMENT_S: (f64, f64) = (4.0, 2.0);

/// Frames per second of video sent to the reviewer, before its own limit
const AI_FPS: f64 = 2.0;

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
    /// timers waiting for the video's wave-start table)
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

/// Reviews and the downloads under way
pub struct Cuttlefish {
    /// Sessions are read from the Inkspector's root
    inspector: Arc<Inspector>,
    reviews: PathBuf,
    downloads: Arc<Mutex<BTreeMap<String, Download>>>,
    /// Held while a review file is written
    writing: Arc<Mutex<()>>,
    lookups: Arc<Mutex<MetaLookups>>,
    thumbs: Mutex<Thumbs>,
    /// The `cuttlefish` crate's store, shared by the reviewer and the
    /// knowledge view
    knowledge: Arc<Knowledge>,
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
        settings: Settings,
        translate_model: Option<String>,
    ) -> Self {
        Self {
            inspector,
            reviews,
            downloads: Arc::default(),
            writing: Arc::default(),
            lookups: Arc::default(),
            thumbs: Mutex::default(),
            knowledge: Arc::new(Knowledge::new(knowledge, settings, translate_model)),
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
    /// message of its chat and its last change, newest first
    pub fn reviews(&self) -> Result<Value> {
        let entries = match std::fs::read_dir(&self.reviews) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({ "dir": self.reviews, "reviews": [] }));
            }
            Err(e) => {
                return Err(e).with_context(|| format!("cannot list {}", self.reviews.display()));
            }
        };
        let mut reviews: Vec<(u64, Value)> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let id = entry.file_name().to_str()?.to_string();
                if check_id(&id).is_err() {
                    return None;
                }
                let path = entry.path().join(REVIEW_FILE);
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
                if let Some(video) = &review.video {
                    self.look_up_meta(&id, video);
                }
                let topic = review
                    .messages
                    .iter()
                    .find(|m| m.role == Role::User)
                    .map(|m| m.text.chars().take(TOPIC_CHARS).collect::<String>());
                Some((
                    modified_ms,
                    json!({
                        "id": id,
                        "video": review.video,
                        "title": review.title,
                        "game": review.game,
                        "from": review.source.as_ref().map(|s| &s.from),
                        "comments": review.comments.len(),
                        "messages": review.messages.len(),
                        "topic": topic,
                        "modified_ms": modified_ms,
                    }),
                ))
            })
            .collect();
        reviews.sort_by_key(|(modified_ms, _)| core::cmp::Reverse(*modified_ms));
        let reviews: Vec<Value> = reviews.into_iter().map(|(_, review)| review).collect();
        let fetching = self.lookups.lock().unwrap().running.clone();
        Ok(json!({ "dir": self.reviews, "reviews": reviews, "fetching": fetching }))
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
        std::thread::spawn(move || {
            let result = ytdlp_meta(&url).and_then(|meta| {
                let _writing = writing.lock().unwrap();
                let mut review = read_review(&path)?;
                let video = review.video.as_mut().context("the review lost its video")?;
                if video.add_meta(meta) {
                    let title = video.title.clone();
                    write_atomic(&path, &serde_json::to_vec_pretty(&review)?)?;
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
        std::fs::remove_dir_all(&dir).with_context(|| format!("cannot delete {}", dir.display()))
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
    fn review_with_video(&self, video: &VideoRef) -> Option<(String, Review)> {
        let entries = std::fs::read_dir(&self.reviews).ok()?;
        entries.filter_map(|e| e.ok()).find_map(|entry| {
            let id = entry.file_name().to_str()?.to_string();
            let review = read_review(&entry.path().join(REVIEW_FILE)).ok()?;
            let stored = review.video.as_ref()?;
            let has_file = stored
                .file
                .as_ref()
                .is_some_and(|f| entry.path().join(f).is_file());
            (has_file && stored.same(video)).then_some((id, review))
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

    /// Build the #vod-review corpus from the knowledge folder's archive and
    /// create or update a review for every VOD whose video is on disk
    /// (`cuttlefish corpus reviews`); answers with the corpus's counts and
    /// what was written
    pub fn community_reviews(&self) -> Result<Value> {
        let knowledge = self.knowledge.root();
        let built = corpus::build(knowledge)?;
        corpus::write(knowledge, &built)?;
        let stats = corpus::Stats::of(&built, knowledge);
        let _writing = self.writing.lock().unwrap();
        let written = corpus_reviews::write(&built, knowledge, &self.reviews)?;
        Ok(json!({
            "corpus": stats.to_string(),
            "vods": stats.vods,
            "with_local_video": stats.with_local_video,
            "reviews": written.to_string(),
            "created": written.created,
            "updated": written.updated,
            "unchanged": written.unchanged,
            "dir": self.reviews,
        }))
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
                if let Some((id, _)) = self.review_with_video(&video) {
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
                        source: None,
                        comments: Vec::new(),
                        notes: Vec::new(),
                        messages: Vec::new(),
                        extra: Map::new(),
                    },
                };
                review.video = Some(video);
                write_atomic(&review_file, &serde_json::to_vec_pretty(&review)?)
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

    /// The video context of a chat message: frames of `t_s`..`t_end_s` (or
    /// of the moment around `t_s`) and the review's comments near it
    fn video_context(
        &self,
        video: &VideoRef,
        t_s: f64,
        t_end_s: Option<f64>,
        review: Option<&str>,
    ) -> Result<VideoContext> {
        let (start_s, end_s) = match t_end_s {
            Some(end) => (t_s, end),
            None => ((t_s - MOMENT_S.0).max(0.0), t_s + MOMENT_S.1),
        };
        ensure!(end_s > start_s, "the range must end after it starts");
        let path = self.video_path(video, review)?;
        let comments = match review {
            Some(id) if !id.is_empty() => self.review(id)?.comments,
            _ => Vec::new(),
        };
        Ok(VideoContext {
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
            frames: jpeg_frames(&path, start_s, end_s)?,
            comments: comments
                .iter()
                .filter(|c| c.t_s >= start_s - NEAR_S && c.t_s <= end_s + NEAR_S)
                .map(|c| ai::ExistingComment {
                    t_s: c.t_s,
                    text: c.text.clone(),
                    author: Some(c.author.clone()),
                })
                .collect(),
        })
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
        let context = match (&video, body["t_s"].as_f64()) {
            (Some(video), Some(t_s)) => Some(
                self.video_context(video, t_s, body["t_end_s"].as_f64(), review)
                    .map_err(bad)?,
            ),
            _ => None,
        };
        let request = ChatRequest {
            history,
            message: message.to_string(),
            video: context,
        };
        let reply = self.knowledge.chat(&request)?;
        let comments: Vec<Value> = reply.comments.into_iter().map(page_comment).collect();
        Ok(json!({
            "text": reply.text,
            "sources": reply.sources,
            "comments": comments,
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
            None if path == "meta" => Ok(Reply::json(
                self.meta(&video()?, query.get("r").map(String::as_str))
                    .map_err(bad)?,
            )),
            Some(("knowledge", rest)) => Ok(Reply::json(self.knowledge.get(rest, query)?)),
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
                Ok(Reply::json(self.community_reviews().map_err(bad)?))
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
    let knowledge = crate::knowledge::routes(Arc::clone(&cuttlefish.knowledge));
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

/// JPEG frames of `start_s`..`end_s` of a video at [`AI_FPS`], 720 lines
/// high
fn jpeg_frames(path: &Path, start_s: f64, end_s: f64) -> Result<Vec<ai::Frame>> {
    let span = end_s - start_s;
    let count = ((span * AI_FPS).ceil() as usize).max(1);
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &format!("{start_s:.3}"), "-t"])
        .arg(format!("{span:.3}"))
        .arg("-i")
        .arg(path)
        .args(["-vf", &format!("fps={AI_FPS},scale=-2:720"), "-frames:v"])
        .arg(count.to_string())
        .args(["-f", "image2pipe", "-c:v", "mjpeg", "-q:v", "4", "-"])
        .stdin(Stdio::null())
        .output()
        .context("cannot run ffmpeg")?;
    ensure!(
        output.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let frames: Vec<ai::Frame> = split_jpegs(&output.stdout)
        .into_iter()
        .enumerate()
        .map(|(i, jpeg)| ai::Frame {
            t_s: start_s + i as f64 / AI_FPS,
            jpeg: jpeg.to_vec(),
        })
        .collect();
    ensure!(!frames.is_empty(), "no frames in that range");
    Ok(frames)
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
            errors.last().map_or("see the studio's log", String::as_str)
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
            crate::recorder::Recorder::new("/tmp/procon-test-"),
            dir.join("calibration.json"),
            dir.join("annotations"),
        ));
        let cuttlefish = Cuttlefish::new(
            inspector,
            dir.join("reviews"),
            dir.join("knowledge"),
            Settings::default(),
            None,
        );
        (dir, cuttlefish)
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
          "source": {"from": "discord", "url": "https://discord.com/channels/1/2/300", "video": "https://cdn.discordapp.com/attachments/2/77/wipe.mp4?ex=1"},
          "comments": [{"id": "discord-301-0", "t_s": 12.0, "author": "Dee", "text": "0:12 nobody had the Flyfish", "shapes": [], "created_ms": 1685614200000, "source": {"from": "discord", "url": "https://discord.com/channels/1/2/301"}}],
          "notes": [{"id": "discord-300-note", "author": "Cy", "text": "wipe on W3", "created_ms": 1685613600000, "source": {"from": "discord", "url": "https://discord.com/channels/1/2/300"}, "unplaced": [{"raw": "W3", "kind": "unknown", "wave": 3}]}]
        }"#;
        let review: Review = serde_json::from_str(text).unwrap();
        assert_eq!(review.title.as_deref(), Some("Cy, 2023-06-01"));
        assert_eq!(review.game, Some(Game::S3));
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
        // The listing tells the origin, title and era
        let (dir, cuttlefish) = scratch("imported");
        std::fs::create_dir_all(dir.join("discord-300")).unwrap();
        std::fs::write(dir.join("discord-300/review.json"), text).unwrap();
        let listed = cuttlefish.reviews().unwrap();
        assert_eq!(listed["reviews"][0]["from"], "discord");
        assert_eq!(listed["reviews"][0]["title"], "Cy, 2023-06-01");
        assert_eq!(listed["reviews"][0]["game"], "S3");
        // Nothing to build from an empty knowledge folder: no reviews made
        let done = cuttlefish.community_reviews().unwrap();
        assert_eq!(done["vods"], 0);
        assert_eq!(done["created"], 0);
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
}
