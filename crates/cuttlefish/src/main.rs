//! `cuttlefish`: fill the knowledge store, search it and ask the model.
//!
//! Run `cuttlefish --help` for the commands. Keys come from the
//! environment only: `ANTHROPIC_API_KEY` (ask, translate) and
//! `DISCORD_BOT_TOKEN` (ingest discord-bot).

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use cuttlefish::discord;
use cuttlefish::doc::Document;
use cuttlefish::embed::E5Embedder;
use cuttlefish::eval::EvalSet;
use cuttlefish::ingest::{self, Meta};
use cuttlefish::llm::{Client, Settings};
use cuttlefish::review::{Reviewer, translate};
use cuttlefish::store::Store;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about = "Cuttlefish: Salmon Run knowledge store and AI reviewer")]
struct Cli {
    /// Data folder (default: $CUTTLEFISH_DATA or ~/.local/share/cuttlefish)
    #[arg(long, global = true)]
    data: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Import sources into the store
    #[command(subcommand)]
    Ingest(Ingest),
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
    /// Ask Cuttlefish a question (needs ANTHROPIC_API_KEY)
    Ask {
        /// The question
        question: String,
        #[command(flatten)]
        model: ModelArgs,
        /// Knowledge excerpts to retrieve
        #[arg(short, default_value_t = 8)]
        k: usize,
    },
    /// Translate text with the community's names (needs ANTHROPIC_API_KEY)
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
        /// Also ask the model and check the answers (needs ANTHROPIC_API_KEY)
        #[arg(long)]
        answer: bool,
        #[command(flatten)]
        model: ModelArgs,
    },
    /// Documents per source and chunks in the index
    Stats,
    /// Re-chunk and re-embed every stored document
    Reindex,
}

#[derive(Args)]
struct ModelArgs {
    /// Model name
    #[arg(long, env = "CUTTLEFISH_MODEL", default_value = cuttlefish::llm::DEFAULT_MODEL)]
    model: String,
    /// Effort: low, medium, high, xhigh, max
    #[arg(long, default_value = cuttlefish::llm::DEFAULT_EFFORT)]
    effort: String,
}

impl ModelArgs {
    fn settings(&self) -> Settings {
        Settings {
            model: self.model.clone(),
            effort: self.effort.clone(),
            ..Settings::default()
        }
    }
}

#[derive(Subcommand)]
enum Ingest {
    /// Web pages: urls, a list file, a sitemap, or MediaWiki categories
    Url {
        /// Page urls
        urls: Vec<String>,
        /// File with one url per line (# starts a comment)
        #[arg(long)]
        list: Option<PathBuf>,
        /// Sitemap (or sitemap index) url
        #[arg(long)]
        sitemap: Option<String>,
        /// MediaWiki api.php url, used with --category
        #[arg(long)]
        mediawiki: Option<String>,
        /// MediaWiki category to import (repeatable)
        #[arg(long, requires = "mediawiki")]
        category: Vec<String>,
        /// At most this many pages
        #[arg(long, default_value_t = 200)]
        max_pages: usize,
        /// Seconds between requests to one site (robots.txt may ask more)
        #[arg(long, default_value_t = 3.0)]
        delay_s: f32,
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

/// Adds documents to the store, saving the index every few documents
struct Sink {
    store: Store,
    embedder: E5Embedder,
    added: usize,
}

impl Sink {
    fn open(data: &Path) -> Result<Self> {
        let embedder = E5Embedder::load(&Store::models_dir(data))?;
        let store = Store::open(data, &embedder)?;
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

    fn add(&mut self, doc: &Document) -> Result<usize> {
        let n = self.store.add(doc, &self.embedder)?;
        self.added += 1;
        if self.added.is_multiple_of(10) {
            self.store.save()?;
        }
        Ok(n)
    }

    fn raw_dir(&self, kind: &str) -> PathBuf {
        self.store.raw_dir(kind)
    }

    fn note(&mut self, line: &str) {
        if line.starts_with("skipped") {
            log::warn!("{line}");
        } else {
            println!("{line}");
        }
    }
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    let data = cli.data.clone().unwrap_or_else(Store::default_root);
    match cli.command {
        Command::Ingest(i) => ingest(&data, i),
        Command::Search { query, k } => {
            let embedder = E5Embedder::load(&Store::models_dir(&data))?;
            let store = Store::open(&data, &embedder)?;
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
            let embedder = E5Embedder::load(&Store::models_dir(&data))?;
            let store = Store::open(&data, &embedder)?;
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
            let embedder = E5Embedder::load(&Store::models_dir(&data))?;
            let store = Store::open(&data, &embedder)?;
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
            Ok(())
        }
        Command::Reindex => {
            let index = data.join("index");
            if index.exists() {
                std::fs::remove_dir_all(&index)?;
            }
            let embedder = E5Embedder::load(&Store::models_dir(&data))?;
            let mut store = Store::open(&data, &embedder)?;
            let n = store.reindex(&embedder)?;
            store.save()?;
            println!("{n} chunks indexed");
            Ok(())
        }
    }
}

fn ingest(data: &Path, cmd: Ingest) -> Result<()> {
    match cmd {
        Ingest::Url {
            urls,
            list,
            sitemap,
            mediawiki,
            category,
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
                mediawiki,
                categories: category,
                max_pages,
                delay_s,
            };
            let mut sink = Sink::open(data)?;
            ingest::web(&mut sink, &web, &meta)?;
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
            ingest::files(&mut sink, &paths, url.as_deref(), &meta)?;
            sink.finish()
        }
    }
}
