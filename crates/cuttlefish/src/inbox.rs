//! The inbox: `<data>/inbox/`, where anything can be dropped (files,
//! folders, zip or tar archives, git repositories), and the import that
//! digests it.
//!
//! Each file is classified by name and, where needed, its first bytes
//! ([`classify`]) and taken as:
//!
//! - **prose** (markdown, text, HTML, PDF, Word `.docx`, subtitles): a
//!   document, chunked and embedded like any other ([`crate::file`]);
//! - **a Discord export** (DiscordChatExporter JSON): its conversations;
//! - **a structured file** (JSON, YAML, TOML, CSV, TSV, `.po`,
//!   `.properties`): read for names in several languages, which go into the
//!   glossary with their file and key ([`crate::tables`]); never embedded;
//! - **an image or icon**: an entry of the asset catalogue
//!   ([`crate::assets`]);
//! - **an archive**: unpacked into the local cache with `bsdtar` and its
//!   files taken the same way;
//! - anything else (source code, binaries, media, fonts, office files that
//!   are not `.docx`, unknown formats) is skipped with the reason.
//!
//! Hidden files and folders (`.git`, partial uploads) and dependency or
//! build folders (`node_modules`, `target`, `dist`, ...) are not entered, so
//! a git repository gives only its text, string and locale files.
//!
//! Files are remembered in `<data>/inbox.json` by path, size, time and
//! content hash: a file seen before and unchanged is not read again, a
//! changed file replaces what it gave, and a file with the same content as
//! another is skipped as a copy. Files never leave the inbox; the documents
//! of files removed from it stay until deleted. Each import writes a
//! [`Report`] to `<data>/reports/`.

use crate::assets::{self, Asset, Catalogue};
use crate::discord;
use crate::doc::{Document, SourceKind, doc_id};
use crate::ingest::{Meta, Sink};
use crate::store::{Store, write_atomic};
use crate::tables::{self, Member};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The inbox folder's name in the data folder
pub const INBOX: &str = "inbox";

/// Largest prose file read
const MAX_PROSE: u64 = 100 << 20;

/// Largest image catalogued
const MAX_IMAGE: u64 = 50 << 20;

/// Fewest characters a document must have
const MIN_TEXT: usize = 40;

/// Archives inside archives unpacked, at most
const MAX_DEPTH: usize = 3;

/// Example paths kept per reason a file was skipped
const EXAMPLES: usize = 20;

/// Import reports kept
const REPORTS_KEPT: usize = 30;

/// Folders of dependencies and build output, never entered
pub const SKIP_DIRS: [&str; 14] = [
    "node_modules",
    "bower_components",
    "target",
    "build",
    "dist",
    "out",
    "bin",
    "obj",
    "__pycache__",
    "venv",
    "vendor",
    "coverage",
    "Pods",
    "DerivedData",
];

/// Source code extensions
const CODE: [&str; 44] = [
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cc", "cpp", "hpp", "cxx", "cs",
    "java", "kt", "kts", "go", "rb", "php", "lua", "swift", "sh", "bash", "zsh", "fish", "ps1",
    "bat", "cmd", "dart", "scala", "m", "mm", "jl", "vue", "svelte", "css", "scss", "sass", "less",
    "sql", "gd", "ipynb",
];

/// How a file is taken
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// A document of text
    Prose,
    /// A DiscordChatExporter JSON export
    Discord,
    /// A structured file, read for name tables
    Table,
    /// An image or icon
    Image,
    /// An archive to unpack
    Archive,
    /// Not taken, for this reason
    Skip(&'static str),
}

/// How a file is taken, from its path in the inbox and, for JSON and
/// unknown formats, its first bytes (`head` is called only then)
pub fn classify(rel: &str, head: impl FnOnce() -> Vec<u8>) -> Route {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_lowercase();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext),
        _ => (name.as_str(), ""),
    };
    if [".tar.gz", ".tar.xz", ".tar.bz2", ".tar.zst"]
        .iter()
        .any(|e| name.ends_with(e))
    {
        return Route::Archive;
    }
    if [
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
        "composer.json",
        "deno.json",
        "cargo.toml",
        "pyproject.toml",
        "pubspec.yaml",
        "docker-compose.yml",
        "docker-compose.yaml",
        ".prettierrc.json",
    ]
    .contains(&name.as_str())
    {
        return Route::Skip("project configuration");
    }
    if [
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        "cargo.lock",
        "composer.lock",
        "poetry.lock",
    ]
    .contains(&name.as_str())
    {
        return Route::Skip("lock file");
    }
    let boilerplate = [
        "license",
        "licence",
        "copying",
        "notice",
        "authors",
        "changelog",
        "changes",
        "code_of_conduct",
        "contributing",
        "security",
    ];
    if boilerplate.contains(&stem) {
        return Route::Skip("project boilerplate (license, changelog, ...)");
    }
    match ext {
        "md" | "markdown" | "mdx" | "txt" | "text" | "rst" | "org" | "adoc" | "asciidoc"
        | "html" | "htm" | "xhtml" | "pdf" | "docx" | "srt" | "vtt" => Route::Prose,
        "json" => {
            let head = head();
            let text = String::from_utf8_lossy(&head);
            if text.contains("\"guild\"") && text.contains("\"channel\"") {
                Route::Discord
            } else {
                Route::Table
            }
        }
        "yaml" | "yml" | "toml" | "csv" | "tsv" | "po" | "properties" => Route::Table,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "avif" => Route::Image,
        "zip" | "tar" | "tgz" | "7z" => Route::Archive,
        "rar" => Route::Skip("RAR archive: not read (repack it as zip)"),
        e if CODE.contains(&e) => Route::Skip(
            "source code: not embedded (text, string and locale files beside it are read)",
        ),
        "mp4" | "mkv" | "mov" | "webm" | "avi" | "flv" | "m4v" | "mp3" | "wav" | "ogg" | "flac"
        | "m4a" | "opus" | "aac" => {
            Route::Skip("video or sound: not imported (review videos in Cuttlefish)")
        }
        "doc" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odt" | "odp" | "rtf" | "epub"
        | "pages" | "numbers" | "key" => {
            Route::Skip("office file not read: save it as PDF, docx or text (tables as CSV)")
        }
        "ttf" | "otf" | "woff" | "woff2" | "eot" => Route::Skip("font"),
        "xml" | "plist" | "resx" | "xlf" | "xliff" | "strings" | "msbt" | "bfres" | "sarc"
        | "szs" | "zs" | "bntx" | "byml" | "bgyml" | "bin" => {
            Route::Skip("game data or resource format not read yet")
        }
        "exe" | "dll" | "so" | "dylib" | "o" | "a" | "lib" | "class" | "jar" | "wasm" | "pyc"
        | "pdb" | "apk" | "ipa" | "nro" | "nso" | "nsp" | "xci" | "iso" | "dmg" | "msi" | "deb"
        | "rpm" => Route::Skip("program or binary"),
        _ => {
            let head = head();
            if is_archive(&head) {
                Route::Archive
            } else if head.contains(&0) {
                Route::Skip("binary file of an unknown format")
            } else if ext.is_empty() {
                Route::Skip("no extension: unknown format")
            } else {
                Route::Skip("unknown format")
            }
        }
    }
}

/// Whether a file's first bytes are those of an archive `bsdtar` unpacks:
/// zip, gzip, xz, bzip2, zstd, 7z or tar
pub fn is_archive(head: &[u8]) -> bool {
    const MAGIC: [&[u8]; 6] = [
        b"PK\x03\x04",
        b"\x1f\x8b",
        b"\xfd7zXZ\x00",
        b"BZh",
        b"\x28\xb5\x2f\xfd",
        b"7z\xbc\xaf\x27\x1c",
    ];
    MAGIC.iter().any(|m| head.starts_with(m)) || head.get(257..262) == Some(b"ustar")
}

/// A file as the last import saw it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Seen {
    /// Content hash ([`hash_file`])
    hash: String,
    bytes: u64,
    /// Modification time, Unix seconds
    modified: i64,
    /// What it gave: `document`, `discord`, `table`, `no-table`, `asset`,
    /// `archive`, `copy`, `empty`
    kind: String,
    /// The document, table or asset
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
}

/// What the imports took from each file of the inbox (`inbox.json`)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// By path in the inbox
    #[serde(default)]
    files: BTreeMap<String, Seen>,
}

impl Manifest {
    fn path(root: &Path) -> PathBuf {
        root.join("inbox.json")
    }

    fn load(root: &Path) -> Self {
        let path = Self::path(root);
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            log::warn!(
                "{} does not read ({e}); every file is read again",
                path.display()
            );
            Self::default()
        })
    }

    fn save(&self, root: &Path) -> Result<()> {
        write_atomic(&Self::path(root), &serde_json::to_vec_pretty(self)?)
    }
}

/// A file taken by an import
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Taken {
    /// Path in the inbox (a table of several files: their family,
    /// `locales/*/weapons.json`)
    pub path: String,
    /// As what: `document`, `discord`, `glossary`, `asset`
    pub kind: String,
    /// What it gave, in words
    pub detail: String,
}

/// Files skipped for one reason
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub reason: String,
    pub count: usize,
    /// The first few paths
    pub examples: Vec<String>,
}

/// What an import of the inbox did
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Id and file name in `reports/`: the start time,
    /// `YYYY-MM-DD_HH-MM-SS-mmm`
    pub id: String,
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    /// Stopped before the end: tables and the catalogue were not updated
    pub cancelled: bool,
    /// Files looked at (archive contents included)
    pub files: usize,
    /// Files unchanged since the last import
    pub unchanged: usize,
    pub taken: Vec<Taken>,
    pub skipped: Vec<Skipped>,
    /// Files that could not be read, with the error
    pub failed: Vec<Taken>,
    /// Files gone from the inbox since the last import (their documents
    /// stay)
    pub gone: Vec<String>,
    /// Folders skipped, archives unpacked, repositories found
    pub notes: Vec<String>,
}

impl Report {
    fn new() -> Self {
        let started = Utc::now();
        Report {
            id: started.format("%Y-%m-%d_%H-%M-%S-%3f").to_string(),
            started,
            finished: None,
            cancelled: false,
            files: 0,
            unchanged: 0,
            taken: Vec::new(),
            skipped: Vec::new(),
            failed: Vec::new(),
            gone: Vec::new(),
            notes: Vec::new(),
        }
    }

    fn skip(&mut self, reason: &str, path: &str) {
        let group = match self.skipped.iter_mut().find(|s| s.reason == reason) {
            Some(g) => g,
            None => {
                self.skipped.push(Skipped {
                    reason: String::from(reason),
                    count: 0,
                    examples: Vec::new(),
                });
                self.skipped.last_mut().unwrap()
            }
        };
        group.count += 1;
        if group.examples.len() < EXAMPLES {
            group.examples.push(String::from(path));
        }
    }

    fn take(&mut self, path: &str, kind: &str, detail: String) {
        self.taken.push(Taken {
            path: String::from(path),
            kind: String::from(kind),
            detail,
        });
    }

    /// Files taken of one kind
    pub fn count(&self, kind: &str) -> usize {
        self.taken.iter().filter(|t| t.kind == kind).count()
    }

    /// One line: what was taken, skipped, unchanged and failed
    pub fn summary(&self) -> String {
        let skipped: usize = self.skipped.iter().map(|s| s.count).sum();
        alloc::format!(
            "{} documents, {} Discord exports, {} name tables, {} images; {} unchanged, {skipped} skipped, {} failed{}",
            self.count("document"),
            self.count("discord"),
            self.count("glossary"),
            self.count("asset"),
            self.unchanged,
            self.failed.len(),
            if self.cancelled { " (cancelled)" } else { "" }
        )
    }

    fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join("reports");
        write_atomic(
            &dir.join(alloc::format!("{}.json", self.id)),
            &serde_json::to_vec_pretty(self)?,
        )?;
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        names.sort();
        let extra = names.len().saturating_sub(REPORTS_KEPT);
        for old in &names[..extra] {
            let _ = std::fs::remove_file(old);
        }
        Ok(())
    }
}

/// The last `n` import reports, newest first; reports that do not read are
/// left out
pub fn reports(root: &Path, n: usize) -> Vec<Report> {
    let Ok(entries) = std::fs::read_dir(root.join("reports")) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "json")
                && !p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .starts_with('.')
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .rev()
        .take(n)
        .filter_map(|p| serde_json::from_slice(&std::fs::read(p).ok()?).ok())
        .collect()
}

/// One report by id
pub fn report(root: &Path, id: &str) -> Result<Report> {
    ensure!(
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-' || c == '_'),
        "not a report id: {id}"
    );
    let path = root.join("reports").join(alloc::format!("{id}.json"));
    Ok(serde_json::from_slice(
        &std::fs::read(&path).with_context(|| alloc::format!("no report {id}"))?,
    )?)
}

/// Content hash of a file: 64-bit FNV-1a, hex (as [`doc_id`])
pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = alloc::vec![0u8; 1 << 20];
    let mut h: u64 = 0xcbf29ce484222325;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    Ok(alloc::format!("{h:016x}"))
}

/// The first `n` bytes of a file (fewer if it is shorter)
pub fn head(path: &Path, n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(n).read_to_end(&mut out);
    }
    out
}

/// A file found in the inbox or an unpacked archive
#[derive(Clone, Debug)]
struct Found {
    /// Path in the inbox; inside an archive, the archive's path, `/` and
    /// the path inside it
    rel: String,
    /// Where it is on disk
    path: PathBuf,
    /// The innermost archive holding it
    archive: Option<String>,
    /// Archives around it
    depth: usize,
}

/// Collects the files of a folder, sorted, without hidden files and
/// folders or dependency and build folders (noted)
fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, notes: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        notes.push(alloc::format!("cannot read the folder {prefix}"));
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = alloc::format!("{prefix}{name}");
        let Ok(kind) = e.file_type() else { continue };
        if name == ".git" {
            let place = prefix.trim_end_matches('/');
            notes.push(alloc::format!(
                "{} is a git repository: its code is skipped, its text, string and locale files are read",
                if place.is_empty() { INBOX } else { place }
            ));
        }
        if name.starts_with('.') || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                notes.push(alloc::format!(
                    "{rel}/ skipped: dependencies or build output"
                ));
            } else {
                walk(&e.path(), &alloc::format!("{rel}/"), out, notes);
            }
        } else if kind.is_file() {
            out.push((rel, e.path()));
        }
    }
}

/// Modification time in Unix seconds
fn modified(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// A structured file of a family, and whether it changed
struct FamilyMember {
    found: Found,
    language: Option<&'static str>,
    seen: Seen,
    changed: bool,
}

/// One import of the inbox
struct Import<'a> {
    sink: &'a mut dyn Sink,
    /// The data folder
    root: PathBuf,
    cache: PathBuf,
    meta: &'a Meta,
    old: Manifest,
    new: Manifest,
    /// Content hash to path, from the last import and this one
    hashes: BTreeMap<String, String>,
    report: Report,
    families: BTreeMap<String, Vec<FamilyMember>>,
    catalogue: Catalogue,
    /// Folders archives were unpacked into
    unpacked: Vec<PathBuf>,
    queue: VecDeque<Found>,
}

impl Import<'_> {
    /// Takes one file
    fn file(&mut self, found: Found) -> Result<()> {
        let route = classify(&found.rel, || head(&found.path, 4096));
        if let Route::Skip(reason) = route {
            self.report.skip(reason, &found.rel);
            return Ok(());
        }
        let meta = std::fs::metadata(&found.path)?;
        let (bytes, modified) = (meta.len(), modified(&meta));
        let old = self.old.files.get(&found.rel).cloned();
        let quick = old
            .as_ref()
            .filter(|s| !self.meta.refresh && s.bytes == bytes && s.modified == modified);
        let (hash, unchanged) = match quick {
            Some(s) => (s.hash.clone(), true),
            None => {
                let hash = hash_file(&found.path)?;
                let same = old.as_ref().is_some_and(|s| s.hash == hash);
                (hash, same && !self.meta.refresh)
            }
        };
        let mut seen = Seen {
            hash: hash.clone(),
            bytes,
            modified,
            kind: String::new(),
            id: None,
        };
        if unchanged {
            let old = old.unwrap();
            seen.kind = old.kind;
            seen.id = old.id;
            self.report.unchanged += 1;
            if route == Route::Table {
                self.family(found, seen, false);
                return Ok(());
            }
            if route == Route::Archive {
                self.keep_archive(&found.rel);
            }
            self.new.files.insert(found.rel, seen);
            return Ok(());
        }
        if let Some(other) = self.hashes.get(&hash).filter(|o| **o != found.rel) {
            let still_there =
                self.new.files.contains_key(other) || self.root.join(INBOX).join(other).is_file();
            if still_there && !self.meta.refresh {
                let example = alloc::format!("{} (= {other})", found.rel);
                self.report.skip("same content as another file", &example);
                seen.kind = String::from("copy");
                self.new.files.insert(found.rel, seen);
                return Ok(());
            }
        }
        self.hashes.insert(hash, found.rel.clone());
        match route {
            Route::Prose => self.prose(&found, &mut seen, bytes)?,
            Route::Discord => self.discord(&found, &mut seen)?,
            Route::Image => self.image(&found, &mut seen, bytes)?,
            Route::Archive => {
                self.archive(&found, &mut seen)?;
            }
            Route::Table => {
                self.family(found, seen, true);
                return Ok(());
            }
            Route::Skip(_) => unreachable!(),
        }
        self.new.files.insert(found.rel, seen);
        Ok(())
    }

    fn prose(&mut self, found: &Found, seen: &mut Seen, bytes: u64) -> Result<()> {
        if bytes > MAX_PROSE {
            self.report.skip("too large (over 100 MB)", &found.rel);
            seen.kind = String::from("empty");
            return Ok(());
        }
        let key = alloc::format!("{INBOX}/{}", found.rel);
        let source = self.meta.source.unwrap_or(SourceKind::File);
        let mut doc = crate::file::load_keyed(&found.path, &key, source)?;
        if doc.text.trim().chars().count() < MIN_TEXT {
            self.report.skip(
                "too little text (a scanned PDF needs OCR first)",
                &found.rel,
            );
            seen.kind = String::from("empty");
            return Ok(());
        }
        doc.path = Some(key);
        self.meta.apply(&mut doc);
        let chunks = self.sink.add(&doc)?;
        let lang = doc
            .language
            .as_deref()
            .map_or_else(String::new, |l| alloc::format!(", {l}"));
        let detail = alloc::format!("\"{}\": {chunks} chunks{lang}", doc.title);
        self.sink
            .note(&alloc::format!("+ {} ({chunks} chunks)", found.rel));
        self.report.take(&found.rel, "document", detail);
        seen.kind = String::from("document");
        seen.id = Some(doc.id);
        Ok(())
    }

    fn discord(&mut self, found: &Found, seen: &mut Seen) -> Result<()> {
        let json = String::from_utf8_lossy(&std::fs::read(&found.path)?).into_owned();
        let (channel, messages) = discord::parse_export(&json)?;
        let docs = discord::to_documents(&channel, &messages, false);
        for mut doc in docs.iter().cloned() {
            self.meta.apply(&mut doc);
            self.sink.add(&doc)?;
        }
        self.sink.note(&alloc::format!(
            "+ {} ({} conversations)",
            found.rel,
            docs.len()
        ));
        let detail = alloc::format!(
            "#{}: {} messages in {} conversations",
            channel.name,
            messages.len(),
            docs.len()
        );
        self.report.take(&found.rel, "discord", detail);
        seen.kind = String::from("discord");
        Ok(())
    }

    fn image(&mut self, found: &Found, seen: &mut Seen, bytes: u64) -> Result<()> {
        if bytes > MAX_IMAGE {
            self.report.skip("too large (over 50 MB)", &found.rel);
            seen.kind = String::from("empty");
            return Ok(());
        }
        let asset = Asset::new(
            &found.rel,
            found.archive.as_deref(),
            &head(&found.path, 256 << 10),
            bytes,
            &seen.hash,
        );
        let size = match (asset.width, asset.height) {
            (Some(w), Some(h)) => alloc::format!("{w}×{h} "),
            _ => String::new(),
        };
        self.report.take(
            &found.rel,
            "asset",
            alloc::format!("{size}{}, \"{}\"", asset.format, asset.name),
        );
        seen.kind = String::from("asset");
        seen.id = Some(asset.id.clone());
        self.catalogue.upsert(asset);
        Ok(())
    }

    /// Keeps what an unchanged archive gave before, without unpacking it
    fn keep_archive(&mut self, rel: &str) {
        let prefix = alloc::format!("{rel}/");
        let inside: Vec<(String, Seen)> = self
            .old
            .files
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.report.unchanged += inside.len();
        self.new.files.extend(inside);
    }

    /// Unpacks an archive into the cache and queues its files
    fn archive(&mut self, found: &Found, seen: &mut Seen) -> Result<()> {
        seen.kind = String::from("archive");
        if found.depth >= MAX_DEPTH {
            self.report
                .skip("archive nested too deep (not unpacked)", &found.rel);
            return Ok(());
        }
        let dir = self.cache.join("unpack").join(&seen.hash);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        self.unpacked.push(dir.clone());
        let out = Command::new("bsdtar")
            .arg("-xf")
            .arg(&found.path)
            .arg("-C")
            .arg(&dir)
            .stdin(Stdio::null())
            .output()
            .context("running bsdtar (install libarchive)")?;
        ensure!(
            out.status.success(),
            "cannot unpack: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let mut files = Vec::new();
        walk(
            &dir,
            &alloc::format!("{}/", found.rel),
            &mut files,
            &mut self.report.notes,
        );
        self.report.notes.push(alloc::format!(
            "unpacked {}: {} files",
            found.rel,
            files.len()
        ));
        for (rel, path) in files {
            self.queue.push_back(Found {
                rel,
                path,
                archive: Some(found.rel.clone()),
                depth: found.depth + 1,
            });
        }
        Ok(())
    }

    /// Adds a structured file to its family (the files that differ only by
    /// language, or the file alone)
    fn family(&mut self, found: Found, seen: Seen, changed: bool) {
        let (language, family) = match tables::path_language(&found.rel) {
            Some((l, family)) => (Some(l), family),
            None => (None, found.rel.clone()),
        };
        self.families.entry(family).or_default().push(FamilyMember {
            found,
            language,
            seen,
            changed,
        });
    }

    /// Reads the families with a changed file into tables
    fn tables(&mut self) -> Result<()> {
        let families = core::mem::take(&mut self.families);
        for (family, members) in families {
            if !members.iter().any(|m| m.changed) {
                for m in members {
                    self.new.files.insert(m.found.rel, m.seen);
                }
                continue;
            }
            let mut read = Vec::new();
            let mut ok = Vec::new();
            for m in members {
                let leaves = if m.seen.bytes > tables::MAX_BYTES {
                    Err(anyhow::anyhow!("too large for a table (over 32 MB)"))
                } else {
                    tables::read(&m.found.path)
                };
                match leaves {
                    Ok((leaves, declared)) => {
                        read.push(Member {
                            file: m.found.rel.clone(),
                            language: m.language.or(declared),
                            leaves,
                        });
                        ok.push(m);
                        continue;
                    }
                    Err(e) => self.report.failed.push(Taken {
                        path: m.found.rel.clone(),
                        kind: String::from("table"),
                        detail: alloc::format!("{e:#}"),
                    }),
                }
                self.new.files.insert(
                    m.found.rel,
                    Seen {
                        kind: String::from("empty"),
                        ..m.seen
                    },
                );
            }
            let source = alloc::format!("{INBOX}/{family}");
            let table = tables::build(&source, &read);
            if table.terms.is_empty() {
                // A data table: small ones are kept as text
                tables::remove(&self.root, &table.id)?;
                for m in ok {
                    let mut seen = Seen {
                        kind: String::from("no-table"),
                        ..m.seen
                    };
                    if seen.bytes > tables::MAX_TEXT {
                        self.report.skip(
                            "data table over 1 MB without names in several languages",
                            &m.found.rel,
                        );
                    } else if let Err(e) = self.data_table(&m.found, &mut seen) {
                        self.report.failed.push(Taken {
                            path: m.found.rel.clone(),
                            kind: String::from("document"),
                            detail: alloc::format!("{e:#}"),
                        });
                        continue;
                    }
                    self.new.files.insert(m.found.rel, seen);
                }
                continue;
            }
            let kind = {
                tables::save(&self.root, &table)?;
                let detail = alloc::format!(
                    "{} terms in {} from {} ({})",
                    table.terms.len(),
                    table.languages.join(", "),
                    match table.files.len() {
                        1 => String::from("1 file"),
                        n => alloc::format!("{n} files"),
                    },
                    table.note
                );
                self.sink
                    .note(&alloc::format!("+ {family} ({} terms)", table.terms.len()));
                self.report.take(&family, "glossary", detail);
                "table"
            };
            for m in ok {
                let seen = Seen {
                    kind: String::from(kind),
                    id: Some(table.id.clone()),
                    ..m.seen
                };
                self.new.files.insert(m.found.rel, seen);
            }
        }
        Ok(())
    }

    /// A structured file without names in several languages, as a small
    /// text document of its values
    fn data_table(&mut self, found: &Found, seen: &mut Seen) -> Result<()> {
        let text = tables::as_text(&found.path)?;
        if text.trim().chars().count() < MIN_TEXT {
            self.report
                .skip("structured file with too little data", &found.rel);
            return Ok(());
        }
        let key = alloc::format!("{INBOX}/{}", found.rel);
        let name = found.rel.rsplit('/').next().unwrap_or_default();
        let source = self.meta.source.unwrap_or(SourceKind::File);
        let mut doc = Document::new(source, &key, String::from(name), text);
        doc.path = Some(key);
        self.meta.apply(&mut doc);
        let chunks = self.sink.add(&doc)?;
        let lines = doc.text.lines().count();
        self.sink
            .note(&alloc::format!("+ {} ({chunks} chunks)", found.rel));
        self.report.take(
            &found.rel,
            "document",
            alloc::format!("data table as text: {lines} lines, {chunks} chunks"),
        );
        seen.kind = String::from("document");
        seen.id = Some(doc.id);
        Ok(())
    }

    /// After a full walk: what is gone, tables without files, the
    /// catalogue linked to the new glossary
    fn finish(&mut self) -> Result<()> {
        for (rel, seen) in &self.old.files {
            if !self.new.files.contains_key(rel) && seen.kind != "copy" {
                self.report.gone.push(rel.clone());
            }
        }
        if !self.report.gone.is_empty() {
            self.report.notes.push(String::from(
                "files gone from the inbox keep their documents; delete them in the list of documents",
            ));
        }
        for table in tables::load_all(&self.root) {
            let kept = table
                .files
                .iter()
                .any(|f| self.new.files.get(f).is_some_and(|s| s.kind == "table"));
            if !kept && table.source.starts_with(INBOX) {
                tables::remove(&self.root, &table.id)?;
                self.report.notes.push(alloc::format!(
                    "table {} removed: its files are gone",
                    table.source
                ));
            }
        }
        let new = &self.new.files;
        self.catalogue
            .assets
            .retain(|a| new.get(&a.path).is_some_and(|s| s.kind == "asset"));
        let glossary = Store::load_glossary(&self.root)?;
        assets::link(&mut self.catalogue.assets, &glossary);
        Ok(())
    }
}

/// Imports the inbox of the data folder `root`: documents go to the sink,
/// name tables to `terms/`, images to the catalogue; archives are unpacked
/// in `cache`. With `meta.refresh`, every file is read again. Returns the
/// report, also written to `reports/`.
pub fn import(sink: &mut dyn Sink, root: &Path, cache: &Path, meta: &Meta) -> Result<Report> {
    let inbox = root.join(INBOX);
    std::fs::create_dir_all(&inbox)?;
    let old = Manifest::load(root);
    let hashes = old
        .files
        .iter()
        .filter(|(_, s)| s.kind != "copy")
        .map(|(k, s)| (s.hash.clone(), k.clone()))
        .collect();
    let mut import = Import {
        sink,
        root: root.to_path_buf(),
        cache: cache.to_path_buf(),
        meta,
        old,
        new: Manifest::default(),
        hashes,
        report: Report::new(),
        families: BTreeMap::new(),
        catalogue: Catalogue::load(root),
        unpacked: Vec::new(),
        queue: VecDeque::new(),
    };
    let mut files = Vec::new();
    walk(&inbox, "", &mut files, &mut import.report.notes);
    import.queue = files
        .into_iter()
        .map(|(rel, path)| Found {
            rel,
            path,
            archive: None,
            depth: 0,
        })
        .collect();
    import
        .sink
        .note(&alloc::format!("{} files in the inbox", import.queue.len()));
    let mut done = 0;
    let result = (|| -> Result<()> {
        while let Some(found) = import.queue.pop_front() {
            if import.sink.cancelled() {
                import.report.cancelled = true;
                return Ok(());
            }
            import.sink.progress(done, done + import.queue.len() + 1);
            done += 1;
            import.report.files += 1;
            let rel = found.rel.clone();
            if let Err(e) = import.file(found) {
                import.sink.note(&alloc::format!("failed {rel}: {e:#}"));
                import.report.failed.push(Taken {
                    path: rel,
                    kind: String::from("file"),
                    detail: alloc::format!("{e:#}"),
                });
            }
        }
        import.tables()?;
        import.finish()
    })();
    for dir in &import.unpacked {
        let _ = std::fs::remove_dir_all(dir);
    }
    if import.report.cancelled {
        // Keep what the last import knew of the files not reached
        for (rel, seen) in core::mem::take(&mut import.old.files) {
            import.new.files.entry(rel).or_insert(seen);
        }
    }
    import.new.save(root)?;
    import.catalogue.save(root)?;
    import.report.finished = Some(Utc::now());
    import.report.save(root)?;
    result?;
    Ok(import.report)
}

/// What waits in the inbox
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Pending {
    /// Files in the inbox (archives counted once)
    pub files: usize,
    /// Their total size
    pub bytes: u64,
    /// Files new or changed since the last import, by size and time
    pub new: usize,
    /// The inbox folder
    pub folder: PathBuf,
}

/// Looks at the inbox without reading files: how many there are and how
/// many are new or changed since the last import
pub fn pending(root: &Path) -> Pending {
    let folder = root.join(INBOX);
    let mut files = Vec::new();
    walk(&folder, "", &mut files, &mut Vec::new());
    let manifest = Manifest::load(root);
    let mut out = Pending {
        folder,
        ..Pending::default()
    };
    for (rel, path) in files {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        out.files += 1;
        out.bytes += meta.len();
        let known = manifest
            .files
            .get(&rel)
            .is_some_and(|s| s.bytes == meta.len() && s.modified == modified(&meta));
        let skipped = matches!(classify(&rel, Vec::new), Route::Skip(_));
        if !known && !skipped {
            out.new += 1;
        }
    }
    out
}

/// Where an upload of `rel` (a relative path, `/`-separated) goes in the
/// inbox; refused if it could leave the inbox or be hidden
pub fn upload_path(root: &Path, rel: &str) -> Result<PathBuf> {
    let parts: Vec<&str> = rel.split('/').collect();
    ensure!(
        !rel.is_empty()
            && rel.len() <= 1024
            && parts.iter().all(|p| {
                !p.is_empty()
                    && !p.starts_with('.')
                    && !p.contains(['\\', '\0', ':'])
                    && p.len() <= 255
            }),
        "not a file name for the inbox: {rel}"
    );
    Ok(parts.iter().fold(root.join(INBOX), |dir, p| dir.join(p)))
}

/// Puts a folder or an archive on this machine into the inbox of the data
/// folder `root`, as `<its name>/...` or `<its name>`, for [`import`] to
/// take: a folder's files as [`walk`] finds them (hidden, dependency and
/// build folders left out), an archive as it is. Returns its path in the
/// inbox and the files copied; nothing is copied when it is in the inbox
/// already.
pub fn take_in(root: &Path, path: &Path) -> Result<(String, usize)> {
    let inbox = root.join(INBOX);
    std::fs::create_dir_all(&inbox)?;
    let full = std::fs::canonicalize(path)
        .with_context(|| alloc::format!("reading {}", path.display()))?;
    if let Ok(rel) = full.strip_prefix(std::fs::canonicalize(&inbox)?) {
        return Ok((rel.to_string_lossy().replace('\\', "/"), 0));
    }
    let name = full
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .into_owned();
    let mut files = Vec::new();
    if full.is_dir() {
        let mut notes = Vec::new();
        walk(&full, &alloc::format!("{name}/"), &mut files, &mut notes);
        for note in notes {
            log::info!("{note}");
        }
    } else {
        files.push((name.clone(), full));
    }
    for (rel, from) in &files {
        let to = upload_path(root, rel)?;
        let dir = to.parent().context("no folder")?;
        std::fs::create_dir_all(dir)?;
        // Hidden while copied, so an import meanwhile skips it
        let part = dir.join(alloc::format!(
            ".{}.part",
            to.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::copy(from, &part)
            .with_context(|| alloc::format!("copying {} into the inbox", from.display()))?;
        std::fs::rename(&part, &to)?;
    }
    Ok((name, files.len()))
}

/// Document id of an inbox file's document
pub fn document_id(rel: &str) -> String {
    doc_id(&alloc::format!("{INBOX}/{rel}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Document;

    /// Keeps documents in memory
    #[derive(Default)]
    struct Memory {
        docs: Vec<Document>,
        root: PathBuf,
    }

    impl Sink for Memory {
        fn has(&self, key: &str) -> bool {
            self.docs.iter().any(|d| d.id == doc_id(key))
        }
        fn add(&mut self, doc: &Document) -> Result<usize> {
            self.docs.retain(|d| d.id != doc.id);
            self.docs.push(doc.clone());
            Ok(1)
        }
        fn raw_dir(&self, kind: &str) -> PathBuf {
            self.root.join(kind)
        }
        fn note(&mut self, _line: &str) {}
    }

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let path = root.join(INBOX).join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn classifies_by_name_and_content() {
        let none = Vec::new;
        assert_eq!(classify("guides/eggs.MD", none), Route::Prose);
        assert_eq!(classify("a/b/Fundamentals.pdf", none), Route::Prose);
        assert_eq!(
            classify("repo/src/main.rs", none),
            Route::Skip(
                "source code: not embedded (text, string and locale files beside it are read)"
            )
        );
        assert_eq!(
            classify("repo/locales/ja.json", || b"{\"a\": 1}".to_vec()),
            Route::Table
        );
        assert_eq!(
            classify("vod.json", || br#"{"guild": {"id": "1"}, "channel": {}}"#
                .to_vec()),
            Route::Discord
        );
        assert_eq!(classify("names.csv", none), Route::Table);
        assert_eq!(classify("icons/steelhead.SVG", none), Route::Image);
        assert_eq!(classify("pack.tar.gz", none), Route::Archive);
        assert_eq!(
            classify("repo/package-lock.json", none),
            Route::Skip("lock file")
        );
        assert!(matches!(classify("repo/LICENSE", none), Route::Skip(_)));
        assert!(matches!(classify("clip.mp4", none), Route::Skip(_)));
        assert_eq!(
            classify("blob.dat", || b"ab\0cd".to_vec()),
            Route::Skip("binary file of an unknown format")
        );
        assert_eq!(
            classify("notes.xyz", || b"text".to_vec()),
            Route::Skip("unknown format")
        );
    }

    #[test]
    fn upload_paths_stay_in_the_inbox() {
        let root = Path::new("/k");
        assert_eq!(
            upload_path(root, "guides/eggs.pdf").unwrap(),
            Path::new("/k/inbox/guides/eggs.pdf")
        );
        for bad in [
            "",
            "../x",
            "a/../../x",
            "/etc/passwd",
            "a//b",
            ".hidden",
            "a\\b",
        ] {
            assert!(upload_path(root, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn walks_imports_and_dedups() {
        let root =
            std::env::temp_dir().join(alloc::format!("cuttlefish-inbox-{}", std::process::id()));
        let cache = root.join("cache");
        let _ = std::fs::remove_dir_all(&root);
        let guide = b"# Egg flow\n\nKeep the golden eggs moving toward the basket at every tide.";
        write(&root, "guides/eggs.md", guide);
        write(&root, "guides/z-copy.md", guide);
        write(&root, "guides/empty.txt", b"hi");
        write(&root, "repo/.git/HEAD", b"ref: refs/heads/main");
        write(&root, "repo/src/app.js", b"console.log('x')");
        write(
            &root,
            "repo/node_modules/x/readme.md",
            b"# not read at all, it is a dependency",
        );
        write(
            &root,
            "repo/locales/en/weapons.json",
            br#"{"splattershot": "Splattershot"}"#,
        );
        write(
            &root,
            "repo/locales/ja/weapons.json",
            "{\"splattershot\": \"スプラシューター\"}".as_bytes(),
        );
        write(&root, "repo/package.json", br#"{"name": "tool"}"#);
        write(
            &root,
            "data/weapons.json",
            br#"[{"__RowId": "Shooter_Normal_00", "Range": 1.0, "Damage": 36, "Special": "Trizooka"}]"#,
        );
        write(
            &root,
            "icons/Wst_splattershot.svg",
            br#"<svg viewBox="0 0 64 64"></svg>"#,
        );
        write(&root, "junk.exe", b"MZ");
        let mut sink = Memory {
            root: root.clone(),
            ..Default::default()
        };
        let meta = Meta::default();
        assert_eq!(pending(&root).new, 7);
        let report = import(&mut sink, &root, &cache, &meta).unwrap();
        assert_eq!(report.count("document"), 2, "{report:#?}");
        assert_eq!(report.count("glossary"), 1);
        assert_eq!(report.count("asset"), 1);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        let reasons: Vec<&str> = report.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert!(
            reasons.contains(&"same content as another file"),
            "{reasons:?}"
        );
        assert!(reasons.iter().any(|r| r.starts_with("too little text")));
        assert!(reasons.iter().any(|r| r.starts_with("source code")));
        assert!(reasons.contains(&"project configuration"), "{reasons:?}");
        assert!(report.notes.iter().any(|n| n.contains("node_modules")));
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.starts_with("repo is a git repository"))
        );
        assert_eq!(sink.docs.len(), 2);
        assert_eq!(sink.docs[0].path.as_deref(), Some("inbox/guides/eggs.md"));
        assert_eq!(sink.docs[0].id, document_id("guides/eggs.md"));
        // A data table without languages is a small text document
        assert_eq!(sink.docs[1].title, "weapons.json");
        assert!(sink.docs[1].text.contains("Shooter_Normal_00 / Damage: 36"));
        // Names reached the glossary, and the icon is linked to its term
        let glossary = Store::load_glossary(&root).unwrap();
        let term = glossary.lookup("スプラシューター").unwrap();
        assert_eq!(
            term.from,
            ["inbox/repo/locales/*/weapons.json#splattershot"]
        );
        let catalogue = Catalogue::load(&root);
        assert_eq!(catalogue.assets[0].term.as_deref(), Some("splattershot"));
        assert_eq!(catalogue.assets[0].width, Some(64));
        assert_eq!(pending(&root).new, 0);

        // Again: nothing to do
        let again = import(&mut sink, &root, &cache, &meta).unwrap();
        assert!(again.taken.is_empty(), "{:?}", again.taken);
        assert_eq!(again.unchanged, 7);

        // A changed file replaces its document; a gone file is reported
        write(
            &root,
            "guides/eggs.md",
            b"# Egg flow\n\nBank eggs early, before the wave's last twenty seconds.",
        );
        std::fs::remove_file(root.join(INBOX).join("icons/Wst_splattershot.svg")).unwrap();
        let changed = import(&mut sink, &root, &cache, &meta).unwrap();
        assert_eq!(changed.count("document"), 1);
        assert_eq!(changed.gone, ["icons/Wst_splattershot.svg"]);
        assert_eq!(sink.docs.len(), 2);
        let eggs = sink
            .docs
            .iter()
            .find(|d| d.id == document_id("guides/eggs.md"));
        assert!(eggs.unwrap().text.contains("twenty seconds"));
        assert!(Catalogue::load(&root).assets.is_empty());
        assert_eq!(reports(&root, 10).len(), 3);
        let first = &reports(&root, 10)[2];
        assert_eq!(super::report(&root, &first.id).unwrap().taken.len(), 4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn unpacks_archives() {
        let root =
            std::env::temp_dir().join(alloc::format!("cuttlefish-zip-{}", std::process::id()));
        let cache = root.join("cache");
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(src.join("pack/names")).unwrap();
        std::fs::write(
            src.join("pack/names/bosses.csv"),
            "id,en,ja\nsteelhead,Steelhead,バクダン\n",
        )
        .unwrap();
        std::fs::write(
            src.join("pack/guide.md"),
            "# Stingers\n\nKill the Stinger from below, where its pot is exposed.",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(INBOX)).unwrap();
        let status = Command::new("bsdtar")
            .arg("-a")
            .arg("-cf")
            .arg(root.join(INBOX).join("pack.zip"))
            .arg("-C")
            .arg(&src)
            .arg("pack")
            .status()
            .unwrap();
        assert!(status.success());
        let mut sink = Memory {
            root: root.clone(),
            ..Default::default()
        };
        let report = import(&mut sink, &root, &cache, &Meta::default()).unwrap();
        assert_eq!(report.count("document"), 1, "{report:#?}");
        assert_eq!(report.count("glossary"), 1);
        assert_eq!(
            sink.docs[0].path.as_deref(),
            Some("inbox/pack.zip/pack/guide.md")
        );
        assert!(
            Store::load_glossary(&root)
                .unwrap()
                .lookup("バクダン")
                .unwrap()
                .from[0]
                .starts_with("inbox/pack.zip/pack/names/bosses.csv#")
        );
        // Unpacked files are cleaned up; the unchanged archive is not
        // unpacked again
        assert!(cache.join("unpack").read_dir().unwrap().next().is_none());
        let again = import(&mut sink, &root, &cache, &Meta::default()).unwrap();
        assert_eq!(again.unchanged, 3);
        assert!(again.taken.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
