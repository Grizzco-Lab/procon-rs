//! Knowledge view of the Cuttlefish app: the `cuttlefish` crate's store
//!
//! The store (documents, chunk index, glossary) and the E5 embedder load
//! once, on the first request that needs them, and serve everything that
//! follows: searches, imports and the chat's retrieval (the chat lives in
//! the reviews, see [`crate::cuttlefish`]; this view manages the store).
//! The model client is made per request on the backend `[cuttlefish]
//! backend` names (`cuttlefish::llm::Backend`): the API with the key from
//! `ANTHROPIC_API_KEY`, the only place the key is read from, or the
//! logged-in Claude Code CLI on the user's subscription. The key is never
//! shown or logged; the page only learns which backend answers, if any.
//! Searching, the glossary and imports work without one.
//!
//! Imports run one at a time on a thread of their own, with a log the page
//! polls; web pages go through the crate's polite crawler (robots.txt, one
//! request per site every few seconds), and so do whole wiki topics and
//! sites (`cuttlefish::wiki`: a dry run counts what is in scope first; a
//! re-run fetches only what changed). The inbox (`<knowledge>/inbox/`,
//! see `cuttlefish::inbox`) takes files dropped there by hand or uploaded
//! from the page, and its import sorts them into documents, glossary terms
//! and the asset catalogue, with a report.
//!
//! Endpoints under `/api/cuttlefish/knowledge/`:
//!
//! - `GET stats`: documents per source kind, chunks, glossary, digest,
//!   embedder, the model backend (`api`, `claude-cli` or null) and whether
//!   `DISCORD_BOT_TOKEN` is set
//! - `GET model`: the model's name and backend, without loading the store
//!   (the chat asks before its first message)
//! - `GET overview`: what the store holds (documents by source and format,
//!   glossary terms by language, name tables, assets by folder), the inbox,
//!   the last import reports, and the old data folder once moved aside
//!   (`cuttlefish::store::migrate`); read from the files, without the model
//! - `GET search?q=&k=`: the `k` best chunks with their sources and scores
//! - `GET documents`: every document's metadata (no text)
//! - `POST delete` with `{"ids": [...]}`: deletes documents and their
//!   chunks
//! - `GET glossary?q=`: the term named `q`, or the terms mentioned in it
//!   (the Translate view shows a bare term's entry from it at once);
//!   `GET terms?q=`: terms whose names contain `q`, for picking one as you
//!   type
//! - `GET slang`: the user glossary (`<knowledge>/glossary-user.toml`, see
//!   `cuttlefish::slang`): the aliases taught and suggested, newest first,
//!   with their terms' names, and how many suggestions wait; `POST
//!   slang/add` with `{"term", "text", "lang", "note"}` teaches an alias,
//!   `slang/edit` with `{"id", "term"?, "text"?, "lang"?, "note"?,
//!   "status"?}` changes one (approving or rejecting a suggestion sets its
//!   status), `slang/delete` with `{"id"}` removes one. `POST
//!   slang/suggest` with `{"dry_run", "max_batches"?, "batch_chars"?,
//!   "sources"?, "language"?}` tells what a run would read (documents,
//!   characters, batches) or starts one as a job: the model (the
//!   translator's client) reads community documents batch by batch and
//!   proposes aliases, which wait as pending
//! - `GET assets?q=&folder=`: images and icons of the catalogue, with the
//!   names of their glossary terms; `GET thumb?id=` one's thumbnail
//! - `GET inbox`: files waiting in the inbox; `POST upload?path=` with the
//!   file as the body writes one there (at most [`MAX_UPLOAD`] bytes)
//! - `GET reports`, `GET report?id=`: inbox import reports
//! - `POST ingest` with an [`IngestRequest`] starts an import (`409` while
//!   a job runs); `GET jobs` lists this run's jobs (imports and slang
//!   suggestion runs); `POST cancel` stops the current one after its
//!   document or batch
//!
//! [`Knowledge::translate`] serves the Cuttlefish app's Translate view (`POST
//! /api/cuttlefish/translate`, see [`crate::cuttlefish`], which keeps the
//! history): the glossary terms a text uses, and the model's translation
//! when a backend is there.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicBool, Ordering};
use cuttlefish::assets::{self, Catalogue};
use cuttlefish::discord::Bot;
use cuttlefish::doc::{Document, SourceKind};
use cuttlefish::embed::{E5Embedder, Embedder};
use cuttlefish::glossary::{Glossary, Term};
use cuttlefish::google::{self, GoogleFile};
use cuttlefish::ingest::{self, Meta, Web};
use cuttlefish::llm::{Client, Settings};
use cuttlefish::review::{self, ChatReply, ChatRequest};
use cuttlefish::slang::{self, AliasEdit, SuggestOptions, UserGlossary};
use cuttlefish::store::{self, Store};
use cuttlefish::{inbox, lock, tables, wiki};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::SystemTime;
use warp::filters::BoxedFilter;
use warp::http::{Response, StatusCode};
use warp::hyper::body::Buf;
use warp::{Filter, Stream};

/// Knowledge excerpts retrieved per question
const K: usize = 8;

/// Most search results
const MAX_K: usize = 50;

/// Log lines kept per import
const LOG_LINES: usize = 200;

/// Imported documents between two writes of the index (each write
/// replaces the whole index in a synced folder)
const SAVE_EVERY: usize = 50;

/// Largest file uploaded into the inbox from the page
pub const MAX_UPLOAD: u64 = 4 << 30;

/// Most assets listed at once
const MAX_ASSETS: usize = 300;

/// Import reports listed in the overview
const REPORTS_SHOWN: usize = 5;

/// Longest text translated at once
const MAX_TRANSLATED: usize = 4000;

/// Most terms a search as you type answers with
const MAX_TERMS_FOUND: usize = 12;

/// A translation: what the glossary knows of the text, and the model's part
/// when a backend is there
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Translation {
    /// The text as given
    pub text: String,
    /// Language code translated into (`en`, `zh`, `ja`, ...)
    pub target: String,
    /// The text is one glossary term (or one of its names)
    pub term: bool,
    /// That term, or the terms the text mentions, in order of mention
    pub terms: Vec<Term>,
    /// The translation: a bare term's name in `target` from the glossary,
    /// else the model's; none without a model backend
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translation: Option<String>,
    /// For a bare term, the model's explanation: what it means and when a
    /// player says it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// The model was needed and no backend is there (no
    /// `ANTHROPIC_API_KEY`, no logged-in Claude Code CLI)
    pub needs_key: bool,
}

/// Translates `text` into `target` over `glossary`, with the model when a
/// `client` is given. A bare term is answered from the glossary alone (its
/// name in `target`), the model adding an explanation; a sentence gets the
/// terms it mentions and the model's translation.
fn translate_with(
    glossary: &Glossary,
    client: Option<&Client>,
    text: &str,
    target: &str,
) -> Result<Translation> {
    let text = text.trim();
    ensure!(!text.is_empty(), "type something to translate");
    ensure!(
        text.chars().count() <= MAX_TRANSLATED,
        "the text is too long: at most {MAX_TRANSLATED} characters at once"
    );
    let target = target.trim();
    ensure!(
        !target.is_empty()
            && target.len() <= 8
            && target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "bad target language {target:?}"
    );
    let lang_key = target.split('-').next().unwrap_or(target);
    let mut out = Translation {
        text: text.to_string(),
        target: target.to_string(),
        term: false,
        terms: Vec::new(),
        translation: None,
        explanation: None,
        needs_key: false,
    };
    if let Some(term) = glossary.lookup(text) {
        out.term = true;
        out.terms.push(term.clone());
        out.translation = term.name(lang_key).map(String::from);
        match client {
            Some(client) => {
                out.explanation = Some(review::explain(client, glossary, text, target)?);
                if out.translation.is_none() {
                    out.translation = Some(review::translate(client, glossary, text, target)?);
                }
            }
            None => out.needs_key = true,
        }
        return Ok(out);
    }
    out.terms = glossary.find_in(text).into_iter().cloned().collect();
    match client {
        Some(client) => {
            out.translation = Some(review::translate(client, glossary, text, target)?);
        }
        None => out.needs_key = true,
    }
    Ok(out)
}

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
    /// Everything in the inbox; with `reimport`, one file, archive or
    /// family of files (a path as a report shows it) is forgotten first and
    /// read again
    Inbox {
        #[serde(default)]
        reimport: Option<String>,
    },
    /// Web pages or a sitemap
    Web(Web),
    /// A MediaWiki topic: start pages and categories, through the API
    Wiki(wiki::Wiki),
    /// A whole site from a start address, on its host only
    Site(wiki::Site),
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
            Source::Inbox { reimport } => {
                *reimport = reimport
                    .as_deref()
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .map(String::from);
                Ok(match reimport {
                    Some(target) => format!("Inbox: {target} again"),
                    None => "Inbox".to_string(),
                })
            }
            Source::Web(web) => {
                trim(&mut web.urls);
                for url in web.urls.iter().chain(&web.sitemap) {
                    ensure!(is_url(url), "not a web address: {url}");
                }
                ensure!(
                    web.max_pages >= 1 && web.delay_s.is_finite(),
                    "bad page limit or delay"
                );
                Ok(match &web.sitemap {
                    Some(sitemap) => format!("Sitemap {sitemap}"),
                    None if web.urls.len() == 1 => format!("Page {}", web.urls[0]),
                    None => {
                        ensure!(!web.urls.is_empty(), "give a url or a sitemap");
                        format!("{} pages", web.urls.len())
                    }
                })
            }
            Source::Wiki(w) => {
                trim(&mut w.start);
                trim(&mut w.exclude);
                trim(&mut w.link_match);
                w.api = w
                    .api
                    .as_deref()
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(String::from);
                ensure!(!w.start.is_empty(), "give a start page or category");
                if let Some(api) = &w.api {
                    ensure!(is_url(api), "not a web address: {api}");
                } else {
                    ensure!(
                        w.start.iter().any(|s| is_url(s)),
                        "give the wiki's api.php, or a start page's address"
                    );
                }
                ensure!(
                    w.max_pages >= 1 && w.delay_s.is_finite(),
                    "bad page limit or delay"
                );
                Ok(format!(
                    "{}Wiki {}",
                    if w.dry_run { "Dry run: " } else { "" },
                    w.start.join(", ")
                ))
            }
            Source::Site(site) => {
                site.start = site.start.trim().to_string();
                trim(&mut site.skip);
                ensure!(is_url(&site.start), "give the site's address");
                ensure!(
                    site.max_pages >= 1 && site.delay_s.is_finite(),
                    "bad page limit or delay"
                );
                Ok(format!(
                    "{}Site {}",
                    if site.dry_run { "Dry run: " } else { "" },
                    site.start
                ))
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
    /// Documents added (a slang run: suggestions)
    pub added: usize,
    /// Items handled and to handle, once known
    pub done: usize,
    pub total: Option<usize>,
    /// The last lines of its log
    pub lines: Vec<String>,
    pub error: Option<String>,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    /// An inbox import's report id (see `GET report`) and summary
    pub report: Option<String>,
    pub summary: Option<String>,
}

/// A document's format for the overview: the extension of its file;
/// for an address `google-doc`, `google-sheet` or `google-slides`,
/// `subtitles` (videos), `messages` (Discord) or `html`; else `other`
fn format_of(d: &Document) -> String {
    match (&d.path, &d.url) {
        (Some(path), _) => path
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_lowercase())
            .filter(|ext| !ext.contains('/'))
            .unwrap_or_else(|| "file".to_string()),
        (None, Some(url)) => match (d.source, google::recognise(url)) {
            (SourceKind::Video, _) => "subtitles",
            (SourceKind::Discord | SourceKind::DiscordVodReview, _) => "messages",
            (_, Some(GoogleFile::Doc(_))) => "google-doc",
            (_, Some(GoogleFile::Sheet { .. })) => "google-sheet",
            (_, Some(GoogleFile::Slides(_))) => "google-slides",
            (_, None) => "html",
        }
        .to_string(),
        (None, None) => "other".to_string(),
    }
}

/// Unix time in ms
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The knowledge store behind the Cuttlefish app
pub struct Knowledge {
    /// The crate's data folder
    root: PathBuf,
    /// This machine's cache (embedding model, thumbnails, unpacked
    /// archives)
    cache: PathBuf,
    /// Model settings for the chat
    settings: Settings,
    /// Model settings for the translator: the same, with its own model
    /// when one is configured
    translate: Settings,
    loaded: Mutex<Option<Arc<Loaded>>>,
    jobs: Mutex<Vec<IngestJob>>,
    cancel: AtomicBool,
    /// The asset catalogue as last read, with its file's time
    catalogue: Mutex<Option<(SystemTime, Arc<Catalogue>)>>,
    /// Held while the user glossary (`glossary-user.toml`) is read and
    /// written back
    slang: Mutex<()>,
}

impl Knowledge {
    pub fn new(root: PathBuf, settings: Settings, translate_model: Option<String>) -> Self {
        let translate = Settings {
            model: translate_model.or_else(|| settings.model.clone()),
            ..settings.clone()
        };
        Self {
            root,
            cache: store::cache_dir(),
            settings,
            translate,
            loaded: Mutex::default(),
            jobs: Mutex::default(),
            cancel: AtomicBool::new(false),
            catalogue: Mutex::default(),
            slang: Mutex::default(),
        }
    }

    /// The store and embedder, loaded on first use (the first time ever,
    /// the embedding model is downloaded into the local cache); documents
    /// synced in from elsewhere are embedded then. An error is tried again
    /// on the next call.
    pub fn loaded(&self) -> Result<Arc<Loaded>> {
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(loaded) = &*loaded {
            return Ok(Arc::clone(loaded));
        }
        log::info!("Loading the knowledge store in {}", self.root.display());
        let embedder =
            E5Embedder::load(&store::models_dir()).context("cannot load the embedding model")?;
        let mut store = Store::open(&self.root, &embedder)?;
        let caught_up = store.catch_up(&embedder, &mut |line| self.catching_up(line))?;
        if caught_up.documents > 0 {
            log::info!(
                "Embedded {} documents the index lacked",
                caught_up.documents
            );
            store.save()?;
        }
        if caught_up.stopped {
            log::info!("Embedding stopped; the rest is embedded when the store opens next");
        }
        let opened = Arc::new(Loaded {
            store: RwLock::new(store),
            embedder,
        });
        *loaded = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// A line of the embedding done while the store opens: shown in the
    /// running import (a progress line replaces the one before) and logged;
    /// false once that import is cancelled, which stops the embedding
    fn catching_up(&self, line: &str) -> bool {
        let progress = line.contains(" chunks (");
        let running = {
            let jobs = self.jobs.lock().unwrap();
            jobs.iter()
                .find(|j| j.state == JobState::Running)
                .map(|j| j.id)
        };
        if !progress {
            log::info!("{line}");
        }
        let Some(id) = running else {
            return true;
        };
        self.update(id, |job| {
            if progress && job.lines.last().is_some_and(|l| l.contains(" chunks (")) {
                job.lines.pop();
            }
            job.lines.push(line.to_string());
        });
        !self.cancel.load(Ordering::Relaxed)
    }

    /// A model client for the chat on the configured backend (the API with
    /// the key from `ANTHROPIC_API_KEY`, or the Claude Code CLI); `501`
    /// without one
    pub fn client(&self) -> Result<Client, Status> {
        Client::from_env(self.settings.clone()).map_err(|e| Status(StatusCode::NOT_IMPLEMENTED, e))
    }

    /// The translator's client, with its own model when one is configured
    fn translate_client(&self) -> Result<Client, Status> {
        Client::from_env(self.translate.clone()).map_err(|e| Status(StatusCode::NOT_IMPLEMENTED, e))
    }

    /// Answer a chat message with knowledge from the store
    pub fn chat(&self, request: &ChatRequest) -> Result<ChatReply, Status> {
        let client = self.client()?;
        let loaded = self
            .loaded()
            .map_err(|e| Status(StatusCode::NOT_IMPLEMENTED, e))?;
        let store = loaded.store.read().unwrap();
        review::chat(&store, &loaded.embedder, &client, K, request)
            .map_err(|e| Status(StatusCode::BAD_GATEWAY, e))
    }

    /// The model's name (null for the backend's default) and the backend
    /// that would answer now (`api`, `claude-cli` or null), without the
    /// store and without any secret
    pub fn model(&self) -> Value {
        json!({
            "model": self.settings.model,
            "backend": self.settings.detect(),
        })
    }

    /// Documents, chunks, glossary, digest, embedder, the model backend and
    /// whether the Discord token is set
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
            "backend": self.settings.detect(),
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
                    "path": d.path,
                    "format": format_of(d),
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

    /// Deletes documents and their chunks, and writes the index
    pub fn delete(&self, ids: &[String]) -> Result<Value> {
        ensure!(!ids.is_empty(), "no documents given");
        let _lock = lock::acquire(&self.root, "procon studio delete")?;
        let loaded = self.loaded()?;
        let mut store = loaded.store.write().unwrap();
        let mut deleted = Vec::new();
        for id in ids {
            if store.delete(id)? {
                deleted.push(id.clone());
            }
        }
        store.save()?;
        log::info!("Deleted {} documents", deleted.len());
        Ok(json!({ "deleted": deleted }))
    }

    /// What the store holds, read from its files (the model need not be
    /// loaded): documents by source and format, glossary terms by language,
    /// name tables, assets by folder, the inbox and the last imports
    pub fn overview(&self) -> Result<Value> {
        let docs = store::read_documents(&self.root)?;
        let mut sources: BTreeMap<String, usize> = BTreeMap::new();
        let mut formats: BTreeMap<String, usize> = BTreeMap::new();
        for d in &docs {
            let source = serde_json::to_value(d.source)?;
            *sources
                .entry(source.as_str().unwrap_or_default().to_string())
                .or_default() += 1;
            *formats.entry(format_of(d)).or_default() += 1;
        }
        let glossary = Store::load_glossary(&self.root)?;
        let tables: Vec<Value> = tables::load_all(&self.root)
            .iter()
            .map(|t| {
                json!({
                    "source": t.source,
                    "terms": t.terms.len(),
                    "languages": t.languages,
                    "files": t.files.len(),
                    "note": t.note,
                })
            })
            .collect();
        let catalogue = self.catalogue();
        let reports: Vec<Value> = inbox::reports(&self.root, REPORTS_SHOWN)
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "started": r.started,
                    "files": r.files,
                    "summary": r.summary(),
                })
            })
            .collect();
        Ok(json!({
            "data": self.root,
            "documents": {
                "total": docs.len(),
                "sources": sources,
                "formats": formats,
            },
            "glossary": {
                "terms": glossary.terms.len(),
                "imported": glossary.terms.iter().filter(|t| !t.from.is_empty()).count(),
                "languages": glossary.languages(),
                "tables": tables,
            },
            "assets": {
                "total": catalogue.assets.len(),
                "linked": catalogue.assets.iter().filter(|a| a.term.is_some()).count(),
                "folders": catalogue.folders(),
            },
            "inbox": inbox::pending(&self.root),
            "reports": reports,
            "moved_aside": store::moved_aside(&store::legacy_root()),
        }))
    }

    /// The asset catalogue, read again when its file changed
    fn catalogue(&self) -> Arc<Catalogue> {
        let time = std::fs::metadata(self.root.join("assets.json"))
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut cached = self.catalogue.lock().unwrap();
        match &*cached {
            Some((t, c)) if *t == time => Arc::clone(c),
            _ => {
                let c = Arc::new(Catalogue::load(&self.root));
                *cached = Some((time, Arc::clone(&c)));
                c
            }
        }
    }

    /// Images and icons whose name, path or term matches `query`, in
    /// `folder` (and below) when given, with their term's names
    pub fn assets(&self, query: &str, folder: &str) -> Result<Value> {
        let catalogue = self.catalogue();
        let glossary = Store::load_glossary(&self.root)?;
        let query = query.trim().to_lowercase();
        let mut shown = Vec::new();
        let mut matching = 0;
        for a in &catalogue.assets {
            if !folder.is_empty()
                && a.folder != folder
                && !a.folder.starts_with(&format!("{folder}/"))
            {
                continue;
            }
            let term = a.term.as_deref().and_then(|id| glossary.lookup(id));
            let names = term.map(|t| &t.forms);
            if !query.is_empty() {
                let hay = format!(
                    "{} {} {}",
                    a.path,
                    a.term.as_deref().unwrap_or_default(),
                    names
                        .map(|n| n.values().flatten().cloned().collect::<Vec<_>>().join(" "))
                        .unwrap_or_default()
                )
                .to_lowercase();
                if !hay.contains(&query) {
                    continue;
                }
            }
            matching += 1;
            if shown.len() < MAX_ASSETS {
                shown.push(json!({
                    "id": a.id,
                    "path": a.path,
                    "name": a.name,
                    "folder": a.folder,
                    "format": a.format,
                    "bytes": a.bytes,
                    "width": a.width,
                    "height": a.height,
                    "term": a.term,
                    "names": names,
                }));
            }
        }
        Ok(json!({
            "total": catalogue.assets.len(),
            "matching": matching,
            "assets": shown,
            "folders": catalogue.folders(),
        }))
    }

    /// An asset's thumbnail and its media type
    pub fn thumbnail(&self, id: &str) -> Result<(Vec<u8>, &'static str), Status> {
        let catalogue = self.catalogue();
        let asset = catalogue
            .assets
            .iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Status(StatusCode::NOT_FOUND, anyhow::anyhow!("no asset {id}")))?;
        Ok(assets::thumbnail(
            &self.root.join(inbox::INBOX),
            &self.cache,
            asset,
        )?)
    }

    /// Writes an upload into the inbox from its chunks (`None` at the end;
    /// without it, the upload was broken off and nothing is kept); returns
    /// the bytes written
    fn write_upload(
        &self,
        rel: &str,
        mut chunks: tokio::sync::mpsc::Receiver<Option<Vec<u8>>>,
    ) -> Result<u64> {
        let target = inbox::upload_path(&self.root, rel)?;
        let dir = target.parent().context("no folder")?;
        std::fs::create_dir_all(dir)?;
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        let part = dir.join(format!(".{name}.upload"));
        let written = (|| -> Result<u64> {
            let mut file = std::fs::File::create(&part)?;
            let mut bytes = 0;
            loop {
                match chunks.blocking_recv() {
                    Some(Some(chunk)) => {
                        file.write_all(&chunk)?;
                        bytes += chunk.len() as u64;
                    }
                    Some(None) => break,
                    None => anyhow::bail!("the upload was broken off"),
                }
            }
            file.sync_all()?;
            std::fs::rename(&part, &target)?;
            Ok(bytes)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&part);
        }
        written
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

    /// Translates `text` into `target` (see [`Translation`]): the glossary
    /// answers without the store or the model; the model's part needs a
    /// backend, and its failure is a `502`
    pub fn translate(&self, text: &str, target: &str) -> Result<Translation, Status> {
        let glossary = Store::load_glossary(&self.root)?;
        let client = self.translate_client().ok();
        translate_with(&glossary, client.as_ref(), text, target).map_err(|e| {
            let status = if client.is_some() {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::BAD_REQUEST
            };
            Status(status, e)
        })
    }

    /// This run's imports, newest first
    pub fn jobs(&self) -> Value {
        let jobs = self.jobs.lock().unwrap();
        json!({ "jobs": jobs.iter().rev().collect::<Vec<_>>() })
    }

    /// Start an import on a thread of its own
    pub fn ingest(self: &Arc<Self>, mut request: IngestRequest) -> Result<IngestJob, Status> {
        let what = request.check()?;
        let dry_run = match &request.source {
            Source::Wiki(w) => w.dry_run,
            Source::Site(s) => s.dry_run,
            _ => false,
        };
        self.start_job(what, move |knowledge, id| {
            let added = knowledge.run_import(id, &request)?;
            Ok(if dry_run {
                "dry run done: nothing stored".to_string()
            } else {
                format!("done: {added} documents added")
            })
        })
    }

    /// Start a job (an import, a slang suggestion run) on a thread of its
    /// own, one at a time; `run` answers the job's last line
    fn start_job(
        self: &Arc<Self>,
        what: String,
        run: impl FnOnce(&Knowledge, u64) -> Result<String> + Send + 'static,
    ) -> Result<IngestJob, Status> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.iter().any(|j| j.state == JobState::Running) {
            return Err(Status(
                StatusCode::CONFLICT,
                anyhow::anyhow!("a job is running; wait for it or cancel it"),
            ));
        }
        let job = IngestJob {
            id: jobs.last().map_or(1, |j| j.id + 1),
            what,
            state: JobState::Running,
            added: 0,
            done: 0,
            total: None,
            lines: Vec::new(),
            error: None,
            started_ms: now_ms(),
            finished_ms: None,
            report: None,
            summary: None,
        };
        jobs.push(job.clone());
        drop(jobs);
        self.cancel.store(false, Ordering::Relaxed);
        let knowledge = Arc::clone(self);
        let id = job.id;
        std::thread::Builder::new()
            .name("knowledge-job".to_string())
            .spawn(move || {
                let result = run(&knowledge, id);
                if let Err(e) = &result {
                    log::warn!("Job failed: {:#}", e);
                }
                knowledge.update(id, |job| {
                    job.finished_ms = Some(now_ms());
                    match result {
                        Ok(last) => {
                            job.state = JobState::Done;
                            job.done = job.total.unwrap_or(job.done);
                            job.lines.push(last);
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
            .context("cannot start the job")?;
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

    /// Take the store's write lock (not for a dry run, which stores
    /// nothing), load the store, import, and write the index even when
    /// stopped
    fn run_import(&self, id: u64, request: &IngestRequest) -> Result<usize> {
        let dry_run = matches!(&request.source, Source::Wiki(w) if w.dry_run)
            || matches!(&request.source, Source::Site(s) if s.dry_run);
        let _lock = if dry_run {
            None
        } else {
            Some(lock::acquire(&self.root, "procon studio import")?)
        };
        self.update(id, |job| {
            job.lines.push("loading the knowledge store".to_string())
        });
        let loaded = self.loaded()?;
        let mut sink = JobSink {
            knowledge: self,
            loaded: &loaded,
            id,
            unsaved: 0,
        };
        let meta = &request.meta;
        let result = match &request.source {
            Source::Inbox { reimport } => {
                let result = match reimport {
                    Some(target) => {
                        inbox::reimport(&mut sink, &self.root, &self.cache, meta, target)
                    }
                    None => inbox::import(&mut sink, &self.root, &self.cache, meta),
                };
                loaded.store.write().unwrap().reload_glossary()?;
                result.map(|report| {
                    self.update(id, |job| {
                        job.report = Some(report.id.clone());
                        job.summary = Some(report.summary());
                    });
                    report.count("document")
                })
            }
            Source::Web(web) => ingest::web(&mut sink, web, meta),
            Source::Wiki(w) => wiki::wiki(&mut sink, w, meta),
            Source::Site(site) => wiki::site(&mut sink, site, meta),
            Source::Youtube { url, max } => {
                ingest::youtube(&mut sink, url, ingest::SUB_LANGS, max.unwrap_or(50), meta)
            }
            Source::File { paths, url } => {
                let url = url.as_deref().map(str::trim);
                let result = ingest::files(&mut sink, &self.root, &self.cache, paths, url, meta);
                // Folders and archives may have brought name tables
                loaded.store.write().unwrap().reload_glossary()?;
                result
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

    fn revision(&self, key: &str) -> Option<u64> {
        self.loaded.store.read().unwrap().document(key)?.revision
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

    fn delete(&mut self, id: &str) -> Result<bool> {
        let mut store = self.loaded.store.write().unwrap();
        let removed = store.delete(id)?;
        self.unsaved += removed as usize;
        Ok(removed)
    }

    fn raw_dir(&self, kind: &str) -> PathBuf {
        self.loaded.store.read().unwrap().raw_dir(kind)
    }

    fn add_table(&mut self, table: &tables::Table) -> Result<()> {
        let mut store = self.loaded.store.write().unwrap();
        tables::save(store.root(), table)?;
        store.reload_glossary()
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

// ----------------------------------------------------------------- slang

/// A slang suggestion run as the page asks for it: a dry run tells what it
/// would read
#[derive(Deserialize)]
struct SuggestRequest {
    #[serde(default)]
    dry_run: bool,
    #[serde(flatten)]
    options: SuggestOptions,
}

impl Knowledge {
    /// The loaded store reads its glossary again, after the user glossary
    /// changed
    fn glossary_changed(&self) -> Result<()> {
        let loaded = self.loaded.lock().unwrap().clone();
        if let Some(loaded) = loaded {
            loaded.store.write().unwrap().reload_glossary()?;
        }
        Ok(())
    }

    /// Terms whose names contain `query` (see `Glossary::search`), for
    /// picking one as you type
    pub fn terms(&self, query: &str) -> Result<Value> {
        let glossary = Store::load_glossary(&self.root)?;
        Ok(json!({ "terms": glossary.search(query, MAX_TERMS_FOUND) }))
    }

    /// The user glossary: every alias taught or suggested, newest first,
    /// each with its term's official names (`term_forms`, null when the
    /// term is gone), and how many suggestions wait
    pub fn slang(&self) -> Result<Value> {
        let glossary = Store::load_glossary(&self.root)?;
        let user = UserGlossary::load(&self.root)?;
        let aliases: Vec<Value> = user
            .aliases
            .iter()
            .rev()
            .map(|a| {
                let term = a.find(&glossary).map(|i| &glossary.terms[i]);
                let mut value = json!(a);
                value["term_forms"] = json!(term.map(|t| &t.forms));
                value
            })
            .collect();
        Ok(json!({
            "file": self.root.join(slang::FILE),
            "aliases": aliases,
            "pending": user.pending().count(),
            "backend": self.translate.detect(),
        }))
    }

    /// Reads the user glossary, lets `change` edit it against the glossary
    /// and writes it back
    fn change_slang(
        &self,
        change: impl FnOnce(&Glossary, &mut UserGlossary) -> Result<Value>,
    ) -> Result<Value> {
        let answer = {
            let _held = self.slang.lock().unwrap();
            let glossary = Store::load_glossary(&self.root)?;
            let mut user = UserGlossary::load(&self.root)?;
            let answer = change(&glossary, &mut user)?;
            user.save(&self.root)?;
            answer
        };
        self.glossary_changed()?;
        Ok(answer)
    }

    /// `POST slang/add`, `slang/edit`, `slang/delete`
    fn post_slang(&self, action: &str, body: &Value) -> Result<Value> {
        let text = |key: &str| body[key].as_str().unwrap_or_default();
        match action {
            "add" => self.change_slang(|g, user| {
                let alias = user.add(
                    g,
                    text("term"),
                    text("text"),
                    text("lang"),
                    text("note"),
                    now_ms(),
                )?;
                log::info!("Slang: {} ({}) → {}", alias.text, alias.lang, alias.term);
                Ok(json!(alias))
            }),
            "edit" => {
                let edit: AliasEdit = serde_json::from_value(body.clone())
                    .map_err(|e| anyhow::anyhow!("bad edit: {e}"))?;
                self.change_slang(|g, user| Ok(json!(user.edit(g, text("id"), &edit)?)))
            }
            "delete" => self.change_slang(|_, user| {
                ensure!(user.remove(text("id")), "no alias {}", text("id"));
                Ok(json!({ "deleted": text("id") }))
            }),
            _ => bail!("no endpoint POST knowledge/slang/{action}"),
        }
    }

    /// `POST slang/suggest`: what a run would read (`dry_run`), or the run
    /// started as a job
    pub fn suggest(self: &Arc<Self>, body: &[u8]) -> Result<Value, Status> {
        let request: SuggestRequest =
            serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad request: {e}"))?;
        let docs = store::read_documents(&self.root)?;
        let plan = {
            let _held = self.slang.lock().unwrap();
            slang::plan(&docs, &UserGlossary::load(&self.root)?, &request.options)
        };
        let summary = json!({
            "documents": plan.documents,
            "chars": plan.chars,
            "batches_total": plan.batches_total,
            "batches": plan.batches.len(),
            "chars_run": plan.chars_run,
            "backend": self.translate.detect(),
        });
        if request.dry_run {
            return Ok(summary);
        }
        if plan.batches.is_empty() {
            return Err(anyhow::anyhow!("no community text left to read").into());
        }
        let client = self.translate_client()?;
        let what = format!(
            "Slang suggestions: {} of {} batches",
            plan.batches.len(),
            plan.batches_total
        );
        let job = self.start_job(what, move |knowledge, id| {
            knowledge.run_suggest(id, &client, &plan.batches)
        })?;
        Ok(json!({ "plan": summary, "job": job }))
    }

    /// Sends the batches to the model one by one; after each, its new
    /// suggestions and what it read go into the user glossary
    fn run_suggest(&self, id: u64, client: &Client, batches: &[slang::Batch]) -> Result<String> {
        let total = batches.len();
        self.update(id, |job| job.total = Some(total));
        let mut added = 0;
        for (i, batch) in batches.iter().enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("cancelled after {i} of {total} batches: {added} suggestions");
            }
            let glossary = Store::load_glossary(&self.root)?;
            let reply = client.send(&slang::suggest_prompt(&glossary, batch))?;
            let n = {
                let _held = self.slang.lock().unwrap();
                let mut user = UserGlossary::load(&self.root)?;
                let found =
                    slang::parse_candidates(&reply.text, &glossary, &user, batch, now_ms())?;
                let n = user.take(found, batch);
                user.save(&self.root)?;
                n
            };
            added += n;
            let titles: String = batch
                .pieces
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                .chars()
                .take(160)
                .collect();
            self.update(id, |job| {
                job.done = i + 1;
                job.added = added;
                job.lines
                    .push(format!("batch {}/{total}: {n} new ({titles})", i + 1));
            });
        }
        Ok(format!("done: {added} suggestions to review"))
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
            "model" => Ok(self.model()),
            "search" => Ok(self.search(text("q"), k())?),
            "documents" => Ok(self.documents()?),
            "overview" => Ok(self.overview()?),
            "glossary" => Ok(self.glossary(text("q"))?),
            "terms" => Ok(self.terms(text("q"))?),
            "slang" => Ok(self.slang()?),
            "assets" => Ok(self.assets(text("q"), text("folder"))?),
            "inbox" => Ok(json!(inbox::pending(&self.root))),
            "reports" => Ok(json!({ "reports": inbox::reports(&self.root, REPORTS_SHOWN) })),
            "report" => Ok(json!(inbox::report(&self.root, text("id"))?)),
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
            "ingest" => {
                let request: IngestRequest =
                    serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("bad import: {e}"))?;
                Ok(json!(self.ingest(request)?))
            }
            "cancel" => {
                self.cancel();
                Ok(self.jobs())
            }
            "delete" => {
                let body = json_body()?;
                let ids: Vec<String> = serde_json::from_value(body["ids"].clone())
                    .map_err(|e| anyhow::anyhow!("bad ids: {e}"))?;
                Ok(self.delete(&ids)?)
            }
            "slang/suggest" => self.suggest(body),
            _ if path.starts_with("slang/") => {
                Ok(self.post_slang(&path["slang/".len()..], &json_body()?)?)
            }
            _ => Err(Status(
                StatusCode::NOT_FOUND,
                anyhow::anyhow!("no endpoint POST knowledge/{path}"),
            )),
        }
    }
}

/// A JSON error answer
fn error_response(status: StatusCode, e: &anyhow::Error) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(
            json!({ "error": format!("{e:#}") })
                .to_string()
                .into_bytes(),
        )
        .unwrap()
}

/// Streams an upload's body to the writer thread
async fn upload(
    knowledge: Arc<Knowledge>,
    rel: String,
    mut body: impl Stream<Item = Result<impl Buf, warp::Error>> + Unpin,
) -> Response<Vec<u8>> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let path = rel.clone();
    let writer = tokio::task::spawn_blocking(move || knowledge.write_upload(&path, rx));
    let mut complete = true;
    while let Some(chunk) = body.next().await {
        let sent = match chunk {
            Ok(mut buf) => {
                tx.send(Some(buf.copy_to_bytes(buf.remaining()).to_vec()))
                    .await
            }
            Err(_) => {
                complete = false;
                break;
            }
        };
        if sent.is_err() {
            // The writer stopped; its error is the answer
            break;
        }
    }
    if complete {
        let _ = tx.send(None).await;
    }
    drop(tx);
    match writer.await {
        Ok(Ok(bytes)) => {
            log::info!("Uploaded {rel} into the inbox ({bytes} bytes)");
            Response::builder()
                .header("content-type", "application/json")
                .body(
                    json!({ "path": rel, "bytes": bytes })
                        .to_string()
                        .into_bytes(),
                )
                .unwrap()
        }
        Ok(Err(e)) => error_response(StatusCode::BAD_REQUEST, &e),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &anyhow::anyhow!("{e}")),
    }
}

/// Routes that do not answer JSON from whole bodies: `POST upload?path=`
/// (streamed into the inbox) and `GET thumb?id=` (an image)
pub fn routes(knowledge: Arc<Knowledge>) -> BoxedFilter<(Response<Vec<u8>>,)> {
    let base = || {
        warp::path("api")
            .and(warp::path("cuttlefish"))
            .and(warp::path("knowledge"))
    };
    let uploader = Arc::clone(&knowledge);
    let upload = warp::post()
        .and(base())
        .and(warp::path("upload"))
        .and(warp::path::end())
        .and(warp::query::<HashMap<String, String>>())
        .and(warp::body::content_length_limit(MAX_UPLOAD))
        .and(warp::body::stream())
        .then(move |query: HashMap<String, String>, body| {
            let knowledge = Arc::clone(&uploader);
            let rel = query.get("path").cloned().unwrap_or_default();
            async move { upload(knowledge, rel, Box::pin(body)).await }
        });
    let thumb = warp::get()
        .and(base())
        .and(warp::path("thumb"))
        .and(warp::path::end())
        .and(warp::query::<HashMap<String, String>>())
        .then(move |query: HashMap<String, String>| {
            let knowledge = Arc::clone(&knowledge);
            async move {
                let id = query.get("id").cloned().unwrap_or_default();
                let made = tokio::task::spawn_blocking(move || knowledge.thumbnail(&id)).await;
                match made {
                    Ok(Ok((bytes, media_type))) => Response::builder()
                        .header("content-type", media_type)
                        .header("cache-control", "private, max-age=86400")
                        // An SVG opened on its own runs no scripts
                        .header(
                            "content-security-policy",
                            "default-src 'none'; style-src 'unsafe-inline'",
                        )
                        .body(bytes)
                        .unwrap(),
                    Ok(Err(Status(status, e))) => error_response(status, &e),
                    Err(e) => {
                        error_response(StatusCode::INTERNAL_SERVER_ERROR, &anyhow::anyhow!("{e}"))
                    }
                }
            }
        });
    upload.or(thumb).unify().boxed()
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

        let mut w = request(
            r#"{"kind": "wiki", "start": [" https://wiki.example/wiki/Category:Salmon_Run ", ""], "depth": 1, "exclude": ["Category:Mechanics"], "dry_run": true}"#,
        )
        .unwrap();
        assert_eq!(
            w.check().unwrap(),
            "Dry run: Wiki https://wiki.example/wiki/Category:Salmon_Run"
        );
        let Source::Wiki(spec) = &w.source else {
            panic!("not a wiki")
        };
        assert_eq!((spec.depth, spec.max_pages), (1, 500));
        // A title alone needs the api
        assert!(
            request(r#"{"kind": "wiki", "start": ["Category:Salmon Run"]}"#)
                .unwrap()
                .check()
                .is_err()
        );
        assert!(
            request(
                r#"{"kind": "wiki", "start": ["Salmon Run"], "api": "https://w.example/w/api.php"}"#
            )
            .unwrap()
            .check()
            .is_ok()
        );
        let mut site = request(
            r#"{"kind": "site", "start": "https://s.example/", "skip": ["/map/", " "], "max_pages": 30}"#,
        )
        .unwrap();
        assert_eq!(site.check().unwrap(), "Site https://s.example/");
        let Source::Site(spec) = &site.source else {
            panic!("not a site")
        };
        assert_eq!(spec.skip, ["/map/"]);
        assert!(
            request(r#"{"kind": "site", "start": "s.example"}"#)
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
        let knowledge = Knowledge::new(dir.clone(), Settings::default(), None);
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
    fn slang_is_taught_edited_and_planned() {
        let dir = std::env::temp_dir().join(format!("procon-slang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let knowledge = Arc::new(Knowledge::new(dir.clone(), Settings::default(), None));
        let post = |path: &str, body: Value| knowledge.post(path, body.to_string().as_bytes());
        let added = post(
            "slang/add",
            json!({"term": "Maws", "text": "shark", "lang": "en", "note": "its fin"}),
        )
        .ok()
        .unwrap();
        let id = added["id"].as_str().unwrap().to_string();
        assert_eq!(added["source"], "user");
        // Looked up, found in sentences and searched from now on
        assert_eq!(
            knowledge.glossary("shark").unwrap()["terms"][0]["id"],
            "maws"
        );
        assert_eq!(
            knowledge.glossary("the shark again").unwrap()["terms"][0]["id"],
            "maws"
        );
        assert_eq!(knowledge.terms("shar").unwrap()["terms"][0]["id"], "maws");
        let listed = knowledge.slang().unwrap();
        assert_eq!(listed["aliases"][0]["term_forms"]["en"][0], "Maws");
        assert_eq!(listed["pending"], 0);
        // Errors answer 400
        let bad = post(
            "slang/add",
            json!({"term": "Maws", "text": "Maws", "lang": "en"}),
        );
        assert_eq!(bad.err().map(|s| s.0), Some(StatusCode::BAD_REQUEST));
        let edited = post("slang/edit", json!({"id": id, "note": "fin"}))
            .ok()
            .unwrap();
        assert_eq!(edited["note"], "fin");
        assert!(dir.join(slang::FILE).is_file());
        post("slang/delete", json!({"id": id})).ok().unwrap();
        assert!(knowledge.glossary("shark").unwrap()["terms"][0].is_null());
        // A dry run over a store without documents reads nothing
        let dry = post("slang/suggest", json!({"dry_run": true}))
            .ok()
            .unwrap();
        assert_eq!(
            (dry["documents"].as_u64(), dry["batches"].as_u64()),
            (Some(0), Some(0))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn translates_from_the_glossary_without_a_key() {
        let g = Glossary::seed();
        // A bare term: its entry and its name in the target language
        let t = translate_with(&g, None, " 熊刷 ", "en").unwrap();
        assert!(t.term);
        assert_eq!(t.terms[0].id, "grizzco-roller");
        assert_eq!(t.translation.as_deref(), Some("Grizzco Roller"));
        assert_eq!(t.explanation, None);
        assert!(t.needs_key);
        // The glossary has no Chinese name for it: nothing without the model
        let t = translate_with(&g, None, "shore run", "zh").unwrap();
        assert!(t.term);
        assert_eq!(t.translation, None);
        // A sentence: the terms it mentions, the translation left to the model
        let t = translate_with(&g, None, "我刚拿的熊刷，不应该上柱子拍的", "en").unwrap();
        assert!(!t.term);
        let ids: Vec<_> = t.terms.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["grizzco-roller", "fish-stick"]);
        assert_eq!(t.translation, None);
        assert!(t.needs_key);
        let written = serde_json::to_value(&t).unwrap();
        assert!(written.get("translation").is_none());
        assert!(translate_with(&g, None, "  ", "en").is_err());
        assert!(translate_with(&g, None, "x", "en/../").is_err());
    }

    #[test]
    fn imports_wait_for_the_store_lock() {
        let root =
            std::env::temp_dir().join(format!("procon-knowledge-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let held = lock::acquire(&root, "cuttlefish ingest").unwrap();
        let knowledge = Arc::new(Knowledge::new(root.clone(), Settings::default(), None));
        let job = knowledge
            .ingest(request(r#"{"kind": "youtube", "url": "https://youtu.be/x"}"#).unwrap())
            .ok()
            .expect("the job starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let error = loop {
            let jobs = knowledge.jobs.lock().unwrap().clone();
            let j = jobs.iter().find(|j| j.id == job.id).unwrap();
            if j.state != JobState::Running {
                assert_eq!(j.state, JobState::Failed);
                break j.error.clone().unwrap();
            }
            assert!(std::time::Instant::now() < deadline, "the job never ended");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert!(
            error.starts_with("the knowledge store is being written by cuttlefish ingest pid "),
            "{error}"
        );
        assert!(error.contains("delete "), "{error}");
        // Deleting waits the same
        let e = knowledge.delete(&["x".to_string()]).unwrap_err();
        assert!(e.to_string().contains("being written by"), "{e}");
        drop(held);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn jobs_run_one_at_a_time() {
        let knowledge = Knowledge::new(PathBuf::from("/nonexistent"), Settings::default(), None);
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
            report: None,
            summary: None,
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
