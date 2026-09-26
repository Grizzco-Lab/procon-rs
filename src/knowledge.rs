//! Knowledge view of the Cuttlefish app: the `cuttlefish` crate's store
//!
//! The store (documents, chunk index, glossary) and the E5 embedder load
//! once, on the first request that needs them, and serve everything that
//! follows: searches, imports and the reviewer's retrieval ("Ask
//! Cuttlefish" in the player, and the Ask box here). The model client is
//! made per request with the key from `ANTHROPIC_API_KEY`, the only place
//! the key is read from; it is never shown or logged, and the page only
//! learns whether it is set. Searching, the glossary and imports work
//! without it.
//!
//! Imports run one at a time on a thread of their own, with a log the page
//! polls; web pages go through the crate's polite crawler (robots.txt, one
//! request per site every few seconds).
//!
//! Endpoints under `/api/cuttlefish/knowledge/`:
//!
//! - `GET stats`: documents per source kind, chunks, glossary, digest,
//!   embedder, and whether `ANTHROPIC_API_KEY` and `DISCORD_BOT_TOKEN` are
//!   set
//! - `GET search?q=&k=`: the `k` best chunks with their sources and scores
//! - `GET documents`: every document's metadata (no text)
//! - `GET glossary?q=`: the term named `q`, or the terms mentioned in it
//! - `POST ask` with `{"question", "k"?}`: the model's answer with the
//!   sources it cites; `501` without the key
//! - `POST translate` with `{"text", "to"}`: the text in language `to`;
//!   `501` without the key
//! - `POST ingest` with an [`IngestRequest`] starts an import (`409` while
//!   one runs); `GET jobs` lists this run's imports; `POST cancel` stops the
//!   current one after its document
//!
//! The crate has no way to delete a document yet, so neither has the page.

use alloc::sync::Arc;
use anyhow::{Context, Result, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use cuttlefish::discord::Bot;
use cuttlefish::doc::Document;
use cuttlefish::embed::{E5Embedder, Embedder};
use cuttlefish::ingest::{self, Meta, Web};
use cuttlefish::llm::{Client, Settings};
use cuttlefish::review::{self, AiComment, ReviewRequest};
use cuttlefish::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use warp::http::StatusCode;

/// Knowledge excerpts retrieved per question
const K: usize = 8;

/// Most search results
const MAX_K: usize = 50;

/// Log lines kept per import
const LOG_LINES: usize = 200;

/// Imported documents between two writes of the index
const SAVE_EVERY: usize = 10;

/// An error with the status it answers with
pub struct Status(pub StatusCode, pub anyhow::Error);

impl From<anyhow::Error> for Status {
    fn from(e: anyhow::Error) -> Self {
        Status(StatusCode::BAD_REQUEST, e)
    }
}

/// The store and the embedder, loaded once
pub struct Loaded {
    pub store: RwLock<Store>,
    pub embedder: E5Embedder,
}

/// Whether an environment variable holds something; its value is never
/// read further
fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.to_string_lossy().trim().is_empty())
}

/// What to import
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Source {
    /// Web pages, a sitemap or MediaWiki categories
    Web(Web),
    /// A YouTube video, playlist or channel's transcripts
    Youtube {
        url: String,
        /// Most videos; by default 50
        #[serde(default)]
        max: Option<usize>,
    },
    /// Local markdown, text, HTML or PDF files
    File {
        paths: Vec<PathBuf>,
        /// Url to cite for them
        #[serde(default)]
        url: Option<String>,
    },
    /// DiscordChatExporter JSON exports
    DiscordExport {
        paths: Vec<PathBuf>,
        /// One conversation per file (a thread or forum post)
        #[serde(default)]
        whole: bool,
    },
    /// Discord channels through the bot API (`DISCORD_BOT_TOKEN`)
    DiscordBot {
        channels: Vec<String>,
        /// Also their threads and forum posts
        #[serde(default = "yes")]
        threads: bool,
    },
}

fn yes() -> bool {
    true
}

/// An import: its source and metadata overrides
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct IngestRequest {
    #[serde(flatten)]
    pub source: Source,
    #[serde(default)]
    pub meta: Meta,
}

fn is_url(text: &str) -> bool {
    text.starts_with("https://") || text.starts_with("http://")
}

impl IngestRequest {
    /// Check the request, trimming what the page sent; answers with a short
    /// description for the job list
    pub fn check(&mut self) -> Result<String> {
        let trim = |list: &mut Vec<String>| {
            *list = list
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        };
        let check_paths = |paths: &[PathBuf]| -> Result<String> {
            ensure!(!paths.is_empty(), "give at least one file");
            for path in paths {
                ensure!(path.is_absolute(), "give full paths ({})", path.display());
            }
            let names: Vec<String> = paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            Ok(names.join(", "))
        };
        match &mut self.source {
            Source::Web(web) => {
                trim(&mut web.urls);
                trim(&mut web.categories);
                for url in web.urls.iter().chain(&web.sitemap).chain(&web.mediawiki) {
                    ensure!(is_url(url), "not a web address: {url}");
                }
                ensure!(
                    web.mediawiki.is_none() || !web.categories.is_empty(),
                    "give a category for the wiki"
                );
                ensure!(
                    web.max_pages >= 1 && web.delay_s.is_finite(),
                    "bad page limit or delay"
                );
                Ok(match (&web.mediawiki, &web.sitemap) {
                    (Some(api), _) => format!("MediaWiki {} · {api}", web.categories.join(", ")),
                    (None, Some(sitemap)) => format!("Sitemap {sitemap}"),
                    (None, None) if web.urls.len() == 1 => format!("Page {}", web.urls[0]),
                    (None, None) => {
                        ensure!(!web.urls.is_empty(), "give a url, a sitemap or a wiki");
                        format!("{} pages", web.urls.len())
                    }
                })
            }
            Source::Youtube { url, .. } => {
                *url = url.trim().to_string();
                ensure!(is_url(url), "give a YouTube address");
                Ok(format!("YouTube {url}"))
            }
            Source::File { paths, url } => {
                if let Some(u) = url {
                    ensure!(is_url(u.trim()), "not a web address: {u}");
                }
                Ok(format!("Files {}", check_paths(paths)?))
            }
            Source::DiscordExport { paths, .. } => {
                Ok(format!("Discord export {}", check_paths(paths)?))
            }
            Source::DiscordBot { channels, .. } => {
                trim(channels);
                ensure!(!channels.is_empty(), "give a channel id");
                for id in channels.iter() {
                    ensure!(
                        id.chars().all(|c| c.is_ascii_digit()),
                        "a channel id is a number: {id}"
                    );
                }
                Ok(format!("Discord channels {}", channels.join(", ")))
            }
        }
    }
}

/// State of an import
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// An import, as the page sees it
#[derive(Clone, Debug, Serialize)]
pub struct IngestJob {
    pub id: u64,
    /// What is imported, for the list
    pub what: String,
    pub state: JobState,
    /// Documents added
    pub added: usize,
    /// Items handled and to handle, once known
    pub done: usize,
    pub total: Option<usize>,
    /// The last lines of its log
    pub lines: Vec<String>,
    pub error: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
}

/// Unix time in ms
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The knowledge store behind the Cuttlefish app
pub struct Knowledge {
    /// The crate's data folder
    root: PathBuf,
    /// Model settings for questions, reviews and translations
    settings: Settings,
    loaded: Mutex<Option<Arc<Loaded>>>,
    jobs: Mutex<Vec<IngestJob>>,
    cancel: AtomicBool,
}

impl Knowledge {
    pub fn new(root: PathBuf, settings: Settings) -> Self {
        Self {
            root,
            settings,
            loaded: Mutex::default(),
            jobs: Mutex::default(),
            cancel: AtomicBool::new(false),
        }
    }

    /// The store and embedder, loaded on first use (the first time ever,
    /// the embedding model is downloaded into the data folder); an error is
    /// tried again on the next call
    pub fn loaded(&self) -> Result<Arc<Loaded>> {
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(loaded) = &*loaded {
            return Ok(Arc::clone(loaded));
        }
        log::info!("Loading the knowledge store in {}", self.root.display());
        let embedder = E5Embedder::load(&Store::models_dir(&self.root))
            .context("cannot load the embedding model")?;
        let store = Store::open(&self.root, &embedder)?;
        let opened = Arc::new(Loaded {
            store: RwLock::new(store),
            embedder,
        });
        *loaded = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// A model client with the key from `ANTHROPIC_API_KEY`; `501` without
    /// it
    pub fn client(&self) -> Result<Client, Status> {
        Client::from_env(self.settings.clone()).map_err(|e| Status(StatusCode::NOT_IMPLEMENTED, e))
    }

    /// Review a stretch of video with knowledge from the store
    pub fn review(&self, request: &ReviewRequest) -> Result<Vec<AiComment>, Status> {
        let client = self.client()?;
        let loaded = self
            .loaded()
            .map_err(|e| Status(StatusCode::NOT_IMPLEMENTED, e))?;
        let store = loaded.store.read().unwrap();
        review::review(&store, &loaded.embedder, &client, K, request)
            .map_err(|e| Status(StatusCode::BAD_GATEWAY, e))
    }

    /// Documents, chunks, glossary, digest, embedder and which keys are set
    pub fn stats(&self) -> Result<Value> {
        let loaded = self.loaded()?;
        let store = loaded.store.read().unwrap();
        let (counts, chunks) = store.stats()?;
        let sources: Vec<Value> = counts
            .iter()
            .map(|(kind, documents)| json!({ "source": kind, "documents": documents }))
            .collect();
        Ok(json!({
            "data": self.root,
            "documents": counts.iter().map(|(_, n)| n).sum::<usize>(),
            "sources": sources,
            "chunks": chunks,
            "glossary_terms": store.glossary().terms.len(),
            "own_glossary": self.root.join("glossary.toml").is_file(),
            "digest": store.digest().is_some(),
            "embedder": loaded.embedder.name(),
            "model": self.settings.model,
            "anthropic_key": is_set("ANTHROPIC_API_KEY"),
            "discord_token": is_set("DISCORD_BOT_TOKEN"),
        }))
    }

    /// The `k` chunks nearest to a query
    pub fn search(&self, query: &str, k: usize) -> Result<Value> {
        let query = query.trim();
        ensure!(!query.is_empty(), "type something to search for");
        let loaded = self.loaded()?;
        let store = loaded.store.read().unwrap();
        let hits = store.search(query, k.clamp(1, MAX_K), &loaded.embedder)?;
        Ok(json!({ "hits": hits }))
    }

    /// Every document without its text, newest first, with its chunks
    pub fn documents(&self) -> Result<Value> {
        let loaded = self.loaded()?;
        let store = loaded.store.read().unwrap();
        let mut chunks: HashMap<&str, usize> = HashMap::new();
        for entry in store.index().entries() {
            *chunks.entry(&entry.doc_id).or_default() += 1;
        }
        let mut documents = store.documents()?;
        documents.sort_by_key(|d| core::cmp::Reverse(d.fetched_at));
        let documents: Vec<Value> = documents
            .iter()
            .map(|d: &Document| {
                json!({
                    "id": d.id,
                    "source": d.source,
                    "title": d.title,
                    "url": d.url,
                    "language": d.language,
                    "license": d.license,
                    "attribution": d.attribution,
                    "fetched_at": d.fetched_at,
                    "weight": d.weight,
                    "chunks": chunks.get(d.id.as_str()).copied().unwrap_or(0),
                    "chars": d.text.chars().count(),
                })
            })
            .collect();
        Ok(json!({ "documents": documents }))
    }

    /// The term named `query`, or else the terms a text mentions; read from
    /// the data folder's glossary (or the seed) without loading the model
    pub fn glossary(&self, query: &str) -> Result<Value> {
        let glossary = Store::load_glossary(&self.root)?;
        let query = query.trim();
        let terms = match glossary.lookup(query) {
            Some(term) => vec![term],
            None if query.is_empty() => Vec::new(),
            None => glossary.find_in(query),
        };
        Ok(json!({ "terms": terms, "size": glossary.terms.len() }))
    }

    /// The model's answer to a question, with the sources it cites
    pub fn ask(&self, question: &str, k: usize) -> Result<Value, Status> {
        let question = question.trim();
        if question.is_empty() {
            return Err(anyhow::anyhow!("ask something").into());
        }
        let client = self.client()?;
        let loaded = self.loaded()?;
        let store = loaded.store.read().unwrap();
        let answer = review::ask(
            &store,
            &loaded.embedder,
            &client,
            k.clamp(1, MAX_K),
            question,
        )
        .map_err(|e| Status(StatusCode::BAD_GATEWAY, e))?;
        Ok(json!(answer))
    }

    /// `text` in language `to`, with the glossary's names
    pub fn translate(&self, text: &str, to: &str) -> Result<Value, Status> {
        let (text, to) = (text.trim(), to.trim());
        if text.is_empty() || to.is_empty() {
            return Err(anyhow::anyhow!("give a text and a language").into());
        }
        let client = self.client()?;
        let glossary = Store::load_glossary(&self.root)?;
        let translated = review::translate(&client, &glossary, text, to)
            .map_err(|e| Status(StatusCode::BAD_GATEWAY, e))?;
        Ok(json!({ "text": translated, "to": to }))
    }

    /// This run's imports, newest first
    pub fn jobs(&self) -> Value {
        let jobs = self.jobs.lock().unwrap();
        json!({ "jobs": jobs.iter().rev().collect::<Vec<_>>() })
    }

    /// Start an import on a thread of its own
    pub fn ingest(self: &Arc<Self>, mut request: IngestRequest) -> Result<IngestJob, Status> {
        let what = request.check()?;
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.iter().any(|j| j.state == JobState::Running) {
            return Err(Status(
                StatusCode::CONFLICT,
                anyhow::anyhow!("an import is running; wait for it or cancel it"),
            ));
        }
        let job = IngestJob {
            id: jobs.last().map_or(1, |j| j.id + 1),
            what,
            state: JobState::Running,
            added: 0,
            done: 0,
            total: None,
            lines: vec!["loading the knowledge store".to_string()],
            error: None,
            started_ms: now_ms(),
            finished_ms: None,
        };
        jobs.push(job.clone());
        drop(jobs);
        self.cancel.store(false, Ordering::Relaxed);
        let knowledge = Arc::clone(self);
        let id = job.id;
        std::thread::Builder::new()
            .name("ingest".to_string())
            .spawn(move || {
                let result = knowledge.run_import(id, &request);
                if let Err(e) = &result {
                    log::warn!("Import failed: {:#}", e);
                }
                knowledge.update(id, |job| {
                    job.finished_ms = Some(now_ms());
                    match result {
                        Ok(added) => {
                            job.state = JobState::Done;
                            job.done = job.total.unwrap_or(job.done);
                            job.lines.push(format!("done: {added} documents added"));
                        }
                        Err(e) if knowledge.cancel.load(Ordering::Relaxed) => {
                            job.state = JobState::Cancelled;
                            job.lines.push(format!("{e:#}"));
                        }
                        Err(e) => {
                            job.state = JobState::Failed;
                            job.error = Some(format!("{e:#}"));
                        }
                    }
                });
            })
            .context("cannot start the import")?;
        Ok(job)
    }

    /// Stop the running import after its document
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut IngestJob)) {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            f(job);
            let extra = job.lines.len().saturating_sub(LOG_LINES);
            job.lines.drain(..extra);
        }
    }

    /// Load the store, import, and write the index even when stopped
    fn run_import(&self, id: u64, request: &IngestRequest) -> Result<usize> {
        let loaded = self.loaded()?;
        let mut sink = JobSink {
            knowledge: self,
            loaded: &loaded,
            id,
            unsaved: 0,
        };
        let meta = &request.meta;
        let result = match &request.source {
            Source::Web(web) => ingest::web(&mut sink, web, meta),
            Source::Youtube { url, max } => {
                ingest::youtube(&mut sink, url, ingest::SUB_LANGS, max.unwrap_or(50), meta)
            }
            Source::File { paths, url } => {
                ingest::files(&mut sink, paths, url.as_deref().map(str::trim), meta)
            }
            Source::DiscordExport { paths, whole } => {
                ingest::discord_export(&mut sink, paths, *whole, meta)
            }
            Source::DiscordBot { channels, threads } => Bot::from_env()
                .and_then(|bot| ingest::discord_bot(&mut sink, &bot, channels, *threads, meta)),
        };
        if sink.unsaved > 0 {
            loaded.store.read().unwrap().save()?;
        }
        result
    }
}

/// Imports into the loaded store, logging into a job
struct JobSink<'a> {
    knowledge: &'a Knowledge,
    loaded: &'a Loaded,
    id: u64,
    /// Documents added since the index was last written
    unsaved: usize,
}

impl ingest::Sink for JobSink<'_> {
    fn has(&self, key: &str) -> bool {
        self.loaded.store.read().unwrap().has(key)
    }

    fn add(&mut self, doc: &Document) -> Result<usize> {
        let mut store = self.loaded.store.write().unwrap();
        let chunks = store.add(doc, &self.loaded.embedder)?;
        self.unsaved += 1;
        if self.unsaved >= SAVE_EVERY {
            store.save()?;
            self.unsaved = 0;
        }
        self.knowledge.update(self.id, |job| job.added += 1);
        Ok(chunks)
    }

    fn raw_dir(&self, kind: &str) -> PathBuf {
        self.loaded.store.read().unwrap().raw_dir(kind)
    }

    fn note(&mut self, line: &str) {
        log::info!("Import: {line}");
        let line = line.chars().take(400).collect();
        self.knowledge.update(self.id, |job| job.lines.push(line));
    }

    fn progress(&mut self, done: usize, total: usize) {
        self.knowledge.update(self.id, |job| {
            job.done = done;
            job.total = Some(total);
        });
    }

    fn cancelled(&self) -> bool {
        self.knowledge.cancel.load(Ordering::Relaxed)
    }
}

// ------------------------------------------------------------------ HTTP

impl Knowledge {
    /// Answer a `GET` under `/api/cuttlefish/knowledge/`
    pub fn get(&self, path: &str, query: &HashMap<String, String>) -> Result<Value, Status> {
        let text = |key: &str| query.get(key).map(String::as_str).unwrap_or_default();
        let k = || text("k").parse().unwrap_or(K);
        match path {
            "stats" => Ok(self.stats()?),
            "search" => Ok(self.search(text("q"), k())?),
            "documents" => Ok(self.documents()?),
            "glossary" => Ok(self.glossary(text("q"))?),
            "jobs" => Ok(self.jobs()),
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint knowledge/{path}"),
            )),
        }
    }

    /// Answer a `POST` under `/api/cuttlefish/knowledge/`
    pub fn post(self: &Arc<Self>, path: &str, body: &[u8]) -> Result<Value, Status> {
        let json_body = || -> Result<Value, Status> {
            serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad JSON: {e}").into())
        };
        match path {
            "ask" => {
                let body = json_body()?;
                let k = body["k"].as_u64().map_or(K, |k| k as usize);
                self.ask(body["question"].as_str().unwrap_or_default(), k)
            }
            "translate" => {
                let body = json_body()?;
                let text = |key: &str| body[key].as_str().unwrap_or_default().to_string();
                self.translate(&text("text"), &text("to"))
            }
            "ingest" => {
                let request: IngestRequest =
                    serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad import: {e}"))?;
                Ok(json!(self.ingest(request)?))
            }
            "cancel" => {
                self.cancel();
                Ok(self.jobs())
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST knowledge/{path}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(json: &str) -> Result<IngestRequest> {
        Ok(serde_json::from_str(json)?)
    }

    #[test]
    fn ingest_requests() {
        let mut r = request(
            r#"{"kind": "web", "urls": [" https://a.example/x ", ""], "meta": {"source": "guide", "license": "CC BY"}}"#,
        )
        .unwrap();
        assert_eq!(r.check().unwrap(), "Page https://a.example/x");
        let Source::Web(web) = &r.source else {
            panic!("not web")
        };
        assert_eq!(web.urls, ["https://a.example/x"]);
        assert_eq!(web.max_pages, 200);
        assert_eq!(r.meta.license.as_deref(), Some("CC BY"));

        let mut wiki = request(
            r#"{"kind": "web", "mediawiki": "https://wiki.example/w/api.php", "categories": ["Category:Salmon Run"], "max_pages": 20}"#,
        )
        .unwrap();
        assert!(
            wiki.check()
                .unwrap()
                .starts_with("MediaWiki Category:Salmon Run")
        );
        assert!(
            request(r#"{"kind": "web", "mediawiki": "https://w/api.php"}"#)
                .unwrap()
                .check()
                .is_err()
        );
        assert!(request(r#"{"kind": "web"}"#).unwrap().check().is_err());
        assert!(
            request(r#"{"kind": "web", "urls": ["file:///etc/passwd"]}"#)
                .unwrap()
                .check()
                .is_err()
        );

        let mut yt = request(r#"{"kind": "youtube", "url": "https://youtu.be/x"}"#).unwrap();
        assert_eq!(yt.check().unwrap(), "YouTube https://youtu.be/x");
        assert_eq!(
            yt.source,
            Source::Youtube {
                url: "https://youtu.be/x".into(),
                max: None
            }
        );

        let mut files = request(r#"{"kind": "file", "paths": ["/a/guide.md"]}"#).unwrap();
        assert_eq!(files.check().unwrap(), "Files guide.md");
        assert!(
            request(r#"{"kind": "file", "paths": ["guide.md"]}"#)
                .unwrap()
                .check()
                .is_err()
        );
        let mut export =
            request(r#"{"kind": "discord-export", "paths": ["/x/vod.json"], "whole": true}"#)
                .unwrap();
        assert!(export.check().is_ok());

        let mut bot = request(r#"{"kind": "discord-bot", "channels": ["123", " "]}"#).unwrap();
        assert!(bot.check().is_ok());
        assert_eq!(
            bot.source,
            Source::DiscordBot {
                channels: vec!["123".into()],
                threads: true
            }
        );
        assert!(
            request(r#"{"kind": "discord-bot", "channels": ["abc"]}"#)
                .unwrap()
                .check()
                .is_err()
        );
        assert!(request(r#"{"kind": "twitter"}"#).is_err());
    }

    #[test]
    fn glossary_without_the_model() {
        let dir = std::env::temp_dir().join(format!("procon-knowledge-{}", std::process::id()));
        let knowledge = Knowledge::new(dir.clone(), Settings::default());
        let found = knowledge.glossary("Steelhead").unwrap();
        assert_eq!(found["terms"][0]["id"], "steelhead");
        assert!(found["size"].as_u64().unwrap() > 10);
        let mentioned = knowledge.glossary("the Steelhead and the Stinger").unwrap();
        assert_eq!(mentioned["terms"].as_array().unwrap().len(), 2);
        assert_eq!(knowledge.glossary("").unwrap()["terms"], json!([]));
        // Nothing was written
        assert!(!dir.exists());
    }

    #[test]
    fn jobs_run_one_at_a_time() {
        let knowledge = Knowledge::new(PathBuf::from("/nonexistent"), Settings::default());
        knowledge.jobs.lock().unwrap().push(IngestJob {
            id: 1,
            what: "x".into(),
            state: JobState::Running,
            added: 0,
            done: 0,
            total: None,
            lines: Vec::new(),
            error: None,
            started_ms: 0,
            finished_ms: None,
        });
        let knowledge = Arc::new(knowledge);
        let started = knowledge
            .ingest(request(r#"{"kind": "youtube", "url": "https://youtu.be/x"}"#).unwrap());
        assert_eq!(started.err().map(|s| s.0), Some(StatusCode::CONFLICT));
        knowledge.update(1, |job| {
            job.lines = (0..LOG_LINES + 5).map(|i| i.to_string()).collect()
        });
        let jobs = knowledge.jobs();
        assert_eq!(
            jobs["jobs"][0]["lines"].as_array().unwrap().len(),
            LOG_LINES
        );
        assert_eq!(jobs["jobs"][0]["lines"][0], "5");
    }
}
