//! `cuttlefish`: fill the knowledge store, search it and ask the model.
//!
//! Run `cuttlefish --help` for the commands. Keys come from the
//! environment only, or the env file `scripts/run.sh` loads
//! ([`cuttlefish::env_file`]): `ANTHROPIC_API_KEY` (ask, translate; or the
//! logged-in Claude Code CLI with `--backend claude-cli`),
//! `DISCORD_BOT_TOKEN` (ingest discord-bot) and `DISCORD_USER_TOKEN` (fetch
//! discord).

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use core::sync::atomic::{AtomicBool, Ordering};
use cuttlefish::crawl::Fetcher;
use cuttlefish::discord;
use cuttlefish::discord_fetch::{
    self, Browser, Https, Interruptible, Options, Pace, Range, TOKEN_VAR,
};
use cuttlefish::discord_media::{Attachments, MEDIA, size};
use cuttlefish::doc::Document;
use cuttlefish::embed::E5Embedder;
use cuttlefish::eval::EvalSet;
use cuttlefish::ingest::{self, Meta};
use cuttlefish::llm::{Backend, Client, Settings};
use cuttlefish::lock::{self, WriteLock};
use cuttlefish::review::{Reviewer, translate};
use cuttlefish::slang::{self, UserGlossary};
use cuttlefish::store::{self, Retrieval, Store};
use cuttlefish::{assets, env_file, image_text, inbox, leanny, tables};
use cuttlefish::{corpus, corpus_reviews, corpus_videos, expert};
use cuttlefish::{deep_eval, notes, questions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser)]
#[command(about = "Cuttlefish: Salmon Run knowledge store and AI reviewer")]
struct Cli {
    /// Knowledge folder; by default the studio's (see --config), else
    /// $CUTTLEFISH_DATA
    #[arg(long, global = true)]
    data: Option<PathBuf>,
    /// The studio's config file, whose knowledge folder is used ([cuttlefish]
    /// knowledge, else Knowledge next to the Inkspector's root); by default
    /// ./config.toml when there is one
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Import sources into the store
    #[command(subcommand)]
    Ingest(Ingest),
    /// Fetch raw material into a folder (the inbox reads it)
    #[command(subcommand)]
    Fetch(Fetch),
    /// The #vod-review corpus: the reviewed VODs of the fetched archive,
    /// their videos, and reviews of them for the studio
    #[command(subcommand)]
    Corpus(Corpus),
    /// Show the chunks closest to a query
    Search {
        /// What to look for, in any language
        query: String,
        /// Number of results
        #[arg(short, default_value_t = 8)]
        k: usize,
        /// Ranking: hybrid (embeddings and keywords), embedding or keyword
        #[arg(long, default_value = "hybrid")]
        mode: String,
    },
    /// Look up a glossary term, or list the terms mentioned in a text
    Glossary {
        /// A term in any language, or a sentence
        text: String,
    },
    /// Ask Cuttlefish a question (needs a model backend, see --backend)
    Ask {
        /// The question
        question: String,
        #[command(flatten)]
        model: ModelArgs,
        /// Knowledge excerpts to retrieve
        #[arg(short, default_value_t = 8)]
        k: usize,
    },
    /// Translate text with the community's names (needs a model backend)
    Translate {
        /// The text
        text: String,
        /// Target language code: en, ja, zh, zh-Hant, es, ru, fr, ...
        #[arg(long)]
        to: String,
        #[command(flatten)]
        model: ModelArgs,
    },
    /// Run an evaluation file (see `eval.example.toml`) or `retrieval`, the
    /// crate's retrieval set (questions/retrieval.toml), with recall@1/5
    /// for embeddings, BM25 and hybrid ranking; or `deep`: ask the model
    /// the deep question bank's questions that need no video
    /// (questions/deep.toml) and keep the answers in
    /// <knowledge>/eval/deep-<date>.jsonl for review in the studio
    Eval {
        /// Evaluation file, "retrieval" or "deep"
        target: String,
        /// Knowledge excerpts to retrieve
        #[arg(short, default_value_t = 8)]
        k: usize,
        /// Also ask the model and check the answers (needs a model backend;
        /// `deep` always asks)
        #[arg(long)]
        answer: bool,
        /// deep: the language the questions are asked in (en, zh)
        #[arg(long, default_value = "en")]
        lang: String,
        /// deep: questions asked at once (at most 8)
        #[arg(long, default_value_t = cuttlefish::deep_eval::DEFAULT_PARALLEL)]
        parallel: usize,
        /// deep: at most this many questions
        #[arg(long)]
        max: Option<usize>,
        /// deep: only these question ids (repeatable)
        #[arg(long)]
        only: Vec<String>,
        #[command(flatten)]
        model: ModelArgs,
    },
    /// What the store holds: documents per source, chunks, glossary terms
    /// per language, name tables, assets, the last inbox import
    Stats,
    /// List the documents (id, source, title, file or url)
    Docs,
    /// Delete documents and their chunks by id (see `docs`)
    Delete {
        /// Document ids
        #[arg(required = true)]
        ids: Vec<String>,
    },
    /// Re-chunk and re-embed every stored document
    Reindex,
    /// Read the images `fetch discord --attachments media` downloaded,
    /// with the model (see --backend): the text in each, in its language
    /// and in English, and what it shows (tables, marks on maps, numbers),
    /// kept by the image's hash in <knowledge>/media/image-text.jsonl, so
    /// no image is sent twice; the next `ingest inbox` puts each under its
    /// message
    ReadImages {
        /// Only these channels (links, <server>/<channel> or ids;
        /// repeatable); by default every fetched channel with images
        #[arg(long)]
        channel: Vec<discord_fetch::ChannelRef>,
        /// Only count and list the images a run would read
        #[arg(long)]
        dry_run: bool,
        /// Read images again that have a text
        #[arg(long)]
        refresh: bool,
        /// At most this many images this run
        #[arg(long)]
        max: Option<usize>,
        /// Images read at once (at most 8)
        #[arg(long, default_value_t = image_text::DEFAULT_PARALLEL)]
        parallel: usize,
        #[command(flatten)]
        model: ModelArgs,
    },
    /// Slang suggestions from the community documents, as the studio's
    /// Slang panel makes them (the studio reads the file on each request;
    /// its chat picks the changes up on the next slang change or restart)
    #[command(subcommand)]
    Slang(Slang),
}

#[derive(Subcommand)]
enum Slang {
    /// Ask the model for aliases and new terms in the text not read yet
    Suggest {
        /// Read everything not read yet, in as many batches as it takes
        #[arg(long)]
        all: bool,
        /// Batches at most (5 by default; with --all, no limit unless given)
        #[arg(long)]
        max_batches: Option<usize>,
        /// Batches sent at once (at most 8)
        #[arg(long, default_value_t = slang::DEFAULT_PARALLEL)]
        parallel: usize,
        /// Leave every suggestion pending, for review in the studio
        #[arg(long)]
        no_auto_apply: bool,
        /// Confidence from which a suggestion is approved at once
        #[arg(long, default_value_t = slang::DEFAULT_THRESHOLD)]
        threshold: f32,
        /// Only tell what a run would read
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        model: ModelArgs,
    },
    /// Move old aliases to the new terms that claim their text (the Flyfish's
    /// "missiles" to "Flyfish missiles")
    Move {
        /// Only list them
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Args)]
struct ModelArgs {
    /// Model name; by default the backend's (claude-opus-5-5 on the API,
    /// the CLI's own on the CLI)
    #[arg(long, env = "CUTTLEFISH_MODEL")]
    model: Option<String>,
    /// Effort: low, medium, high, xhigh, max
    #[arg(long, default_value = cuttlefish::llm::DEFAULT_EFFORT)]
    effort: String,
    /// What answers: api (ANTHROPIC_API_KEY), claude-cli (the logged-in
    /// Claude Code CLI on your subscription) or auto (the API when the key
    /// is set, else the CLI when it is on PATH)
    #[arg(long, env = "CUTTLEFISH_BACKEND", default_value_t = Backend::Auto)]
    backend: Backend,
}

impl ModelArgs {
    fn settings(&self) -> Settings {
        Settings {
            model: self.model.clone(),
            effort: self.effort.clone(),
            backend: self.backend,
            ..Settings::default()
        }
    }
}

#[derive(Subcommand)]
enum Ingest {
    /// Everything in <data>/inbox/: prose becomes documents, name tables
    /// glossary terms, images the asset catalogue (see the README)
    Inbox {
        /// Forget what this inbox file, folder, archive or family of files
        /// (a path as a report shows it) gave, and import it again
        #[arg(long, value_name = "PATH")]
        reimport: Option<String>,
        #[command(flatten)]
        meta: Meta,
    },
    /// Web pages: urls, a list file or a sitemap
    Url {
        /// Page urls
        urls: Vec<String>,
        /// File with one url per line (# starts a comment)
        #[arg(long)]
        list: Option<PathBuf>,
        /// Sitemap (or sitemap index) url
        #[arg(long)]
        sitemap: Option<String>,
        /// Every tab of a Google Sheet, even when its address names one
        #[arg(long)]
        all_tabs: bool,
        /// At most this many pages
        #[arg(long, default_value_t = 200)]
        max_pages: usize,
        /// Seconds between requests to one site (robots.txt may ask more)
        #[arg(long, default_value_t = 3.0)]
        delay_s: f32,
        #[command(flatten)]
        meta: Meta,
    },
    /// A MediaWiki topic through the wiki's API: categories with their
    /// subcategories, start pages with the pages they link to; again, only
    /// pages whose revision changed
    Wiki {
        #[command(flatten)]
        wiki: cuttlefish::wiki::Wiki,
        #[command(flatten)]
        meta: Meta,
    },
    /// A whole site from a start address, on its host only
    Site {
        #[command(flatten)]
        site: cuttlefish::wiki::Site,
        #[command(flatten)]
        meta: Meta,
    },
    /// YouTube video, playlist or channel transcripts (through yt-dlp)
    Youtube {
        /// Video, playlist or channel url
        url: String,
        /// Subtitle languages (yt-dlp --sub-langs)
        #[arg(long, default_value = ingest::SUB_LANGS)]
        sub_langs: String,
        /// At most this many videos
        #[arg(long, default_value_t = 50)]
        max: usize,
        #[command(flatten)]
        meta: Meta,
    },
    /// A DiscordChatExporter JSON export
    DiscordExport {
        /// Export files
        files: Vec<PathBuf>,
        /// Treat as one conversation per file (a thread or forum post)
        #[arg(long)]
        whole: bool,
        #[command(flatten)]
        meta: Meta,
    },
    /// Channels through the official bot API (token in DISCORD_BOT_TOKEN)
    DiscordBot {
        /// Channel ids (repeatable)
        #[arg(long, required = true)]
        channel: Vec<String>,
        /// Skip the channels' threads and forum posts
        #[arg(long)]
        no_threads: bool,
        #[command(flatten)]
        meta: Meta,
    },
    /// Local markdown, text, HTML or PDF files
    File {
        /// Paths
        paths: Vec<PathBuf>,
        /// Url to cite for the file
        #[arg(long)]
        url: Option<String>,
        #[command(flatten)]
        meta: Meta,
    },
    /// Lean's Splatoon 3 datamine (leanny.github.io): Salmonids, stages,
    /// Salmon Run weapons and specials, hazard levels and the Eggstra Work
    /// scenarios as fact cards, the Eggstra Work events table
    /// (corpus/eggstra_events.json, dates from Inkipedia) and a name table
    /// for the glossary. Incremental: files are fetched again only when
    /// their ETag changed; --refresh rebuilds the cards anyway
    Leanny {
        /// List the files and what the copies fetched so far would give;
        /// fetch and store nothing
        #[arg(long)]
        dry_run: bool,
        /// Skip the weapon and special parameter files (about 180 more
        /// requests the first time; the cards then lack the Parameters
        /// section)
        #[arg(long)]
        no_weapons: bool,
        /// Seconds between requests
        #[arg(long, default_value_t = cuttlefish::leanny::DEFAULT_DELAY_S)]
        delay_s: f32,
        #[command(flatten)]
        meta: Meta,
    },
}

#[derive(Subcommand)]
enum Fetch {
    /// Slowly archive Discord channels you are a member of, with your own
    /// account's token (DISCORD_USER_TOKEN in the env file). Against
    /// Discord's terms: the account can be banned. Read-only, only the
    /// given channels and their threads; resumes where it stopped
    Discord {
        /// The channel's link (right-click the channel > Copy Link:
        /// https://discord.com/channels/<server id>/<channel id>), or
        /// <server id>/<channel id>, or its id (repeatable)
        #[arg(long, required = true)]
        channel: Vec<discord_fetch::ChannelRef>,
        /// Server id, for the active-threads listing and the search of
        /// --count (default: the one the links name, else the channel's)
        #[arg(long)]
        guild: Option<String>,
        /// Only count: the channel's messages through Discord's search (a
        /// forum's posts through its listing too) and the time a whole
        /// fetch would take at this pace; reads no message
        #[arg(long)]
        count: bool,
        /// Folder for the channels' files themselves. By default each channel
        /// goes to <knowledge>/inbox/discord/<guild>/<channel>/, the
        /// knowledge folder being the studio's (see --config), where the
        /// inbox import picks it up
        #[arg(long)]
        out: Option<PathBuf>,
        /// Skip the channels' threads and forum posts
        #[arg(long)]
        no_threads: bool,
        /// The messages' uploaded files, after the messages: list counts
        /// them and adds up their sizes; videos downloads the videos, media
        /// the videos and images, one at a time at the same pace, into
        /// <knowledge>/media/discord/<guild>/<channel>/<message id>/ (with
        /// --out: <out>/media/...), skipping files already there and
        /// continuing partial ones; a link that expired has its page read
        /// again
        #[arg(long, value_enum, default_value_t = Attachments::List)]
        attachments: Attachments,
        /// Do not download files larger than this many MB
        #[arg(long, value_name = "MB")]
        max_file_mb: Option<u64>,
        /// Stop downloading when a channel's media folder would grow past
        /// this many GB (a synced folder has limited space)
        #[arg(long, value_name = "GB")]
        max_total_gb: Option<f64>,
        /// Seconds between requests, drawn anew for each from this range
        #[arg(long, default_value_t = Pace::default().delay)]
        delay: Range,
        /// Requests between longer pauses, drawn anew after each pause
        #[arg(long, default_value_t = Pace::default().pause_every)]
        pause_every: Range,
        /// Seconds of a longer pause, drawn from this range
        #[arg(long, default_value_t = Pace::default().pause)]
        pause: Range,
        /// At most this many requests per day (UTC); the run stops there
        #[arg(long)]
        daily_cap: Option<u32>,
        /// At most this many requests this run (2 checks access with one
        /// page of messages); the next run continues
        #[arg(long)]
        max_requests: Option<u32>,
        /// At most this many minutes a run; the next run continues
        #[arg(long)]
        max_minutes: Option<f64>,
    },
}

#[derive(Subcommand)]
enum Corpus {
    /// Build <knowledge>/corpus/vod-review.jsonl from the archive in the
    /// inbox: one line per reviewed VOD (its video, poster, date and era,
    /// the comments with their moments, placed in the video or waiting
    /// for its wave-start table), and print the counts
    Build,
    /// Download the corpus's YouTube VODs at 480p with yt-dlp into
    /// <knowledge>/media/youtube/<id>.mp4 (with <id>.info.json), one at
    /// a time with a random pause between them; resumes, skips what is
    /// there, remembers deleted and private videos
    Videos {
        /// Only list the videos with yt-dlp's metadata (title, length,
        /// size of the 480p download) and add up what a run would
        /// download; downloads nothing
        #[arg(long)]
        list: bool,
        /// With --list: ask yt-dlp again about videos already listed
        #[arg(long)]
        refresh: bool,
        /// Seconds between two downloads, drawn anew from this range
        #[arg(long, default_value_t = corpus_videos::DEFAULT_DELAY)]
        delay: Range,
        /// At most this many downloads this run
        #[arg(long)]
        max: Option<usize>,
        /// Ask again for videos remembered as unavailable
        #[arg(long)]
        retry_unavailable: bool,
    },
    /// Read the HUD of every VOD's video on disk that has no wave table
    /// yet (gameplay-vision's reader: wave number and timer) and write
    /// <video stem>.wave_starts.json beside it, so the next build places
    /// the wave-timer moments; about 1-2 s per minute of video
    Align {
        /// Scan videos that have a table already, too
        #[arg(long)]
        refresh: bool,
    },
    /// Create or update a review of the studio for every VOD whose video
    /// is on disk: <reviews>/discord-<id>/review.json, the comments at
    /// their moments, the rest as notes; re-running changes only what the
    /// archive gave, never what was added in the studio
    Reviews {
        /// The reviews folder; by default the studio's ([cuttlefish]
        /// reviews of --config, else Reviews next to the knowledge folder)
        #[arg(long)]
        reviews: Option<PathBuf>,
    },
    /// Index every reviewer comment of the corpus as an expert comment of
    /// its own (reviewer, date, era, the VOD and the moment it is about),
    /// one document per VOD, under the store's write lock; VODs whose
    /// comments did not change are skipped, those gone are removed
    Index,
    /// How well a moment alone finds expert comments: for up to N comments
    /// placed in a video on disk with a wave table (one per VOD), search
    /// the expert comments with the moment's summary (the HUD's wave,
    /// timer and eggs at that time), leaving the comment's own message
    /// out, and count comments on the same VOD and about the same wave
    /// near the top. No model is asked.
    Retrieval {
        /// Moments to try
        #[arg(long, default_value_t = 10)]
        n: usize,
        /// Expert comments retrieved per moment
        #[arg(long, default_value_t = 10)]
        k: usize,
        /// A question put before the summary, as a player would ask
        #[arg(long)]
        question: Option<String>,
    },
}

/// Adds documents to the store, saving the index every few documents,
/// under the store's write lock
struct Sink {
    store: Store,
    embedder: E5Embedder,
    added: usize,
    /// Released last, after the index is written
    _lock: WriteLock,
}

/// Opens the store: brings the data folder of the older layout over (by
/// copying), loads the embedder and, with `catch_up`, embeds documents the
/// index lacks (synced in from elsewhere)
fn open(data: &Path, catch_up: bool) -> Result<(Store, E5Embedder)> {
    let models = store::models_dir();
    if let Err(e) = store::migrate(&store::legacy_root(), data) {
        log::warn!("could not bring the older data folder over: {e:#}");
    }
    let embedder = E5Embedder::load(&models)?;
    let mut store = Store::open(data, &embedder)?;
    if catch_up {
        let mut last = std::time::Instant::now();
        let caught_up = store.catch_up(&embedder, &mut |line| {
            // Progress every few seconds, other lines as they come
            if !line.contains(" chunks (") || last.elapsed().as_secs() >= 5 {
                log::info!("{line}");
                last = std::time::Instant::now();
            }
            true
        })?;
        if caught_up.documents > 0 {
            println!(
                "{} documents synced in from elsewhere embedded",
                caught_up.documents
            );
            store.save()?;
        }
        // The expert notes' files are the truth
        let synced = notes::sync(&mut store, &embedder)?;
        if synced.changed() {
            println!(
                "expert notes: {} embedded, {} removed",
                synced.embedded, synced.removed
            );
            store.save()?;
        }
    }
    Ok((store, embedder))
}

impl Sink {
    fn open(data: &Path) -> Result<Self> {
        let lock = lock::acquire(data, "cuttlefish ingest")?;
        let (store, embedder) = open(data, true)?;
        Ok(Sink {
            store,
            embedder,
            added: 0,
            _lock: lock,
        })
    }

    fn finish(self) -> Result<()> {
        self.store.save()?;
        println!("{} documents added", self.added);
        Ok(())
    }
}

impl ingest::Sink for Sink {
    fn has(&self, key: &str) -> bool {
        self.store.has(key)
    }

    fn has_id(&self, id: &str) -> bool {
        self.store.has_id(id)
    }

    fn has_table(&self, key: &str) -> bool {
        tables::has(self.store.root(), key)
    }

    fn revision(&self, key: &str) -> Option<u64> {
        self.store.document(key)?.revision
    }

    fn add(&mut self, doc: &Document) -> Result<usize> {
        let n = self.store.add(doc, &self.embedder)?;
        self.added += 1;
        if self.added.is_multiple_of(10) {
            self.store.save()?;
        }
        Ok(n)
    }

    fn delete(&mut self, id: &str) -> Result<bool> {
        self.store.delete(id)
    }

    fn raw_dir(&self, kind: &str) -> PathBuf {
        self.store.raw_dir(kind)
    }

    fn add_table(&mut self, table: &cuttlefish::tables::Table) -> Result<()> {
        cuttlefish::tables::save(self.store.root(), table)?;
        self.store.reload_glossary()
    }

    fn note(&mut self, line: &str) {
        if line.starts_with("skipped") {
            log::warn!("{line}");
        } else {
            println!("{line}");
        }
    }
}

/// Where the studio with this config keeps its knowledge, as the studio
/// finds it: `[cuttlefish] knowledge` (relative to the config file), else
/// `Knowledge` next to the sessions' folder, which is `[inspect] root` or
/// the folder of the recording prefix (the dashboard's choice, saved in
/// `<config>.state.json`, before `[recording] prefix`)
fn studio_knowledge(config: &Path) -> Result<PathBuf> {
    let text =
        std::fs::read_to_string(config).with_context(|| format!("reading {}", config.display()))?;
    let value: toml::Value =
        toml::from_str(&text).with_context(|| format!("in {}", config.display()))?;
    let dir = config.parent().unwrap_or(Path::new("."));
    let get = |table: &str, key: &str| value.get(table)?.get(key)?.as_str().map(String::from);
    if let Some(knowledge) = get("cuttlefish", "knowledge") {
        return Ok(dir.join(knowledge));
    }
    let sessions = match get("inspect", "root") {
        Some(root) => dir.join(root),
        None => {
            let saved = std::fs::read(config.with_extension("state.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|state| state["prefix"].as_str().map(String::from));
            let prefix = saved
                .or_else(|| get("recording", "prefix"))
                .context("the config has no [recording] prefix")?;
            // As the recorder: "a/b-" lives in "a", "a/b/" in "a/b"
            match Path::new(&format!("{prefix}x")).parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            }
        }
    };
    Ok(sessions
        .parent()
        .unwrap_or(Path::new("."))
        .join("Knowledge"))
}

/// The reviews folder as the studio finds it: `[cuttlefish] reviews` of the
/// config (relative to it), else `Reviews` next to the knowledge folder
fn reviews_folder(knowledge: &Path, config: Option<&Path>) -> Result<PathBuf> {
    let config = config
        .map(Path::to_path_buf)
        .or_else(|| Some(PathBuf::from("config.toml")).filter(|c| c.is_file()));
    if let Some(config) = config {
        let text = std::fs::read_to_string(&config)
            .with_context(|| format!("reading {}", config.display()))?;
        let value: toml::Value =
            toml::from_str(&text).with_context(|| format!("in {}", config.display()))?;
        if let Some(reviews) = value
            .get("cuttlefish")
            .and_then(|c| c.get("reviews"))
            .and_then(|r| r.as_str())
        {
            return Ok(config.parent().unwrap_or(Path::new(".")).join(reviews));
        }
    }
    Ok(knowledge.parent().unwrap_or(Path::new(".")).join("Reviews"))
}

/// The knowledge folder: `--data`, else the studio's through `--config` (or
/// `./config.toml` when there is one), else `$CUTTLEFISH_DATA`
fn knowledge_folder(data: Option<PathBuf>, config: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(data) = data {
        return Ok(data);
    }
    let config = config.or_else(|| Some(PathBuf::from("config.toml")).filter(|c| c.is_file()));
    if let Some(config) = config {
        let data = studio_knowledge(&config)?;
        log::info!(
            "knowledge folder of {}: {}",
            config.display(),
            data.display()
        );
        return Ok(data);
    }
    match std::env::var_os("CUTTLEFISH_DATA").filter(|d| !d.is_empty()) {
        Some(d) => Ok(PathBuf::from(d)),
        None => bail!(
            "no knowledge folder: run where the studio's config.toml is, or give --config <studio config>, --data <folder> or $CUTTLEFISH_DATA"
        ),
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // Before any thread: secrets from the env file, as scripts/run.sh
    if let Some(p) = env_file::load() {
        log::info!("environment loaded from {}", p.display());
    }
    let cli = Cli::parse();
    if let Command::Fetch(f) = cli.command {
        return fetch(f, cli.data, cli.config);
    }
    let data = knowledge_folder(cli.data.clone(), cli.config.clone())?;
    match cli.command {
        Command::Fetch(_) => unreachable!(),
        Command::Corpus(c) => corpus_command(&data, cli.config.as_deref(), c),
        Command::Ingest(i) => ingest(&data, i),
        Command::Search { query, k, mode } => {
            let mode = match mode.as_str() {
                "hybrid" => Retrieval::Hybrid,
                "embedding" => Retrieval::Embedding,
                "keyword" => Retrieval::Keyword,
                other => bail!("no ranking {other}: hybrid, embedding or keyword"),
            };
            let (store, embedder) = open(&data, true)?;
            let hits = store.search_with(mode, &query, k, &embedder, &|_| true)?;
            if hits.is_empty() {
                println!("nothing found (is the store empty? see `cuttlefish stats`)");
            }
            for h in hits {
                let e = &h.entry;
                let snippet: String = e.text.chars().take(240).collect();
                println!(
                    "{:.3} [{}] {}{}{}",
                    h.score,
                    serde_json::to_value(e.source)?.as_str().unwrap_or_default(),
                    e.title,
                    if e.heading.is_empty() { "" } else { " > " },
                    e.heading
                );
                if let Some(u) = &e.url {
                    println!("      {u}");
                }
                println!("      {}\n", snippet.replace('\n', " "));
            }
            Ok(())
        }
        Command::Glossary { text } => {
            let g = Store::load_glossary(&data)?;
            let terms = match g.lookup(&text) {
                Some(t) => vec![t],
                None => g.find_in(&text),
            };
            if terms.is_empty() {
                println!("no glossary term found");
            }
            print!(
                "{}",
                cuttlefish::glossary::Glossary::prompt_lines(&terms, None)
            );
            Ok(())
        }
        Command::Ask { question, model, k } => {
            let mut reviewer = Reviewer::open(&data, model.settings())?;
            reviewer.k = k;
            let answer = reviewer.ask(&question)?;
            println!("{}\n", answer.text);
            for s in answer.sources {
                println!(
                    "[{}] {} > {} ({}){}",
                    s.id,
                    s.title,
                    s.heading,
                    s.license.as_deref().unwrap_or("license unknown"),
                    s.url.map(|u| format!(" {u}")).unwrap_or_default()
                );
            }
            Ok(())
        }
        Command::Translate { text, to, model } => {
            let client = Client::from_env(model.settings())?;
            let g = Store::load_glossary(&data)?;
            println!("{}", translate(&client, &g, &text, &to)?);
            Ok(())
        }
        Command::Eval {
            target,
            k,
            answer,
            lang,
            parallel,
            max,
            only,
            model,
        } => {
            if target == "deep" {
                let opts = deep_eval::Options {
                    lang,
                    parallel,
                    max,
                    only,
                    k,
                };
                return eval_deep(&data, model.settings(), &opts);
            }
            let set = if target == "retrieval" {
                EvalSet::parse(cuttlefish::eval::RETRIEVAL)?
            } else {
                let file = PathBuf::from(target);
                EvalSet::parse(
                    &std::fs::read_to_string(&file)
                        .with_context(|| format!("reading {}", file.display()))?,
                )?
            };
            let mut reviewer = if answer {
                let mut r = Reviewer::open(&data, model.settings())?;
                r.k = k;
                Some(r)
            } else {
                None
            };
            let (store, embedder) = open(&data, true)?;
            let (mut found, mut points, mut expected) = (0, 0, 0);
            for case in &set.cases {
                let hits = store.search(&case.question, k, &embedder)?;
                let ok = case.retrieved(&hits);
                found += ok as usize;
                println!("{} {}", if ok { "ok  " } else { "MISS" }, case.question);
                if let Some(r) = reviewer.as_mut() {
                    let a = r.ask(&case.question)?;
                    let got = case.points_in(&a.text);
                    points += got.len();
                    expected += case.expect_points.len();
                    println!(
                        "     points {}/{}: {:?}\n     {}\n",
                        got.len(),
                        case.expect_points.len(),
                        got,
                        a.text.replace('\n', "\n     ")
                    );
                }
            }
            println!(
                "retrieval: {found}/{} cases in the top {k}",
                set.cases.len()
            );
            if answer {
                println!("answers: {points}/{expected} expected points");
            }
            let started = std::time::Instant::now();
            let ranks = cuttlefish::eval::compare(&store, &embedder, &set)?;
            println!("\nrank of the first right chunk (embedding, BM25, hybrid; - past 5):");
            let show = |r: Option<usize>| r.map_or_else(|| String::from("-"), |r| r.to_string());
            for (case, r) in set.cases.iter().zip(&ranks) {
                println!(
                    "{:>3} {:>3} {:>3}  {}",
                    show(r[0]),
                    show(r[1]),
                    show(r[2]),
                    case.question
                );
            }
            let n = set.cases.len();
            println!("\n{:<10} {:>9} {:>9}", "", "recall@1", "recall@5");
            for (m, name) in ["embedding", "BM25", "hybrid"].iter().enumerate() {
                let at = |k| cuttlefish::eval::recall_at(&ranks, m, k);
                println!("{name:<10} {:>4}/{n:<4} {:>4}/{n:<4}", at(1), at(5));
            }
            println!(
                "({} searches in {:.1} s)",
                3 * n,
                started.elapsed().as_secs_f32()
            );
            Ok(())
        }
        Command::Stats => {
            let (store, _) = open(&data, true)?;
            let (counts, chunks) = store.stats()?;
            println!("data folder: {}", data.display());
            for (kind, n) in counts {
                println!(
                    "{:>6} {}",
                    n,
                    serde_json::to_value(kind)?.as_str().unwrap_or_default()
                );
            }
            println!("{chunks:>6} chunks in the index");
            let glossary = store.glossary();
            println!("{:>6} glossary terms", glossary.terms.len());
            for (lang, n) in glossary.languages() {
                println!("{n:>10} with a name in {lang}");
            }
            for t in tables::load_all(&data) {
                println!("{:>6} terms from {}", t.terms.len(), t.source);
            }
            let catalogue = assets::Catalogue::load(&data);
            println!("{:>6} images and icons", catalogue.assets.len());
            for (folder, n) in catalogue.folders() {
                println!("{n:>10} in {folder}");
            }
            if let Some(r) = inbox::reports(&data, 1).first() {
                println!("last inbox import {}: {}", r.id, r.summary());
            }
            Ok(())
        }
        Command::Docs => {
            for d in store::read_documents(&data)? {
                println!(
                    "{} {:<18} {}  {}",
                    d.id,
                    serde_json::to_value(d.source)?.as_str().unwrap_or_default(),
                    d.title,
                    d.path.or(d.url).unwrap_or_default()
                );
            }
            Ok(())
        }
        Command::Delete { ids } => {
            let _lock = lock::acquire(&data, "cuttlefish delete")?;
            let (mut store, _) = open(&data, false)?;
            for id in &ids {
                if store.delete(id)? {
                    println!("deleted {id}");
                } else {
                    println!("no document {id}");
                }
            }
            store.save()
        }
        Command::Reindex => {
            let _lock = lock::acquire(&data, "cuttlefish reindex")?;
            let index = data.join("index");
            if index.exists() {
                std::fs::remove_dir_all(&index)?;
            }
            let (mut store, embedder) = open(&data, false)?;
            let n = store.reindex(&embedder)?;
            store.save()?;
            println!("{n} chunks indexed");
            Ok(())
        }
        Command::Slang(s) => slang_command(&data, s),
        Command::ReadImages {
            channel,
            dry_run,
            refresh,
            max,
            parallel,
            model,
        } => {
            let channels: Vec<String> = channel.into_iter().map(|c| c.channel).collect();
            let texts = image_text::Texts::load(&data)?;
            let mut plan = image_text::plan(&data, &texts, &channels, refresh)?;
            println!(
                "{} images in the fetched channels' media folders, {} of them read before: {} to read",
                plan.images,
                plan.read,
                plan.jobs.len()
            );
            if let Some(max) = max {
                plan.jobs.truncate(max);
            }
            if dry_run || plan.jobs.is_empty() {
                for job in &plan.jobs {
                    println!("  {}", job.context);
                }
                return Ok(());
            }
            let client = Client::from_env(model.settings())?;
            println!(
                "reading {} images through {}, {} at once",
                plan.jobs.len(),
                client.backend(),
                parallel.clamp(1, image_text::MAX_PARALLEL)
            );
            let glossary = Store::load_glossary(&data)?;
            let stop = ctrl_c()?;
            let tally = image_text::read(
                &client,
                &glossary,
                &data,
                &plan.jobs,
                parallel,
                &|| stop.load(Ordering::Relaxed),
                &|line| println!("{line}"),
            )?;
            println!(
                "{tally}; kept in {}; `cuttlefish ingest inbox` puts them under their messages",
                image_text::Texts::path(&data).display()
            );
            Ok(())
        }
    }
}

/// `fetch`: `data` and `config` find the knowledge folder for the default
/// output
fn fetch(cmd: Fetch, data: Option<PathBuf>, config: Option<PathBuf>) -> Result<()> {
    match cmd {
        Fetch::Discord {
            channel,
            guild,
            count,
            out,
            no_threads,
            attachments,
            max_file_mb,
            max_total_gb,
            delay,
            pause_every,
            pause,
            daily_cap,
            max_requests,
            max_minutes,
        } => {
            // The channels' own folder (their files in <out>/media), or
            // <knowledge>/inbox/discord with a folder per channel below it
            // (their files in <knowledge>/media/discord)
            let (root, media, flat) = match out {
                Some(out) => (out.clone(), out.join("media"), true),
                None => {
                    let knowledge = knowledge_folder(data, config)?;
                    (
                        knowledge.join(inbox::INBOX).join("discord"),
                        knowledge.join(MEDIA).join("discord"),
                        false,
                    )
                }
            };
            let token = std::env::var(TOKEN_VAR)
                .ok()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .with_context(|| {
                    format!(
                        "{TOKEN_VAR} is not set: put it in the env file (~/.config/procon/env, chmod 600), see the README"
                    )
                })?;
            let guild = discord_fetch::guild_of(guild, &channel)?;
            let stop = ctrl_c()?;
            println!(
                "{} {} channel(s) with your own account, against Discord's terms: at your own risk. Ctrl+C stops after the request under way.",
                if count { "Counting" } else { "Reading" },
                channel.len()
            );
            println!(
                "Pace: {delay} s between requests, a pause of {pause} s every {pause_every} requests{}{}{}",
                daily_cap.map_or_else(String::new, |n| format!(", at most {n} requests a day")),
                max_requests
                    .map_or_else(String::new, |n| format!(", at most {n} requests this run")),
                max_minutes.map_or_else(String::new, |n| format!(", at most {n} minutes this run")),
            );
            let browser = Browser::from_env();
            println!("{}", browser.describe());
            if !count {
                println!(
                    "Attachments: {}",
                    match attachments {
                        Attachments::None => "not looked at".to_string(),
                        Attachments::List =>
                            "counted, not downloaded (--attachments videos or media downloads them)"
                                .to_string(),
                        Attachments::Videos | Attachments::Media => format!(
                            "{} downloaded into {}{}{}",
                            if attachments == Attachments::Videos {
                                "videos"
                            } else {
                                "videos and images"
                            },
                            media.display(),
                            max_file_mb
                                .map_or_else(String::new, |mb| format!(", files up to {mb} MB")),
                            max_total_gb.map_or_else(String::new, |gb| format!(
                                ", up to {gb} GB per channel"
                            )),
                        ),
                    }
                );
            }
            let options = Options {
                channels: channel.into_iter().map(|c| c.channel).collect(),
                guild,
                threads: !no_threads,
                flat,
                count,
                pace: Pace {
                    delay,
                    pause_every,
                    pause,
                    daily_cap,
                    max_requests,
                    max_minutes,
                },
                browser,
                attachments,
                max_file_mb,
                max_total_gb,
                media: media.clone(),
            };
            let pace = options.pace.clone();
            let mut http = Https::default();
            let mut clock = Interruptible {
                stop,
                started: Instant::now(),
            };
            let summary =
                discord_fetch::run(&mut http, &mut clock, token, &root, options, &mut |line| {
                    println!("{line}")
                })?;
            for c in &summary.counts {
                print_count(c, &pace);
            }
            if let Some(m) = &summary.media {
                print_media(m, attachments, max_file_mb, max_total_gb, &media);
            }
            println!(
                "{} requests, {} messages added, {} channels and threads under {}{}",
                summary.requests,
                summary.messages,
                summary.conversations,
                root.display(),
                summary
                    .stopped
                    .map(|s| format!("; stopped: {s}"))
                    .unwrap_or_default()
            );
            Ok(())
        }
    }
}

/// `slang`: suggestions over the community documents, moves of old aliases
fn slang_command(data: &Path, cmd: Slang) -> Result<()> {
    let held = std::sync::Mutex::new(());
    match cmd {
        Slang::Suggest {
            all,
            max_batches,
            parallel,
            no_auto_apply,
            threshold,
            dry_run,
            model,
        } => {
            let opts = slang::SuggestOptions {
                all,
                max_batches,
                ..slang::SuggestOptions::default()
            };
            let docs = store::read_documents(data)?;
            let plan = slang::plan(&docs, &UserGlossary::load(data)?, &opts);
            println!(
                "{} documents, {} characters not read: {} batches; this run reads {}",
                plan.documents,
                plan.chars,
                plan.batches_total,
                plan.batches.len()
            );
            if dry_run || plan.batches.is_empty() {
                return Ok(());
            }
            let client = Client::from_env(model.settings())?;
            let run = slang::RunOptions {
                parallel: parallel.clamp(1, slang::MAX_PARALLEL),
                auto_apply: (!no_auto_apply).then_some(threshold.clamp(0.0, 1.0)),
            };
            let stop = ctrl_c()?;
            let report = slang::run(
                &client,
                data,
                &plan.batches,
                run,
                &held,
                &|| stop.load(Ordering::Relaxed),
                &|_, line| println!("{line}"),
            )?;
            println!("done: {report}");
            Ok(())
        }
        Slang::Move { dry_run } => {
            let mut user = UserGlossary::load(data)?;
            let moves = user.moves();
            for m in &moves {
                println!(
                    "{} ({}): {} → {}{}",
                    m.text,
                    m.lang,
                    m.from_name,
                    m.to_name,
                    m.relation
                        .as_ref()
                        .map(|r| format!(" ({r})"))
                        .unwrap_or_default()
                );
                if !dry_run {
                    user.apply_move(&m.alias)?;
                }
            }
            if !dry_run && !moves.is_empty() {
                user.save(data)?;
            }
            println!(
                "{} aliases {}",
                moves.len(),
                if dry_run { "can move" } else { "moved" }
            );
            Ok(())
        }
    }
}

/// `eval deep`: the bank's askable questions through the model, the
/// answers into `<knowledge>/eval/deep-<date>.jsonl`
fn eval_deep(data: &Path, settings: Settings, opts: &deep_eval::Options) -> Result<()> {
    let client = Client::from_env(settings)?;
    let (store, embedder) = open(data, true)?;
    let bank = questions::Bank::seed();
    let picked = deep_eval::pick(&bank, opts);
    let out = deep_eval::new_file(data, chrono::Utc::now());
    println!(
        "Asking {} of the bank's {} questions in {}, {} at a time; answers go to {}. Ctrl+C stops after the batch under way.",
        picked.len(),
        bank.questions.len(),
        opts.lang,
        opts.parallel.clamp(1, deep_eval::MAX_PARALLEL),
        out.display()
    );
    let stop = ctrl_c()?;
    let summary = deep_eval::run(
        deep_eval::Asker {
            store: &store,
            embedder: &embedder,
            client: &client,
        },
        &bank,
        opts,
        &out,
        &|| stop.load(Ordering::Relaxed),
        &mut |done, total, entry| {
            println!("[{done}/{total}] {} ({})", entry.question, entry.id);
            match &entry.error {
                Some(e) => println!("     failed: {e}"),
                None => {
                    println!("     {}", entry.answer.replace('\n', "\n     "));
                    for s in &entry.sources {
                        println!(
                            "     [{}] {} > {}{}",
                            s.id,
                            s.title,
                            s.heading,
                            s.url.as_ref().map(|u| format!(" {u}")).unwrap_or_default()
                        );
                    }
                }
            }
            println!();
        },
    )?;
    println!("{summary}; review them in the studio's Knowledge view");
    Ok(())
}

/// A flag Ctrl+C sets
fn ctrl_c() -> Result<Arc<AtomicBool>> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
        .context("setting the Ctrl+C handler")?;
    Ok(stop)
}

/// `corpus`: the VOD-review corpus, its videos and its reviews
fn corpus_command(data: &Path, config: Option<&Path>, cmd: Corpus) -> Result<()> {
    let built = corpus::build(data)?;
    match cmd {
        Corpus::Build => {
            let file = corpus::write(data, &built)?;
            println!("{}", corpus::Stats::of(&built, data));
            println!("written: {}", file.display());
            Ok(())
        }
        Corpus::Videos {
            list,
            refresh,
            delay,
            max,
            retry_unavailable,
        } => {
            let dir = corpus_videos::dir(data);
            let stop = ctrl_c()?;
            let mut report = |line: &str| println!("{line}");
            if list {
                let listing = corpus_videos::list(&built, &dir, refresh, &stop, &mut report)?;
                println!("{}", listing.summary());
                println!(
                    "`cuttlefish corpus videos` downloads them into {} at 480p, one at a time with {} s between videos",
                    dir.display(),
                    corpus_videos::DEFAULT_DELAY
                );
                return Ok(());
            }
            println!(
                "Downloading the corpus's YouTube VODs into {} at 480p, {delay} s between videos{}. Ctrl+C stops after the video under way.",
                dir.display(),
                max.map_or_else(String::new, |m| format!(", at most {m} this run"))
            );
            let options = corpus_videos::Options {
                delay,
                max,
                retry_unavailable,
            };
            let summary = corpus_videos::run(&built, &dir, &options, &stop, &mut report)?;
            println!("{summary}");
            Ok(())
        }
        Corpus::Align { refresh } => {
            let stop = ctrl_c()?;
            println!(
                "Reading the HUD of {} videos on disk. Ctrl+C stops after the video under way.",
                corpus::Stats::of(&built, data).with_local_video
            );
            let done = corpus::align(
                &built,
                data,
                refresh,
                &|| stop.load(Ordering::Relaxed),
                &mut |line| println!("{line}"),
            )?;
            println!("{done}");
            let built = corpus::build(data)?;
            corpus::write(data, &built)?;
            println!("{}", corpus::Stats::of(&built, data));
            Ok(())
        }
        Corpus::Reviews { reviews } => {
            let reviews = match reviews {
                Some(r) => r,
                None => reviews_folder(data, config)?,
            };
            corpus::write(data, &built)?;
            let done = corpus_reviews::write(&built, data, &reviews)?;
            println!("{done}");
            println!("reviews in {}", reviews.display());
            Ok(())
        }
        Corpus::Index => {
            let _lock = lock::acquire(data, "cuttlefish corpus index")?;
            corpus::write(data, &built)?;
            let (mut store, embedder) = open(data, false)?;
            let done = expert::index(&mut store, &embedder, &built, &mut |line| {
                println!("{line}")
            })?;
            store.save()?;
            println!("{done}");
            Ok(())
        }
        Corpus::Retrieval { n, k, question } => {
            let (store, embedder) = open(data, false)?;
            let cases =
                expert::evaluate(&store, &embedder, &built, data, n, k, question.as_deref())?;
            let (mut found, mut rank_sum, mut wave_top5, mut vod_chance, mut wave_chance) =
                (0, 0, 0, 0.0, 0.0);
            for c in &cases {
                println!("{} (the HUD shows W{})\n  {}", c.label, c.wave, c.url);
                println!("  query: {}", c.query.replace('\n', " | "));
                match c.same_vod_rank {
                    Some(r) => println!("  same VOD first at rank {r}"),
                    None => println!("  same VOD not in the top {k}"),
                }
                println!(
                    "  same wave in the top 5: {} (chance {:.0}%)",
                    c.same_wave_top5,
                    c.same_wave_share * 100.0
                );
                for (label, same) in &c.top {
                    println!("    {}{label}", if *same { "* " } else { "  " });
                }
                if let Some(r) = c.same_vod_rank {
                    found += 1;
                    rank_sum += r;
                }
                wave_top5 += c.same_wave_top5;
                vod_chance += 1.0 - (1.0 - c.same_vod_share).powi(k as i32);
                wave_chance += c.same_wave_share * 5.0;
            }
            let n = cases.len().max(1) as f64;
            println!(
                "{} moments: a comment on the same VOD in the top {k} for {found} (by chance ~{:.1}){}; comments about the same wave in the top 5: {:.1} on average (by chance {:.1})",
                cases.len(),
                vod_chance,
                if found > 0 {
                    format!(
                        ", first at rank {:.1} on average",
                        rank_sum as f64 / found as f64
                    )
                } else {
                    String::new()
                },
                wave_top5 as f64 / n,
                wave_chance / n
            );
            Ok(())
        }
    }
}

/// Hours and minutes (`3 h 05 min`), or minutes
fn hours(d: core::time::Duration) -> String {
    let minutes = (d.as_secs_f64() / 60.0).round() as u64;
    if minutes >= 60 {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} min")
    }
}

/// Prints what `fetch discord --count` found and how long a fetch takes
fn print_count(c: &discord_fetch::Count, pace: &Pace) {
    let posts = c.posts.map(|p| format!(", {p} posts")).unwrap_or_default();
    let Some(messages) = c.messages else {
        println!("#{} ({}): message count unknown{posts}", c.name, c.id);
        return;
    };
    println!(
        "#{} ({}): {messages} messages by Discord's search{posts}",
        c.name, c.id
    );
    let Some(requests) = c.requests() else {
        return;
    };
    let days = pace
        .daily_cap
        .map(|cap| {
            format!(
                ", {} days at {cap} a day",
                requests.div_ceil(u64::from(cap.max(1)))
            )
        })
        .unwrap_or_default();
    println!(
        "  a whole fetch: about {requests} requests, {} at this pace{days}{}",
        hours(discord_fetch::estimate(requests, pace)),
        if c.forum {
            ""
        } else {
            "; each thread adds a request or more"
        }
    );
}

/// Prints what a run found out about attachments and what it downloaded
fn print_media(
    m: &discord_fetch::Media,
    attachments: Attachments,
    max_file_mb: Option<u64>,
    max_total_gb: Option<f64>,
    media: &Path,
) {
    println!("Attachments in the archive: {}", m.listing);
    if !matches!(attachments, Attachments::Videos | Attachments::Media) {
        println!(
            "  --attachments videos downloads the videos ({}), --attachments media the images too ({}); a synced knowledge folder needs that much space",
            size(m.listing.videos.bytes),
            size(m.listing.videos.bytes + m.listing.images.bytes)
        );
        return;
    }
    let mut parts = vec![format!(
        "downloaded {} files ({}) into {}",
        m.downloaded.files,
        size(m.downloaded.bytes),
        media.display()
    )];
    if m.present > 0 {
        parts.push(format!("{} already there", m.present));
    }
    if m.skipped_size.files > 0 {
        parts.push(format!(
            "{} skipped over {} MB ({})",
            m.skipped_size.files,
            max_file_mb.unwrap_or_default(),
            size(m.skipped_size.bytes)
        ));
    }
    if m.skipped_total.files > 0 {
        parts.push(format!(
            "{} skipped for the {} GB total ({})",
            m.skipped_total.files,
            max_total_gb.unwrap_or_default(),
            size(m.skipped_total.bytes)
        ));
    }
    if m.refreshed > 0 {
        parts.push(format!("{} pages read again for fresh links", m.refreshed));
    }
    if m.failed > 0 {
        parts.push(format!("{} failed (see above)", m.failed));
    }
    if m.out_of_scope > 0 {
        parts.push(format!("{} not on Discord's CDN", m.out_of_scope));
    }
    println!("  {}", parts.join("; "));
}

/// Prints an inbox import's report
fn print_report(r: &inbox::Report) {
    for t in &r.taken {
        println!("+ {:<9} {}: {}", t.kind, t.path, t.detail);
    }
    for s in &r.skipped {
        println!(
            "- skipped {} ({}), e.g. {}",
            s.reason,
            s.count,
            s.examples.join(", ")
        );
    }
    for f in &r.failed {
        println!("! failed {}: {}", f.path, f.detail);
    }
    for g in &r.gone {
        println!("  gone from the inbox: {g}");
    }
    for n in &r.notes {
        println!("  {n}");
    }
    println!("{}", r.summary());
}

fn ingest(data: &Path, cmd: Ingest) -> Result<()> {
    match cmd {
        Ingest::Inbox { reimport, meta } => {
            let mut sink = Sink::open(data)?;
            let cache = store::cache_dir();
            let report = match reimport {
                Some(target) => inbox::reimport(&mut sink, data, &cache, &meta, &target)?,
                None => inbox::import(&mut sink, data, &cache, &meta)?,
            };
            print_report(&report);
            sink.finish()
        }
        Ingest::Url {
            urls,
            list,
            sitemap,
            all_tabs,
            max_pages,
            delay_s,
            meta,
        } => {
            let mut all = urls;
            if let Some(list) = list {
                let text = std::fs::read_to_string(&list)
                    .with_context(|| format!("reading {}", list.display()))?;
                all.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty() && !l.starts_with('#'))
                        .map(String::from),
                );
            }
            let web = ingest::Web {
                urls: all,
                sitemap,
                max_pages,
                delay_s,
                all_tabs,
            };
            let mut sink = Sink::open(data)?;
            ingest::web(&mut sink, &web, &meta)?;
            sink.finish()
        }
        Ingest::Wiki { wiki, meta } => {
            let mut sink = Sink::open(data)?;
            cuttlefish::wiki::wiki(&mut sink, &wiki, &meta)?;
            sink.finish()
        }
        Ingest::Site { site, meta } => {
            let mut sink = Sink::open(data)?;
            cuttlefish::wiki::site(&mut sink, &site, &meta)?;
            sink.finish()
        }
        Ingest::Youtube {
            url,
            sub_langs,
            max,
            meta,
        } => {
            let mut sink = Sink::open(data)?;
            ingest::youtube(&mut sink, &url, &sub_langs, max, &meta)?;
            sink.finish()
        }
        Ingest::DiscordExport { files, whole, meta } => {
            let mut sink = Sink::open(data)?;
            ingest::discord_export(&mut sink, &files, whole, &meta)?;
            sink.finish()
        }
        Ingest::DiscordBot {
            channel,
            no_threads,
            meta,
        } => {
            let bot = discord::Bot::from_env()?;
            let mut sink = Sink::open(data)?;
            ingest::discord_bot(&mut sink, &bot, &channel, !no_threads, &meta)?;
            sink.finish()
        }
        Ingest::File { paths, url, meta } => {
            if paths.is_empty() {
                bail!("no files given");
            }
            let mut sink = Sink::open(data)?;
            let cache = cuttlefish::store::cache_dir();
            ingest::files(&mut sink, data, &cache, &paths, url.as_deref(), &meta)?;
            sink.finish()
        }
        Ingest::Leanny {
            dry_run,
            no_weapons,
            delay_s,
            meta,
        } => {
            let options = leanny::Options {
                dry_run,
                refresh: meta.refresh,
                weapons: !no_weapons,
            };
            let mut fetcher = Fetcher::new(core::time::Duration::from_secs_f32(delay_s.max(1.0)));
            if dry_run {
                // Nothing is fetched or written: no lock, no embedder
                let mut sink = Notes;
                let summary = leanny::ingest(&mut sink, data, &mut fetcher, &options, &meta)?;
                println!("{summary}");
                return Ok(());
            }
            let mut sink = Sink::open(data)?;
            let summary = leanny::ingest(&mut sink, data, &mut fetcher, &options, &meta)?;
            println!("{summary}");
            sink.finish()
        }
    }
}

/// A sink for a dry run: prints the notes, stores nothing
struct Notes;

impl ingest::Sink for Notes {
    fn has(&self, _: &str) -> bool {
        false
    }

    fn add(&mut self, doc: &Document) -> Result<usize> {
        bail!("a dry run stores nothing ({})", doc.title)
    }

    fn raw_dir(&self, kind: &str) -> PathBuf {
        PathBuf::from(kind)
    }

    fn note(&mut self, line: &str) {
        println!("{line}");
    }
}
