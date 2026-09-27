//! Importing sources into the store: web pages (urls, sitemaps), YouTube
//! transcripts, Discord conversations and local files; whole wikis and
//! sites are [`crate::wiki`]'s.
//!
//! Each importer turns its source into [`Document`]s and hands them to a
//! [`Sink`], which stores them (the CLI prints, the studio keeps a job log).
//! Sources already stored are skipped unless [`Meta::refresh`] is set; a
//! page or video that fails is reported and skipped, so one bad url does
//! not stop an import. So is a page that is only the shell of a JavaScript
//! app ([`html::shell_reason`]); Google Docs, Sheets and Slides are read
//! through their exports instead ([`google`]), a sheet with names in
//! several languages becoming a name table of the glossary. A sheet's
//! address without a `gid` (or any, with [`Web::all_tabs`]) brings every
//! tab.

use crate::crawl::{Fetcher, sitemap_locs};
use crate::doc::{Document, SourceKind, doc_id, guess_language};
use crate::google::{self, Format, GoogleFile};
use crate::inbox::{self, INBOX};
use crate::tables::{self, Member, Table};
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
    /// Whether the name table of this url (a Google Sheet's tab) is stored
    /// already ([`crate::tables::has`])
    fn has_table(&self, _key: &str) -> bool {
        false
    }
    /// The source revision the stored document of this url was made from
    /// ([`Document::revision`]), if any
    fn revision(&self, _key: &str) -> Option<u64> {
        None
    }
    /// Store a document; returns its number of chunks
    fn add(&mut self, doc: &Document) -> Result<usize>;
    /// Remove the document with this id (an inbox file imported again);
    /// false when there is none, or when the sink keeps nothing to remove
    fn delete(&mut self, _id: &str) -> Result<bool> {
        Ok(false)
    }
    /// Folder for raw downloads of one kind (`web`, `wiki`, `youtube`,
    /// `discord`)
    fn raw_dir(&self, kind: &str) -> PathBuf;
    /// A line of progress: a document added, a page skipped, an error
    fn note(&mut self, line: &str);
    /// Store a name table (a Google Sheet's names in several languages)
    /// and merge it into the glossary
    fn add_table(&mut self, _table: &Table) -> Result<()> {
        bail!("name tables are not kept by this import")
    }
    /// `done` of `total` items handled
    fn progress(&mut self, _done: usize, _total: usize) {}
    /// Whether to stop before the next item
    fn cancelled(&self) -> bool {
        false
    }
}

/// Apply `meta` and add the document, noting it
pub(crate) fn add(sink: &mut dyn Sink, mut doc: Document, meta: &Meta) -> Result<()> {
    meta.apply(&mut doc);
    let n = sink.add(&doc)?;
    sink.note(&alloc::format!("+ {} ({n} chunks)", doc.title));
    Ok(())
}

/// Stop if the sink asks to
pub(crate) fn check(sink: &dyn Sink) -> Result<()> {
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
    /// At most this many pages from the urls and the sitemap
    pub max_pages: usize,
    /// Seconds between requests to one site (robots.txt may ask more); at
    /// least 1
    pub delay_s: f32,
    /// Every tab of a Google Sheet, even when its address names one
    pub all_tabs: bool,
}

impl Default for Web {
    fn default() -> Self {
        Web {
            urls: Vec::new(),
            sitemap: None,
            max_pages: 200,
            delay_s: 3.0,
            all_tabs: false,
        }
    }
}

/// Import web pages and a sitemap's pages, politely (robots.txt, one
/// request per site every `delay_s`); returns the number of documents added
pub fn web(sink: &mut dyn Sink, web: &Web, meta: &Meta) -> Result<usize> {
    let mut fetcher = Fetcher::new(Duration::from_secs_f32(web.delay_s.max(1.0)));
    let mut added = 0;
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
    let all = sheet_tabs(sink, &mut fetcher, all, web.all_tabs)?;
    if !all.is_empty() {
        sink.note(&alloc::format!("{} pages to fetch", all.len()));
    }
    for (i, url) in all.iter().enumerate() {
        check(sink)?;
        sink.progress(i, all.len());
        if !meta.refresh && (sink.has(url) || sink.has_table(url)) {
            sink.note(&alloc::format!("already stored: {url}"));
            continue;
        }
        let raw = sink.raw_dir("web");
        let got = match google::recognise(url) {
            Some(file) => fetch_google(&mut fetcher, &raw, url, &file),
            None => fetch_page(&mut fetcher, &raw, url).map(Got::Doc),
        };
        match got {
            Ok(Got::Doc(doc)) => {
                add(sink, doc, meta)?;
                added += 1;
            }
            Ok(Got::Table(table)) => {
                sink.add_table(&table)?;
                sink.note(&alloc::format!(
                    "+ {url} ({} terms in {} into the glossary)",
                    table.terms.len(),
                    table.languages.join(", ")
                ));
            }
            Err(e) => sink.note(&alloc::format!("skipped {url}: {e:#}")),
        }
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

/// Whether a local path is taken through the inbox: a folder, or an
/// archive by its name or first bytes
fn through_inbox(path: &Path) -> bool {
    if path.is_dir() {
        return true;
    }
    let name = path.to_string_lossy();
    let head = inbox::head(path, 4096);
    inbox::classify(&name, || head.clone()) == inbox::Route::Archive
        || (inbox::is_archive(&head) && file::is_binary(&head))
}

/// Import local markdown, text, HTML, PDF or Word files, citing `url` if
/// given. Folders and archives are put into the inbox of the data folder
/// `root` and taken as the inbox takes them ([`inbox::take_in`],
/// [`inbox::import`], unpacked in `cache`); binary files are refused. A
/// file that fails is reported and skipped; the import fails only when
/// nothing could be taken. Returns the documents added.
pub fn files(
    sink: &mut dyn Sink,
    root: &Path,
    cache: &Path,
    paths: &[PathBuf],
    url: Option<&str>,
    meta: &Meta,
) -> Result<usize> {
    if paths.is_empty() {
        bail!("no files given");
    }
    let mut added = 0;
    let mut taken_in = 0;
    let mut failed = None;
    for (i, p) in paths.iter().enumerate() {
        check(sink)?;
        sink.progress(i, paths.len());
        if through_inbox(p) {
            match inbox::take_in(root, p) {
                Ok((rel, n)) => {
                    sink.note(&alloc::format!(
                        "{} put into the inbox as {INBOX}/{rel} ({n} files copied)",
                        p.display()
                    ));
                    taken_in += 1;
                }
                Err(e) => {
                    sink.note(&alloc::format!("skipped {}: {e:#}", p.display()));
                    failed = Some(e);
                }
            }
            continue;
        }
        let doc = file::load(p, meta.source.unwrap_or(SourceKind::File))
            .with_context(|| alloc::format!("reading {}", p.display()));
        match doc {
            Ok(mut doc) => {
                if let Some(url) = url {
                    doc.url = Some(String::from(url));
                }
                add(sink, doc, meta)?;
                added += 1;
            }
            Err(e) => {
                sink.note(&alloc::format!("skipped: {e:#}"));
                failed = Some(e);
            }
        }
    }
    if taken_in > 0 {
        check(sink)?;
        sink.note("importing the inbox (folders and archives are unpacked and sorted there)");
        let report = inbox::import(sink, root, cache, meta)?;
        sink.note(&report.summary());
        added += report.count("document");
    } else if added == 0
        && let Some(e) = failed
    {
        return Err(e);
    }
    Ok(added)
}

/// The urls with each Google Sheet among them replaced by its tabs' (when
/// its address names no tab, or with `all`), listed by the sheet's HTML
/// view; a sheet whose tabs cannot be listed stays as it is
fn sheet_tabs(
    sink: &mut dyn Sink,
    fetcher: &mut Fetcher,
    urls: Vec<String>,
    all: bool,
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for url in urls {
        let Some(GoogleFile::Sheet { id, gid }) = google::recognise(&url) else {
            out.push(url);
            continue;
        };
        if gid.is_some() && !all {
            out.push(url);
            continue;
        }
        check(sink)?;
        let tabs = fetcher
            .get(&google::tabs_url(&id))
            .map(|html| google::tabs(&html))
            .unwrap_or_default();
        if tabs.is_empty() {
            out.push(url);
            continue;
        }
        let names: Vec<&str> = tabs.iter().map(|t| t.name.as_str()).collect();
        sink.note(&alloc::format!(
            "{} tabs in the sheet: {}",
            tabs.len(),
            names.join(", ")
        ));
        out.extend(tabs.iter().map(|t| google::tab_url(&id, &t.gid)));
    }
    Ok(out)
}

/// Keeps what was downloaded, named by the document id
pub(crate) fn save_raw(dir: &Path, key: &str, ext: &str, body: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(alloc::format!("{}.{ext}", doc_id(key)));
    std::fs::write(&path, body)?;
    Ok(path)
}

/// What a url gave
enum Got {
    Doc(Document),
    /// A sheet's names in several languages
    Table(Table),
}

/// A web page as a document; fails with the reason when it is not worth
/// keeping (an error, or the shell of a page drawn with JavaScript)
fn fetch_page(fetcher: &mut Fetcher, raw: &Path, url: &str) -> Result<Document> {
    let body = fetcher.get(url)?;
    save_raw(raw, url, "html", body.as_bytes())?;
    let page = html::convert(&body);
    if let Some(why) = html::shell_reason(body.len(), &page.text) {
        bail!("{why}");
    }
    let title = page.title.clone().unwrap_or_else(|| String::from(url));
    let mut doc = Document::new(SourceKind::Web, url, title, page.text);
    doc.url = Some(String::from(url));
    doc.license = page.license;
    doc.language = page
        .language
        .or_else(|| guess_language(&doc.text).map(String::from));
    Ok(doc)
}

/// A Google Doc, Sheet or Slides through its exports ([`google`]), tried
/// in order; stored under the address the user gave. Fails with the
/// reason when none gives text (not shared publicly, not found).
fn fetch_google(fetcher: &mut Fetcher, raw: &Path, url: &str, file: &GoogleFile) -> Result<Got> {
    let mut last = anyhow::anyhow!("no export of the {} gave text", file.kind());
    for (export, format) in file.exports() {
        let got = match fetcher.fetch(&export) {
            Ok(got) => got,
            Err(e) => {
                last = e;
                continue;
            }
        };
        if let Some(why) = google::access_error(got.status, &got.final_url, got.is_html()) {
            if got.status == 404 {
                last = anyhow::anyhow!("{why}");
                continue;
            }
            bail!("{why}");
        }
        if !got.ok() {
            last = anyhow::anyhow!("{export}: HTTP {}", got.status);
            continue;
        }
        let path = save_raw(raw, url, format.ext(), &got.body)?;
        let text = match format {
            Format::Markdown => google::clean_markdown(&got.text()),
            Format::Text => String::from(got.text().trim_start_matches('\u{feff}').trim()),
            Format::Docx => file::read(&path)?.1,
            Format::Csv => {
                let member = Member {
                    file: String::from(url),
                    leaves: tables::read(&path)?.leaves,
                    ..Member::default()
                };
                let table = tables::build(url, &[member]);
                if !table.terms.is_empty() {
                    return Ok(Got::Table(table));
                }
                if got.body.len() as u64 > tables::MAX_TEXT {
                    bail!("a sheet over 1 MB without names in several languages");
                }
                tables::as_text(&path)?
            }
        };
        if text.chars().count() < 40 {
            last = anyhow::anyhow!(
                "the {} export of the {} is empty",
                format.ext(),
                file.kind()
            );
            continue;
        }
        let title = got
            .file_name
            .as_deref()
            .map(google::title_of)
            .filter(|t| !t.is_empty())
            .or_else(|| {
                text.lines()
                    .find_map(|l| l.strip_prefix("# "))
                    .map(|t| String::from(t.trim()))
            })
            .unwrap_or_else(|| String::from(file.kind()));
        let mut doc = Document::new(SourceKind::Web, url, title, text);
        doc.url = Some(String::from(url));
        return Ok(Got::Doc(doc));
    }
    Err(last)
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
        /// Urls stored as name tables
        tables: Vec<String>,
    }

    impl Sink for Memory {
        fn has(&self, key: &str) -> bool {
            self.docs.iter().any(|d| d.id == doc_id(key))
        }
        fn has_table(&self, key: &str) -> bool {
            self.tables.iter().any(|t| t == key)
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
    fn stored_name_tables_are_not_fetched_again() {
        // A sheet's tab that became a name table: no request at all
        let tab = "https://docs.google.com/spreadsheets/d/abc/edit#gid=7";
        let mut sink = Memory {
            tables: alloc::vec![String::from(tab)],
            ..Default::default()
        };
        let pages = Web {
            urls: alloc::vec![String::from(tab)],
            ..Web::default()
        };
        assert_eq!(web(&mut sink, &pages, &Meta::default()).unwrap(), 0);
        assert!(
            sink.notes
                .iter()
                .any(|n| n == &alloc::format!("already stored: {tab}"))
        );
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
        let n = files(
            &mut sink,
            &dir,
            &dir,
            &[a.clone(), b.clone()],
            Some("https://x"),
            &meta,
        )
        .unwrap();
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
        let err = files(&mut stopping, &dir, &dir, &[a, b], None, &Meta::default()).unwrap_err();
        assert_eq!(err.to_string(), "cancelled");
        assert_eq!(stopping.docs.len(), 1);
        assert!(files(&mut stopping, &dir, &dir, &[], None, &Meta::default()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn folders_and_archives_go_through_the_inbox_and_binaries_are_refused() {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-ingest-inbox-{}",
            std::process::id()
        ));
        let data = dir.join("data");
        let guides = dir.join("guides");
        std::fs::create_dir_all(guides.join("node_modules")).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            guides.join("eggs.md"),
            "# Eggs\n\nBank the golden eggs before the tide changes, every wave.",
        )
        .unwrap();
        std::fs::write(guides.join("node_modules/x.md"), "# Not this").unwrap();
        let zip = dir.join("stat.ink-3.128.4.zip");
        std::fs::write(&zip, b"PK\x03\x04\x14\x00\x00\x00\x08\x00garbage\x00\xff").unwrap();
        let unnamed = dir.join("dump");
        std::fs::write(&unnamed, b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\xff").unwrap();
        let blob = dir.join("blob.dat");
        std::fs::write(&blob, b"\x00\x01\x02binary\xff\xfe").unwrap();
        assert!(through_inbox(&guides));
        assert!(through_inbox(&zip));
        assert!(through_inbox(&unnamed));
        assert!(!through_inbox(&blob));
        assert!(!through_inbox(&guides.join("eggs.md")));

        let mut sink = Memory {
            root: dir.clone(),
            ..Default::default()
        };
        // A binary file alone fails with the reason
        let err = files(
            &mut sink,
            &data,
            &dir,
            core::slice::from_ref(&blob),
            None,
            &Meta::default(),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("binary data"), "{err:#}");
        assert!(sink.docs.is_empty());

        // A folder is copied into the inbox (without node_modules) and read
        let n = files(
            &mut sink,
            &data,
            &dir,
            &[guides, blob],
            None,
            &Meta::default(),
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(sink.docs[0].title, "Eggs");
        assert_eq!(sink.docs[0].path.as_deref(), Some("inbox/guides/eggs.md"));
        assert!(data.join("inbox/guides/eggs.md").is_file());
        assert!(!data.join("inbox/guides/node_modules").exists());
        assert!(sink.notes.iter().any(|l| l.contains("binary data")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn web_defaults() {
        let web: Web = serde_json::from_str(r#"{"urls": ["https://a"]}"#).unwrap();
        assert_eq!(web.max_pages, 200);
        assert_eq!(web.delay_s, 3.0);
        assert!(web.sitemap.is_none() && !web.all_tabs);
    }
}
