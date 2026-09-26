//! Cuttlefish app backend: review a video with comments and drawings
//!
//! A review is one video (a recorded session's segment, a video file on this
//! machine or a range of a YouTube video) with comments at its times, each
//! with shapes drawn on the paused frame. Reviews are JSON files in the
//! `[cuttlefish] reviews` folder, `<id>.json`:
//!
//! ```json
//! {"video": {"kind": "youtube", "ref": "https://youtu.be/…", "start_s": 60, "end_s": 90},
//!  "comments": [{"id": "c1", "t_s": 12.5, "t_end_s": 14.0, "author": "user", "text": "Too far forward",
//!                "shapes": [{"kind": "arrow", "points": [[0.2, 0.3], [0.5, 0.5]], "color": "#ff5c8a"}],
//!                "created_ms": 1790000000000}]}
//! ```
//!
//! `ref` is the session folder and segment file (`<session>/<file>`), a file
//! path, or the YouTube URL. Times are seconds into the video as played: for
//! a YouTube range, from the start of the downloaded range. Shape points are
//! fractions (0–1) of the frame's width and height: two corners of a `rect`
//! or `ellipse`, tail and head of an `arrow`, every point of a `freehand`
//! line. `author` is `user`, or `Cuttlefish` for comments from the AI.
//!
//! YouTube ranges are downloaded with `yt-dlp` into the `[cuttlefish] cache`
//! folder on a thread of their own; the page polls their progress.
//!
//! Endpoints under `/api/cuttlefish/`:
//!
//! - `GET reviews`: every review, newest first; `GET`, `PUT`, `DELETE
//!   reviews/<id>` read, write and delete one
//! - `GET video?kind=&ref=&start_s=&end_s=`: the video's bytes, with HTTP
//!   ranges; `GET meta?…`: its frame rate, duration and size
//! - `POST download` with `{"url", "start_s", "end_s"}` starts downloading a
//!   YouTube range (or finds it in the cache); `GET downloads` lists them
//! - `POST ai` with `{"video", "t_s", "t_end_s"?, "question", "review"?}`
//!   asks the `cuttlefish` crate's [`Reviewer`] about the range (or a few
//!   seconds around `t_s`): frames from ffmpeg, the review's comments near
//!   it, knowledge from `[cuttlefish] knowledge`. Answers `{"comments":
//!   [{"t_s", "t_end_s"?, "text", "shapes"}]}`, which the page adds as
//!   Cuttlefish's; `501` while the reviewer cannot start (no
//!   `ANTHROPIC_API_KEY`, the only place the key is read from)
//! - `knowledge/...`: the knowledge view (stats, search, ask, translate,
//!   imports, glossary), see [`crate::knowledge`]; its store and embedder
//!   also serve `ai`
//!
//! Errors are `{"error": "..."}` with status 400 (404 for a missing review).

use crate::inspect::{Inspector, ffprobe};
use crate::knowledge::{Knowledge, Status};
use crate::objects::write_atomic;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use cuttlefish::llm::Settings;
use cuttlefish::review::{self as ai, ReviewRequest};
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

/// Seconds before and after `t_s` a question about a moment covers
const MOMENT_S: (f64, f64) = (4.0, 2.0);

/// Frames per second of video sent to the reviewer, before its own limit
const AI_FPS: f64 = 2.0;

/// Comments this far outside the range still go to the reviewer, seconds
const NEAR_S: f64 = 10.0;

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
    /// `user`, or `Cuttlefish` for the AI
    pub author: String,
    pub text: String,
    /// Drawn on the frame at `t_s`
    #[serde(default)]
    pub shapes: Vec<Shape>,
    /// When it was written, in Unix ms
    pub created_ms: u64,
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

/// A review file
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Review {
    pub video: VideoRef,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Any other keys, kept as they are
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Review {
    /// Check times and shapes are usable
    pub fn validate(&self) -> Result<()> {
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

/// A YouTube range being downloaded, or in the cache
#[derive(Clone, Debug, Serialize)]
pub struct Download {
    /// Cache file stem, from the URL and range
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

/// Reviews, the video cache and the downloads under way
pub struct Cuttlefish {
    /// Sessions are read from the Inkspector's root
    inspector: Arc<Inspector>,
    reviews: PathBuf,
    cache: PathBuf,
    downloads: Arc<Mutex<BTreeMap<String, Download>>>,
    writing: Mutex<()>,
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
        }
    }
}

impl Cuttlefish {
    pub fn new(
        inspector: Arc<Inspector>,
        reviews: PathBuf,
        cache: PathBuf,
        knowledge: PathBuf,
        settings: Settings,
    ) -> Self {
        Self {
            inspector,
            reviews,
            cache,
            downloads: Arc::default(),
            writing: Mutex::default(),
            knowledge: Arc::new(Knowledge::new(knowledge, settings)),
        }
    }

    // ------------------------------------------------------------ reviews

    fn review_path(&self, id: &str) -> Result<PathBuf> {
        ensure!(
            !id.is_empty()
                && id.len() <= 120
                && !id.starts_with('.')
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
            "bad review id {id:?}"
        );
        Ok(self.reviews.join(format!("{id}.json")))
    }

    /// Every review: its id, video, comment count and last change, newest
    /// first
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
                let path = entry.path();
                let id = path.file_stem()?.to_str()?.to_string();
                if path.extension()? != "json" || id.starts_with('.') {
                    return None;
                }
                let modified_ms = entry
                    .metadata()
                    .ok()?
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis() as u64;
                let review = read_review(&path)
                    .inspect_err(|e| log::warn!("Skipping review {id}: {:#}", e))
                    .ok()?;
                Some((
                    modified_ms,
                    json!({
                        "id": id,
                        "video": review.video,
                        "comments": review.comments.len(),
                        "modified_ms": modified_ms,
                    }),
                ))
            })
            .collect();
        reviews.sort_by_key(|(modified_ms, _)| core::cmp::Reverse(*modified_ms));
        let reviews: Vec<Value> = reviews.into_iter().map(|(_, review)| review).collect();
        Ok(json!({ "dir": self.reviews, "reviews": reviews }))
    }

    /// One review
    pub fn review(&self, id: &str) -> Result<Review> {
        read_review(&self.review_path(id)?)
    }

    /// Write a review, replacing the file atomically
    pub fn save_review(&self, id: &str, review: &Review) -> Result<()> {
        review.validate()?;
        let path = self.review_path(id)?;
        let _writing = self.writing.lock().unwrap();
        write_atomic(&path, &serde_json::to_vec_pretty(review)?)
    }

    /// Delete a review's file
    pub fn delete_review(&self, id: &str) -> Result<()> {
        let path = self.review_path(id)?;
        std::fs::remove_file(&path).with_context(|| format!("cannot delete {}", path.display()))
    }

    // ------------------------------------------------------------- videos

    /// The file a video reference plays
    pub fn video_path(&self, video: &VideoRef) -> Result<PathBuf> {
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
                let id = cache_id(&video.reference, video.start_s, video.end_s);
                self.cached(&id)
                    .context("not downloaded yet; open it from the YouTube form")
            }
        }
    }

    /// Frame rate, duration and size of a video
    pub fn meta(&self, video: &VideoRef) -> Result<Value> {
        let path = self.video_path(video)?;
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
    fn video(&self, video: &VideoRef, range: Option<&str>) -> Result<Reply> {
        let path = self.video_path(video)?;
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
        })
    }

    // ---------------------------------------------------------- downloads

    /// The finished download with this cache id, if any
    fn cached(&self, id: &str) -> Option<PathBuf> {
        let path = self.cache.join(format!("{id}.mp4"));
        path.is_file().then_some(path)
    }

    /// Every download of this run, and finished ones asked for again
    pub fn downloads(&self) -> Value {
        let downloads: Vec<Download> = self.downloads.lock().unwrap().values().cloned().collect();
        json!({ "cache": self.cache, "downloads": downloads })
    }

    /// Start downloading a range of a YouTube video, unless it is cached or
    /// already under way
    pub fn download(
        &self,
        url: &str,
        start_s: Option<f64>,
        end_s: Option<f64>,
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
        let id = cache_id(url, start_s, end_s);
        let mut downloads = self.downloads.lock().unwrap();
        if let Some(download) = downloads.get(&id)
            && download.state == DownloadState::Running
        {
            return Ok(download.clone());
        }
        let cached = self.cached(&id).is_some();
        let download = Download {
            id: id.clone(),
            url: url.to_string(),
            start_s,
            end_s,
            state: if cached {
                DownloadState::Done
            } else {
                DownloadState::Running
            },
            percent: cached.then_some(100.0),
            message: if cached {
                "in the cache"
            } else {
                "starting yt-dlp"
            }
            .to_string(),
        };
        downloads.insert(id, download.clone());
        if !cached {
            let job = download.clone();
            let cache = self.cache.clone();
            let downloads = Arc::clone(&self.downloads);
            std::thread::spawn(move || {
                let result = run_ytdlp(&job, &cache, &downloads);
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
                            download.state = DownloadState::Failed;
                            download.message = format!("{e:#}");
                        }
                    }
                }
            });
        }
        Ok(download)
    }

    // ----------------------------------------------------------------- AI

    /// Ask the reviewer about `t_s`..`t_end_s` (or the moment around `t_s`)
    /// of a video; comments as the page stores them
    fn ask(
        &self,
        video: &VideoRef,
        t_s: f64,
        t_end_s: Option<f64>,
        question: Option<&str>,
        review: Option<&str>,
    ) -> Result<Vec<Value>, Status> {
        let (start_s, end_s) = match t_end_s {
            Some(end) => (t_s, end),
            None => ((t_s - MOMENT_S.0).max(0.0), t_s + MOMENT_S.1),
        };
        if end_s <= start_s {
            return Err(anyhow::anyhow!("the range must end after it starts").into());
        }
        let path = self.video_path(video)?;
        let comments = match review {
            Some(id) if !id.is_empty() => self.review(id)?.comments,
            _ => Vec::new(),
        };
        let request = ReviewRequest {
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
            question: question
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(String::from),
            comments: comments
                .iter()
                .filter(|c| c.t_s >= start_s - NEAR_S && c.t_s <= end_s + NEAR_S)
                .map(|c| ai::ExistingComment {
                    t_s: c.t_s,
                    text: c.text.clone(),
                    author: Some(c.author.clone()),
                })
                .collect(),
        };
        Ok(self
            .knowledge
            .review(&request)?
            .into_iter()
            .map(page_comment)
            .collect())
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
            None if path == "video" => self.video(&video()?, range).map_err(bad),
            None if path == "meta" => Ok(Reply::json(self.meta(&video()?).map_err(bad)?)),
            Some(("knowledge", rest)) => Ok(Reply::json(self.knowledge.get(rest, query)?)),
            Some(("reviews", id)) => {
                let path = self.review_path(id).map_err(bad)?;
                if !path.is_file() {
                    return Err(Status(
                        StatusCode::NOT_FOUND,
                        anyhow::anyhow!("no review {id}"),
                    ));
                }
                Ok(Reply::json(json!(read_review(&path).map_err(bad)?)))
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
                self.save_review(id, &review).map_err(bad)?;
                Ok(Reply::json(json!({ "id": id })))
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
                    )
                    .map_err(bad)?;
                Ok(Reply::json(json!(download)))
            }
            (&Method::POST, None) if path == "ai" => {
                let body = json_body()?;
                let video: VideoRef = serde_json::from_value(body["video"].clone())
                    .map_err(|e| bad(anyhow::anyhow!("no video: {e}")))?;
                let t_s = body["t_s"]
                    .as_f64()
                    .ok_or_else(|| bad(anyhow::anyhow!("no t_s")))?;
                // No key: say so before extracting frames
                self.knowledge.client()?;
                let comments = self.ask(
                    &video,
                    t_s,
                    body["t_end_s"].as_f64(),
                    body["question"].as_str(),
                    body["review"].as_str(),
                )?;
                Ok(Reply::json(json!({ "comments": comments })))
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
    get.or(change).unify().boxed()
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

/// Cache file stem of a URL and range: FNV-1a of them, stable across runs
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

/// Download a range with yt-dlp into `<cache>/<id>.mp4`, updating its
/// progress in `downloads`
fn run_ytdlp(
    job: &Download,
    cache: &Path,
    downloads: &Mutex<BTreeMap<String, Download>>,
) -> Result<()> {
    std::fs::create_dir_all(cache).with_context(|| format!("cannot create {}", cache.display()))?;
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
        .arg("-o")
        .arg(cache.join(format!("{}.%(ext)s", job.id)));
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
    let update = |line: &str| {
        let line = line.trim();
        if line.is_empty() {
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
        cache.join(format!("{}.mp4", job.id)).is_file(),
        "yt-dlp did not write an MP4"
    );
    Ok(())
}

/// Call `f` with every line of `reader`, split at `\n` or `\r` (progress
/// lines end with `\r`)
fn for_each_line(mut reader: impl Read, mut f: impl FnMut(&str)) {
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

    #[test]
    fn review_round_trip() {
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        review.validate().unwrap();
        assert_eq!(review.video.kind, VideoKind::Youtube);
        assert_eq!(review.comments[0].shapes[0].kind, ShapeKind::Arrow);
        assert!(review.comments[1].shapes.is_empty());
        let written = serde_json::to_value(&review).unwrap();
        let mut original: Value = serde_json::from_str(REVIEW).unwrap();
        // Shapes are always written, even when there are none
        original["comments"][1]["shapes"] = json!([]);
        assert_eq!(written, original);
        assert_eq!(written["video"]["ref"], "https://youtu.be/x");
    }

    #[test]
    fn reviews_are_saved_listed_and_deleted() {
        let dir = std::env::temp_dir().join(format!("procon-reviews-{}", std::process::id()));
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
            dir.join("cache"),
            dir.join("knowledge"),
            Settings::default(),
        );
        assert_eq!(cuttlefish.reviews().unwrap()["reviews"], json!([]));
        let review: Review = serde_json::from_str(REVIEW).unwrap();
        cuttlefish.save_review("r-1", &review).unwrap();
        assert_eq!(cuttlefish.review("r-1").unwrap(), review);
        let list = cuttlefish.reviews().unwrap();
        assert_eq!(list["reviews"][0]["id"], "r-1");
        assert_eq!(list["reviews"][0]["comments"], 2);
        assert!(cuttlefish.save_review("../x", &review).is_err());

        let mut bad = review.clone();
        bad.comments[0].t_end_s = Some(1.0);
        assert!(cuttlefish.save_review("r-2", &bad).is_err());

        cuttlefish.delete_review("r-1").unwrap();
        assert!(cuttlefish.review("r-1").is_err());
        let _ = std::fs::remove_dir_all(&dir);
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
