//! Importing sources into the store: web pages (urls, sitemaps, MediaWiki
//! categories), YouTube transcripts, Discord conversations and local files.
//!
//! Each importer turns its source into [`Document`]s and hands them to a
//! [`Sink`], which stores them (the CLI prints, the studio keeps a job log).
//! Sources already stored are skipped unless [`Meta::refresh`] is set; a
//! page or video that fails is reported and skipped, so one bad url does
//! not stop an import.

use crate::crawl::{Fetcher, MediaWiki, sitemap_locs};
use crate::doc::{Document, SourceKind, doc_id, guess_language};
use crate::{discord, file, html, youtube};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use core::time::Duration;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Metadata overrides for imported documents
#[derive(Clone, Debug, Default, PartialEq, Deserialize, clap::Args)]
#[serde(default)]
pub struct Meta {
    /// Kind of source (sets the default weight)
    #[arg(long, value_enum)]
    pub source: Option<SourceKind>,
    /// License or terms of use to record
    #[arg(long)]
    pub license: Option<String>,
    /// Language code of the text
    #[arg(long)]
    pub lang: Option<String>,
    /// Retrieval weight (default: by source kind)
    #[arg(long)]
    pub weight: Option<f32>,
    /// Import again even if already stored
    #[arg(long)]
    pub refresh: bool,
}

impl Meta {
    /// Apply the overrides to a document
    pub fn apply(&self, doc: &mut Document) {
        if let Some(s) = self.source {
            doc.source = s;
            doc.weight = s.default_weight();
        }
        if let Some(l) = &self.license {
            doc.license = Some(l.clone());
        }
        if let Some(l) = &self.lang {
            doc.language = Some(l.clone());
        }
        if let Some(w) = self.weight {
            doc.weight = w;
        }
    }
}

/// Where imported documents go
pub trait Sink {
    /// Whether the document of this url or path is stored already
    fn has(&self, key: &str) -> bool;
    /// Store a document; returns its number of chunks
    fn add(&mut self, doc: &Document) -> Result<usize>;
    /// Folder for raw downloads of one kind (`web`, `wiki`, `youtube`,
    /// `discord`)
    fn raw_dir(&self, kind: &str) -> PathBuf;
    /// A line of progress: a document added, a page skipped, an error
    fn note(&mut self, line: &str);
    /// `done` of `total` items handled
    fn progress(&mut self, _done: usize, _total: usize) {}
    /// Whether to stop before the next item
    fn cancelled(&self) -> bool {
        false
    }
}

/// Apply `meta` and add the document, noting it
fn add(sink: &mut dyn Sink, mut doc: Document, meta: &Meta) -> Result<()> {
    meta.apply(&mut doc);
    let n = sink.add(&doc)?;
    sink.note(&alloc::format!("+ {} ({n} chunks)", doc.title));
    Ok(())
}

/// Stop if the sink asks to
fn check(sink: &dyn Sink) -> Result<()> {
    if sink.cancelled() {
        bail!("cancelled");
    }
    Ok(())
}

/// Web pages to import
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Web {
    /// Page urls
    pub urls: Vec<String>,
    /// Sitemap (or sitemap index) url
    pub sitemap: Option<String>,
    /// MediaWiki `api.php` url, used with `categories`
    pub mediawiki: Option<String>,
    /// MediaWiki categories to import
    pub categories: Vec<String>,
    /// At most this many pages from the urls and the sitemap, and as many
    /// from the categories
    pub max_pages: usize,
    /// Seconds between requests to one site (robots.txt may ask more); at
    /// least 1
    pub delay_s: f32,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            urls: Vec::new(),
            sitemap: None,
            mediawiki: None,
            categories: Vec::new(),
            max_pages: 200,
            delay_s: 3.0,
        }
    }
}

/// Import web pages, a sitemap's pages and MediaWiki categories, politely
/// (robots.txt, one request per site every `delay_s`); returns the number
/// of documents added
pub fn web(sink: &mut dyn Sink, web: &Web, meta: &Meta) -> Result<usize> {
    let mut fetcher = Fetcher::new(Duration::from_secs_f32(web.delay_s.max(1.0)));
    let mut added = 0;
    if let Some(api) = &web.mediawiki {
        let wiki = MediaWiki { api: api.clone() };
        let info = wiki.site_info(&mut fetcher)?;
        let mut titles = Vec::new();
        for c in &web.categories {
            titles.extend(wiki.category(&mut fetcher, c)?);
        }
        titles.truncate(web.max_pages);
        sink.note(&alloc::format!("{} wiki pages listed", titles.len()));
        for (i, title) in titles.iter().enumerate() {
            check(sink)?;
            sink.progress(i, titles.len());
            let url = info.page_url(title);
            if !meta.refresh && sink.has(&url) {
                continue;
            }
            let page = match wiki.page(&mut fetcher, title) {
                Ok(p) => p,
                Err(e) => {
                    sink.note(&alloc::format!("skipped {title}: {e:#}"));
                    continue;
                }
            };
            save_raw(&sink.raw_dir("wiki"), &url, "html", &page.html)?;
            let text = html::fragment_text(&page.html);
            let mut doc = Document::new(SourceKind::Wiki, &url, page.title, text);
            doc.url = Some(url);
            doc.license = info.license.clone();
            add(sink, doc, meta)?;
            added += 1;
        }
    }
    let mut all = web.urls.clone();
    if let Some(sitemap) = &web.sitemap {
        let mut queue = alloc::vec![sitemap.clone()];
        while let Some(s) = queue.pop() {
            check(sink)?;
            if all.len() >= web.max_pages {
                break;
            }
            let (pages, nested) = sitemap_locs(&fetcher.get(&s)?);
            all.extend(pages);
            queue.extend(nested);
        }
    }
    all.truncate(web.max_pages);
    if !all.is_empty() {
        sink.note(&alloc::format!("{} pages to fetch", all.len()));
    }
    for (i, url) in all.iter().enumerate() {
        check(sink)?;
        sink.progress(i, all.len());
        if !meta.refresh && sink.has(url) {
            sink.note(&alloc::format!("already stored: {url}"));
            continue;
        }
        let body = match fetcher.get(url) {
            Ok(b) => b,
            Err(e) => {
                sink.note(&alloc::format!("skipped: {e:#}"));
                continue;
            }
        };
        save_raw(&sink.raw_dir("web"), url, "html", &body)?;
        let page = html::convert(&body);
        let title = page.title.clone().unwrap_or_else(|| url.clone());
        let mut doc = Document::new(SourceKind::Web, url, title, page.text);
        doc.url = Some(url.clone());
        doc.license = page.license;
        doc.language = page
            .language
            .or_else(|| guess_language(&doc.text).map(String::from));
        add(sink, doc, meta)?;
        added += 1;
    }
    Ok(added)
}

/// yt-dlp's default subtitle languages for [`youtube`]
pub const SUB_LANGS: &str = "en,ja,zh-Hans,zh-Hant,es,fr,ru,en-orig,ja-orig";

/// Import the transcripts of a YouTube video, playlist or channel (at most
/// `max` videos) through yt-dlp; returns the number of documents added
pub fn youtube(
    sink: &mut dyn Sink,
    url: &str,
    sub_langs: &str,
    max: usize,
    meta: &Meta,
) -> Result<usize> {
    let raw = sink.raw_dir("youtube");
    let mut ids = youtube::list_ids(url)?;
    ids.truncate(max);
    sink.note(&alloc::format!("{} videos", ids.len()));
    let mut added = 0;
    for (i, id) in ids.iter().enumerate() {
        check(sink)?;
        sink.progress(i, ids.len());
        let key = alloc::format!("https://www.youtube.com/watch?v={id}");
        if !meta.refresh && sink.has(&key) {
            sink.note(&alloc::format!("already stored: {key}"));
            continue;
        }
        match youtube::fetch(id, &raw, sub_langs) {
            Ok(doc) => {
                add(sink, doc, meta)?;
                added += 1;
            }
            Err(e) => sink.note(&alloc::format!("skipped {key}: {e:#}")),
        }
    }
    Ok(added)
}

/// Import DiscordChatExporter JSON exports; `whole` keeps each file as one
/// conversation (a thread or forum post). Returns the documents added.
pub fn discord_export(
    sink: &mut dyn Sink,
    files: &[PathBuf],
    whole: bool,
    meta: &Meta,
) -> Result<usize> {
    let raw = sink.raw_dir("discord");
    std::fs::create_dir_all(&raw)?;
    let mut added = 0;
    for (i, f) in files.iter().enumerate() {
        check(sink)?;
        sink.progress(i, files.len());
        let json = std::fs::read_to_string(f)
            .with_context(|| alloc::format!("reading {}", f.display()))?;
        let (channel, messages) =
            discord::parse_export(&json).with_context(|| alloc::format!("in {}", f.display()))?;
        std::fs::write(raw.join(f.file_name().context("not a file")?), &json)?;
        for doc in discord::to_documents(&channel, &messages, whole) {
            add(sink, doc, meta)?;
            added += 1;
        }
    }
    Ok(added)
}

/// Import Discord channels through the bot API, with their threads and
/// forum posts unless `threads` is false. Returns the documents added.
pub fn discord_bot(
    sink: &mut dyn Sink,
    bot: &discord::Bot,
    channels: &[String],
    threads: bool,
    meta: &Meta,
) -> Result<usize> {
    let raw = sink.raw_dir("discord");
    std::fs::create_dir_all(&raw)?;
    let mut added = 0;
    for id in channels {
        check(sink)?;
        let ch = bot.channel(id)?;
        // (channel, whole conversation)
        let mut targets = alloc::vec![(ch.clone(), false)];
        if threads {
            for t in bot.threads(&ch)? {
                targets.push((bot.channel(&t)?, true));
            }
        }
        for (c, whole) in targets {
            check(sink)?;
            sink.note(&alloc::format!("reading #{}", c.name));
            let messages = bot.messages(&c.id)?;
            let raw_json = serde_json::to_vec_pretty(&(&c, &messages))?;
            std::fs::write(raw.join(alloc::format!("bot-{}.json", c.id)), raw_json)?;
            for doc in discord::to_documents(&c, &messages, whole) {
                add(sink, doc, meta)?;
                added += 1;
            }
        }
    }
    Ok(added)
}

/// Import local markdown, text, HTML or PDF files, citing `url` if given.
/// Returns the documents added.
pub fn files(
    sink: &mut dyn Sink,
    paths: &[PathBuf],
    url: Option<&str>,
    meta: &Meta,
) -> Result<usize> {
    if paths.is_empty() {
        bail!("no files given");
    }
    for (i, p) in paths.iter().enumerate() {
        check(sink)?;
        sink.progress(i, paths.len());
        let mut doc = file::load(p, meta.source.unwrap_or(SourceKind::File))?;
        if let Some(url) = url {
            doc.url = Some(String::from(url));
        }
        add(sink, doc, meta)?;
    }
    Ok(paths.len())
}

/// Keeps what was downloaded, named by the document id
fn save_raw(dir: &Path, key: &str, ext: &str, body: &str) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(alloc::format!("{}.{ext}", doc_id(key))), body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps documents in memory
    #[derive(Default)]
    struct Memory {
        docs: Vec<Document>,
        notes: Vec<String>,
        root: PathBuf,
        stop_after: Option<usize>,
    }

    impl Sink for Memory {
        fn has(&self, key: &str) -> bool {
            self.docs.iter().any(|d| d.id == doc_id(key))
        }
        fn add(&mut self, doc: &Document) -> Result<usize> {
            self.docs.push(doc.clone());
            Ok(1)
        }
        fn raw_dir(&self, kind: &str) -> PathBuf {
            self.root.join(kind)
        }
        fn note(&mut self, line: &str) {
            self.notes.push(String::from(line));
        }
        fn cancelled(&self) -> bool {
            self.stop_after.is_some_and(|n| self.docs.len() >= n)
        }
    }

    #[test]
    fn files_with_meta_and_cancel() {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-ingest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.md");
        let b = dir.join("b.md");
        std::fs::write(&a, "# Eggs\n\nBank the golden eggs.").unwrap();
        std::fs::write(&b, "# Tides\n\nLow tide moves the basket.").unwrap();
        let meta: Meta =
            serde_json::from_str(r#"{"source": "guide", "license": "by permission"}"#).unwrap();
        let mut sink = Memory {
            root: dir.clone(),
            ..Default::default()
        };
        let n = files(&mut sink, &[a.clone(), b.clone()], Some("https://x"), &meta).unwrap();
        assert_eq!(n, 2);
        assert_eq!(sink.docs[0].title, "Eggs");
        assert_eq!(sink.docs[0].source, SourceKind::Guide);
        assert_eq!(sink.docs[0].weight, SourceKind::Guide.default_weight());
        assert_eq!(sink.docs[1].license.as_deref(), Some("by permission"));
        assert_eq!(sink.docs[1].url.as_deref(), Some("https://x"));
        assert!(sink.notes[0].starts_with("+ Eggs"));

        let mut stopping = Memory {
            root: dir.clone(),
            stop_after: Some(1),
            ..Default::default()
        };
        let err = files(&mut stopping, &[a, b], None, &Meta::default()).unwrap_err();
        assert_eq!(err.to_string(), "cancelled");
        assert_eq!(stopping.docs.len(), 1);
        assert!(files(&mut stopping, &[], None, &Meta::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn web_defaults() {
        let web: Web = serde_json::from_str(r#"{"urls": ["https://a"]}"#).unwrap();
        assert_eq!(web.max_pages, 200);
        assert_eq!(web.delay_s, 3.0);
        assert!(web.sitemap.is_none());
    }
}
