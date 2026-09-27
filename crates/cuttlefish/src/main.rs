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
use cuttlefish::discord;
use cuttlefish::discord_fetch::{
    self, Browser, Https, Interruptible, Options, Pace, Range, TOKEN_VAR,
};
use cuttlefish::doc::Document;
use cuttlefish::embed::E5Embedder;
use cuttlefish::eval::EvalSet;
use cuttlefish::ingest::{self, Meta};
use cuttlefish::llm::{Backend, Client, Settings};
use cuttlefish::review::{Reviewer, translate};
use cuttlefish::store::{self, Store};
use cuttlefish::{assets, env_file, inbox, tables};
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
    /// Show the chunks closest to a query
    Search {
        /// What to look for, in any language
        query: String,
        /// Number of results
        #[arg(short, default_value_t = 8)]
        k: usize,
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
    /// Run an evaluation file (see `eval.example.toml`)
    Eval {
        /// Evaluation file
        file: PathBuf,
        /// Knowledge excerpts to retrieve
        #[arg(short, default_value_t = 8)]
        k: usize,
        /// Also ask the model and check the answers (needs a model backend)
        #[arg(long)]
        answer: bool,
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

/// Adds documents to the store, saving the index every few documents
struct Sink {
    store: Store,
    embedder: E5Embedder,
    added: usize,
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
    }
    Ok((store, embedder))
}

impl Sink {
    fn open(data: &Path) -> Result<Self> {
        let (store, embedder) = open(data, true)?;
        Ok(Sink {
            store,
            embedder,
            added: 0,
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
        Command::Ingest(i) => ingest(&data, i),
        Command::Search { query, k } => {
            let (store, embedder) = open(&data, true)?;
            let hits = store.search(&query, k, &embedder)?;
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
            file,
            k,
            answer,
            model,
        } => {
            let set = EvalSet::parse(
                &std::fs::read_to_string(&file)
                    .with_context(|| format!("reading {}", file.display()))?,
            )?;
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
            println!("retrieval: {found}/{} cases", set.cases.len());
            if answer {
                println!("answers: {points}/{expected} expected points");
            }
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
            delay,
            pause_every,
            pause,
            daily_cap,
            max_requests,
            max_minutes,
        } => {
            // The channels' own folder, or <knowledge>/inbox/discord with a
            // folder per channel below it
            let (root, flat) = match out {
                Some(out) => (out, true),
                None => (
                    knowledge_folder(data, config)?
                        .join(inbox::INBOX)
                        .join("discord"),
                    false,
                ),
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
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
                .context("setting the Ctrl+C handler")?;
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
    }
}
