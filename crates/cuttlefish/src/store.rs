//! The knowledge store: a data folder outside the repository, which may
//! be a synced folder (Dropbox, rclone).
//!
//! ```text
//! <data>/
//!   inbox/             anything dropped here, read by [`crate::inbox`]
//!   inbox.json         what the inbox import took from each file (hashes)
//!   glossary.toml      your glossary (the crate's seed until you add one)
//!   glossary-user.toml slang you taught or approved ([`crate::slang`])
//!   terms/<id>.json    name tables imported from the inbox ([`crate::tables`])
//!   assets.json        images and icons from the inbox ([`crate::assets`])
//!   reports/<t>.json   what each inbox import did
//!   digest.md          curated fundamentals, sent with every review
//!   raw/<kind>/...     fetched pages, subtitles and exports as received
//!   docs/<id>.json     processed documents
//!   index/             chunk vectors (see [`crate::index`])
//! ```
//!
//! The studio passes its `[cuttlefish] knowledge` (by default `Knowledge`
//! next to the Inkspector's root); the CLI finds the same folder through the
//! studio's config, else `--data` or `$CUTTLEFISH_DATA`. What needs no
//! syncing stays on this machine, in [`cache_dir`]: the embedding model,
//! thumbnails, unpacked archives. [`migrate`] copies our entries of the
//! folder of before (`~/.local/share/cuttlefish`, shared with another
//! program) over and moves them into a `procon-migrated-<date>.safe-to-delete`
//! folder there, leaving the rest alone.
//!
//! Safe on a synced folder: every file is written whole through a
//! temporary file and a rename ([`write_atomic`]), and files that arrive
//! half-synced or as conflict copies are skipped with a warning. Writers
//! (imports, deletes, reindexing) take the folder's write lock
//! ([`crate::lock`], `.lock`) so two never write at once; readers need no
//! lock. Documents are the truth and the index follows them: chunks of
//! documents that are gone are dropped on opening, an index that does not
//! read is rebuilt, and [`Store::catch_up`] embeds documents synced in from
//! elsewhere.

use crate::chunk::{Chunk, ChunkConfig, chunk_text};
use crate::doc::{Document, SourceKind};
use crate::embed::{Embedder, Role};
use crate::file::is_binary_text;
use crate::glossary::Glossary;
use crate::index::{Entry, FlatIndex, VectorIndex};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail, ensure};
use core::sync::atomic::{AtomicUsize, Ordering};
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A retrieved chunk
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    /// Score: cosine similarity plus the source weight's bonus
    /// ([`crate::index::score`])
    pub score: f32,
    /// The chunk and its document's metadata
    #[serde(flatten)]
    pub entry: Entry,
}

/// Chunks embedded between two looks at whether to stop
/// ([`Store::catch_up`])
pub const EMBED_BATCH: usize = 32;

/// What [`Store::catch_up`] did
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaughtUp {
    /// Documents embedded
    pub documents: usize,
    /// Whether it was told to stop before the end
    pub stopped: bool,
}

/// Temporary files written by this process so far, to name the next
static TEMPORARY: AtomicUsize = AtomicUsize::new(0);

/// Writes a file whole: into a hidden temporary file beside it, then renamed
/// over it, so readers and sync clients never see half a file
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("no folder")?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let n = TEMPORARY.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(alloc::format!(".{name}.{}-{n}.tmp", std::process::id()));
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| alloc::format!("writing {}", path.display()))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Where the data folder was before: `$XDG_DATA_HOME/cuttlefish`, else
/// `~/.local/share/cuttlefish`. Another program uses that folder too, so
/// [`migrate`] takes only our entries out of it and [`Store::open`] refuses
/// it.
pub fn legacy_root() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home().join(".local/share"), PathBuf::from)
        .join("cuttlefish")
}

/// Folder on this machine for what is not synced (the embedding model,
/// thumbnails, unpacked archives): `$CUTTLEFISH_CACHE`, else
/// `$XDG_CACHE_HOME/procon-cuttlefish`, else `~/.cache/procon-cuttlefish`
/// (`~/.cache/cuttlefish` belongs to another program)
pub fn cache_dir() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(dir) = var("CUTTLEFISH_CACHE") {
        return PathBuf::from(dir);
    }
    var("XDG_CACHE_HOME")
        .map_or_else(|| home().join(".cache"), PathBuf::from)
        .join("procon-cuttlefish")
}

/// Folder for embedding model files: `models` in [`cache_dir`]
pub fn models_dir() -> PathBuf {
    cache_dir().join("models")
}

/// True for a document id ([`crate::doc::doc_id`]: 16 lowercase hex
/// digits); other files in `docs/` (conflict copies, temporary files) are
/// not documents
pub fn is_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Ids of the documents in a data folder, from their file names
fn doc_ids(root: &Path) -> Result<BTreeSet<String>> {
    let mut ids = BTreeSet::new();
    let dir = root.join("docs");
    if !dir.is_dir() {
        return Ok(ids);
    }
    for e in std::fs::read_dir(dir)? {
        let path = e?.path();
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        if path.extension().is_some_and(|x| x == "json") && is_id(&id) {
            ids.insert(String::from(id));
        }
    }
    Ok(ids)
}

/// Every document of a data folder, sorted by id. A file that does not read
/// (half-synced, say) is skipped with a warning.
pub fn read_documents(root: &Path) -> Result<Vec<Document>> {
    let mut docs = Vec::new();
    for id in doc_ids(root)? {
        let path = root.join("docs").join(alloc::format!("{id}.json"));
        let read = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Document>(&b)?));
        match read {
            Ok(doc) => docs.push(doc),
            Err(e) => log::warn!("skipping {}: {e:#}", path.display()),
        }
    }
    Ok(docs)
}

/// Documents, their index and the glossary in one data folder
pub struct Store {
    root: PathBuf,
    index: FlatIndex,
    glossary: Glossary,
    chunking: ChunkConfig,
}

impl Store {
    /// Opens (or creates) a data folder for vectors of `embedder`. The
    /// folder's parent must exist, so a synced folder that is missing is not
    /// silently replaced by a new one. Chunks of documents that are gone
    /// are dropped; an index that does not read starts empty, for
    /// [`Store::catch_up`] to rebuild. The folder of before
    /// ([`legacy_root`]) is refused: another program owns it.
    pub fn open(root: &Path, embedder: &dyn Embedder) -> Result<Self> {
        let legacy = legacy_root();
        ensure!(
            root != legacy
                && (root.canonicalize().ok()).is_none_or(|r| Some(r) != legacy.canonicalize().ok()),
            "{} belongs to another program; give the knowledge folder with --config, --data or $CUTTLEFISH_DATA",
            root.display()
        );
        let parent = root.parent().filter(|p| !p.as_os_str().is_empty());
        ensure!(
            parent.is_none_or(Path::is_dir),
            "{} does not exist (is the synced folder there?)",
            parent.unwrap_or(root).display()
        );
        std::fs::create_dir_all(root.join("docs"))
            .with_context(|| alloc::format!("creating {}", root.display()))?;
        let mut index = match FlatIndex::load(&root.join("index")) {
            Ok(Some(i)) if i.embedder() != embedder.name() => bail!(
                "the index in {} was built with {}, not {}; remove the index folder and re-embed with `cuttlefish reindex`",
                root.display(),
                i.embedder(),
                embedder.name()
            ),
            Ok(Some(i)) => i,
            Ok(None) => FlatIndex::new(embedder.name(), embedder.dim()),
            Err(e) => {
                log::warn!(
                    "the index in {} does not read ({e:#}); rebuilding it from the documents",
                    root.display()
                );
                FlatIndex::new(embedder.name(), embedder.dim())
            }
        };
        let ids = doc_ids(root)?;
        let gone: BTreeSet<String> = index
            .entries()
            .iter()
            .filter(|e| !ids.contains(&e.doc_id))
            .map(|e| e.doc_id.clone())
            .collect();
        for id in &gone {
            index.remove_doc(id);
        }
        Ok(Store {
            root: root.to_path_buf(),
            index,
            glossary: Self::load_glossary(root)?,
            chunking: ChunkConfig::default(),
        })
    }

    /// The data folder's own `glossary.toml`, or the seed without one:
    /// the names as written, without imports
    pub fn own_glossary(root: &Path) -> Result<Glossary> {
        let path = root.join("glossary.toml");
        if path.exists() {
            Glossary::load(&path)
        } else {
            Ok(Glossary::seed())
        }
    }

    /// The data folder's `glossary.toml` (or the seed without one), with
    /// the name tables imported from the inbox merged in, newest game
    /// first, and the aliases the user approved
    /// (`glossary-user.toml`, [`crate::slang`])
    pub fn load_glossary(root: &Path) -> Result<Glossary> {
        let mut glossary = Self::own_glossary(root)?;
        for table in crate::tables::load_all(root) {
            glossary.merge(&table.terms);
        }
        crate::slang::UserGlossary::load(root)?.apply(&mut glossary);
        Ok(glossary)
    }

    /// Reads the glossary again (after an import of name tables)
    pub fn reload_glossary(&mut self) -> Result<()> {
        self.glossary = Self::load_glossary(&self.root)?;
        Ok(())
    }

    /// The data folder
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn doc_path(&self, id: &str) -> PathBuf {
        self.root.join("docs").join(alloc::format!("{id}.json"))
    }

    /// Whether the document of this url or path is stored
    pub fn has(&self, key: &str) -> bool {
        self.has_id(&crate::doc::doc_id(key))
    }

    /// Whether the document with this id is stored
    pub fn has_id(&self, id: &str) -> bool {
        is_id(id) && self.doc_path(id).exists()
    }

    /// The stored document of this url or path, when there is one that reads
    pub fn document(&self, key: &str) -> Option<Document> {
        let bytes = std::fs::read(self.doc_path(&crate::doc::doc_id(key))).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Folder for raw downloads of one kind (`web`, `youtube`, `discord`)
    pub fn raw_dir(&self, kind: &str) -> PathBuf {
        self.root.join("raw").join(kind)
    }

    /// The glossary
    pub fn glossary(&self) -> &Glossary {
        &self.glossary
    }

    /// The curated digest (`digest.md`), if written
    pub fn digest(&self) -> Option<String> {
        std::fs::read_to_string(self.root.join("digest.md"))
            .ok()
            .filter(|d| !d.trim().is_empty())
    }

    /// The index
    pub fn index(&self) -> &FlatIndex {
        &self.index
    }

    /// Stores a document (replacing an earlier one with the same id) and
    /// indexes its chunks; returns the number of chunks. Call
    /// [`Store::save`] to write the index.
    pub fn add(&mut self, doc: &Document, embedder: &dyn Embedder) -> Result<usize> {
        ensure!(
            !is_binary_text(&doc.text),
            "\"{}\" is binary data, not text: not stored",
            doc.title
        );
        write_atomic(&self.doc_path(&doc.id), &serde_json::to_vec_pretty(doc)?)?;
        self.index_doc(doc, embedder)
    }

    /// Removes a document and its chunks; false if there was none. Call
    /// [`Store::save`] to write the index.
    pub fn delete(&mut self, id: &str) -> Result<bool> {
        ensure!(is_id(id), "not a document id: {id}");
        let path = self.doc_path(id);
        let existed = path.exists();
        if existed {
            std::fs::remove_file(&path)
                .with_context(|| alloc::format!("removing {}", path.display()))?;
        }
        let chunks = self.index.len();
        self.index.remove_doc(id);
        Ok(existed || self.index.len() != chunks)
    }

    /// A document's chunks: its text's ([`chunk_text`]), or one per expert
    /// comment headed by its label ([`crate::expert`])
    fn doc_chunks(&self, doc: &Document) -> Vec<Chunk> {
        if doc.expert_comments.is_empty() {
            return chunk_text(&doc.text, &self.chunking);
        }
        doc.expert_comments
            .iter()
            .enumerate()
            .map(|(i, c)| Chunk {
                ordinal: i as u32,
                heading: c.expert.label(),
                text: c.text.clone(),
            })
            .collect()
    }

    fn index_doc(&mut self, doc: &Document, embedder: &dyn Embedder) -> Result<usize> {
        let chunks = self.doc_chunks(doc);
        self.index_chunks(doc, &chunks, embedder, &mut |_| true)?;
        Ok(chunks.len())
    }

    /// Indexes a document's chunks, [`EMBED_BATCH`] at a time; after each
    /// batch `batch` hears how many were embedded and says whether to go
    /// on. False when it said stop: the document's chunks are removed
    /// again, so it is embedded whole next time.
    fn index_chunks(
        &mut self,
        doc: &Document,
        chunks: &[Chunk],
        embedder: &dyn Embedder,
        batch: &mut dyn FnMut(usize) -> bool,
    ) -> Result<bool> {
        self.index.remove_doc(&doc.id);
        for group in chunks.chunks(EMBED_BATCH) {
            let inputs: Vec<String> = group
                .iter()
                .map(|c| passage_text(&doc.title, &c.heading, &c.text))
                .collect();
            let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
            let vectors = embedder.embed(&refs, Role::Passage)?;
            for (c, v) in group.iter().zip(vectors) {
                let comment = doc.expert_comments.get(c.ordinal as usize);
                let entry = Entry {
                    doc_id: doc.id.clone(),
                    ordinal: c.ordinal,
                    title: doc.title.clone(),
                    heading: c.heading.clone(),
                    url: comment.map(|c| c.url.clone()).or_else(|| doc.url.clone()),
                    source: doc.source,
                    license: doc.license.clone(),
                    language: doc.language.clone(),
                    weight: doc.weight,
                    game: doc.era(),
                    video: comment
                        .map(|c| c.expert.video.clone())
                        .or_else(|| doc.video().map(String::from)),
                    expert: comment.map(|c| c.expert.clone()),
                    text: c.text.clone(),
                };
                self.index.add(entry, v)?;
            }
            if !batch(group.len()) {
                self.index.remove_doc(&doc.id);
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Every stored document
    pub fn documents(&self) -> Result<Vec<Document>> {
        read_documents(&self.root)
    }

    /// Embeds the documents the index lacks (synced in from another
    /// machine, or all of them after the index was rebuilt). `step` hears
    /// progress lines (`embedding 64 of 950 chunks (2 of 7 documents)`,
    /// after each batch) and says whether to go on; stopped, what was
    /// embedded stays and the rest waits for the next time. Documents whose
    /// text is binary data are never embedded (a warning each). Call
    /// [`Store::save`] afterwards when something was embedded.
    pub fn catch_up(
        &mut self,
        embedder: &dyn Embedder,
        step: &mut dyn FnMut(&str) -> bool,
    ) -> Result<CaughtUp> {
        let indexed: BTreeSet<String> = self
            .index
            .entries()
            .iter()
            .map(|e| e.doc_id.clone())
            .collect();
        let mut todo = Vec::new();
        for doc in self.documents()? {
            if indexed.contains(&doc.id) || doc.text.trim().is_empty() {
                continue;
            }
            if is_binary_text(&doc.text) {
                let line = alloc::format!(
                    "not embedding \"{}\" ({}): its text is binary data; delete it",
                    doc.title,
                    doc.id
                );
                log::warn!("{line}");
                step(&line);
                continue;
            }
            let chunks = self.doc_chunks(&doc);
            todo.push((doc, chunks));
        }
        let total: usize = todo.iter().map(|(_, c)| c.len()).sum();
        let mut out = CaughtUp::default();
        if todo.is_empty() {
            return Ok(out);
        }
        let docs = todo.len();
        let mut done = 0;
        if !step(&alloc::format!(
            "embedding {total} chunks of {docs} documents the index lacks"
        )) {
            out.stopped = true;
            return Ok(out);
        }
        for (i, (doc, chunks)) in todo.iter().enumerate() {
            let whole = self.index_chunks(doc, chunks, embedder, &mut |n| {
                done += n;
                step(&alloc::format!(
                    "embedding {done} of {total} chunks ({} of {docs} documents)",
                    i + 1
                ))
            })?;
            if !whole {
                out.stopped = true;
                break;
            }
            out.documents += 1;
        }
        Ok(out)
    }

    /// Re-chunks and re-embeds every stored document into a fresh index
    /// (after changing the embedder or the chunk sizes)
    pub fn reindex(&mut self, embedder: &dyn Embedder) -> Result<usize> {
        self.index = FlatIndex::new(embedder.name(), embedder.dim());
        let mut n = 0;
        for doc in self.documents()? {
            if is_binary_text(&doc.text) {
                log::warn!(
                    "not embedding \"{}\" ({}): its text is binary data; delete it",
                    doc.title,
                    doc.id
                );
                continue;
            }
            n += self.index_doc(&doc, embedder)?;
        }
        Ok(n)
    }

    /// Writes the index
    pub fn save(&self) -> Result<()> {
        self.index.save(&self.root.join("index"))
    }

    /// The `k` best chunks for a query; the query is expanded with the
    /// other-language names of glossary terms it mentions
    pub fn search(&self, query: &str, k: usize, embedder: &dyn Embedder) -> Result<Vec<Hit>> {
        self.search_where(query, k, embedder, &|_| true)
    }

    /// The `k` best chunks for a query among those `keep` accepts (see
    /// [`Store::search`])
    pub fn search_where(
        &self,
        query: &str,
        k: usize,
        embedder: &dyn Embedder,
        keep: &dyn Fn(&Entry) -> bool,
    ) -> Result<Vec<Hit>> {
        if self.index.is_empty() {
            return Ok(Vec::new());
        }
        let q = self.glossary.expand(query);
        let v = embedder.embed(&[&q], Role::Query)?.remove(0);
        Ok(self
            .index
            .search_where(&v, k, keep)
            .into_iter()
            .map(|(e, score)| Hit {
                score,
                entry: e.clone(),
            })
            .collect())
    }

    /// Number of documents per source kind, and chunks in the index
    pub fn stats(&self) -> Result<(Vec<(SourceKind, usize)>, usize)> {
        let mut counts: Vec<(SourceKind, usize)> = Vec::new();
        for d in self.documents()? {
            match counts.iter_mut().find(|(k, _)| *k == d.source) {
                Some((_, n)) => *n += 1,
                None => counts.push((d.source, 1)),
            }
        }
        Ok((counts, self.index.len()))
    }
}

/// What is embedded for a chunk: its place in the document, then its text
fn passage_text(title: &str, heading: &str, text: &str) -> String {
    if heading.is_empty() {
        alloc::format!("{title}\n{text}")
    } else {
        alloc::format!("{title} > {heading}\n{text}")
    }
}

/// Copies a folder into another, file by file through [`write_atomic`],
/// leaving files that are there already; returns the files copied
fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    let mut n = 0;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let kind = e.file_type()?;
        let target = to.join(e.file_name());
        if kind.is_dir() {
            n += copy_tree(&e.path(), &target)?;
        } else if kind.is_file() && !target.exists() {
            write_atomic(&target, &std::fs::read(e.path())?)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Folders of a data folder that are ours and hold data
const DATA_DIRS: [&str; 6] = ["docs", "index", "raw", "terms", "reports", "inbox"];

/// Files of a data folder that are ours
const DATA_FILES: [&str; 5] = [
    "glossary.toml",
    "glossary-user.toml",
    "digest.md",
    "assets.json",
    "inbox.json",
];

/// Our entries in a data folder of the older layout: its data, and the
/// embedding model it once kept (not copied: it is downloaded again)
fn our_entries() -> impl Iterator<Item = &'static str> {
    DATA_DIRS.into_iter().chain(DATA_FILES).chain(["models"])
}

/// Sizes of the files in a folder and below, by path inside it
fn sizes(dir: &Path) -> BTreeMap<PathBuf, u64> {
    let mut out = BTreeMap::new();
    let mut stack = alloc::vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                stack.push(e.path());
            } else if kind.is_file() {
                let rel = e
                    .path()
                    .strip_prefix(dir)
                    .unwrap_or(&e.path())
                    .to_path_buf();
                out.insert(rel, e.metadata().map_or(0, |m| m.len()));
            }
        }
    }
    out
}

/// Whether a data folder holds any data of ours: a file in one of its data
/// folders, or one of its data files
fn holds_data(root: &Path) -> bool {
    DATA_DIRS.iter().any(|d| !sizes(&root.join(d)).is_empty())
        || DATA_FILES.iter().any(|f| root.join(f).is_file())
}

/// Whether `root` holds a copy of the data of `old`: every file of its data
/// folders with the same size, and the same data files
fn holds_copy_of(old: &Path, root: &Path) -> bool {
    let dirs = DATA_DIRS.iter().all(|d| {
        let copied = sizes(&root.join(d));
        sizes(&old.join(d))
            .iter()
            .all(|(path, size)| copied.get(path) == Some(size))
    });
    let files = DATA_FILES.iter().all(|f| {
        let a = std::fs::read(old.join(f)).ok();
        a.is_none() || a == std::fs::read(root.join(f)).ok()
    });
    dirs && files
}

/// Name of the folder [`migrate`] moves our entries into
const ASIDE_PREFIX: &str = "procon-migrated-";

/// Suffix of that folder's name
const ASIDE_SUFFIX: &str = ".safe-to-delete";

/// A new folder in `old` to move our entries into:
/// `procon-migrated-<YYYY-MM-DD>.safe-to-delete`
fn aside_name(old: &Path) -> Option<PathBuf> {
    let date = chrono::Local::now().format("%Y-%m-%d");
    (1..100)
        .map(|n| match n {
            1 => alloc::format!("{ASIDE_PREFIX}{date}{ASIDE_SUFFIX}"),
            n => alloc::format!("{ASIDE_PREFIX}{date}-{n}{ASIDE_SUFFIX}"),
        })
        .map(|name| old.join(name))
        .find(|aside| !aside.exists())
}

/// Folders [`migrate`] moved our entries of `old` into that are still there
pub fn moved_aside(old: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(old) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(ASIDE_PREFIX) && n.ends_with(ASIDE_SUFFIX)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// What [`migrate`] did
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Migration {
    /// What was copied, in words
    pub copied: Vec<String>,
    /// The folder in `old` our entries were moved into
    pub moved_aside: Option<PathBuf>,
}

/// Brings our entries of a data folder of the older layout (`old`, usually
/// [`legacy_root`], a folder another program uses too) over. Only our
/// entries are touched (see [`DATA_DIRS`], [`DATA_FILES`] and `models`);
/// anything else in `old` stays as it is.
///
/// Our data is copied into `root` when `root` holds none, and checked (every
/// file with its size, the same small files). Then our entries, the model
/// included (it is downloaded again into [`models_dir`]), are moved into
/// `old/procon-migrated-<date>.safe-to-delete/`, never deleted. Without data
/// (empty folders, the model), they are moved there all the same. Nothing
/// is moved when `old` is `root`, when `root`'s parent is missing (an
/// unmounted synced folder), when `root` holds data of its own already, or
/// when the copy differs.
pub fn migrate(old: &Path, root: &Path) -> Result<Migration> {
    let mut done = Migration::default();
    let ours: Vec<&str> = our_entries().filter(|n| old.join(n).exists()).collect();
    if !old.is_dir() || ours.is_empty() {
        return Ok(done);
    }
    let same = match (old.canonicalize(), root.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => old == root,
    };
    // A synced folder that is not there is not replaced by a new one
    let parent_missing = root.parent().is_some_and(|p| !p.is_dir());
    if same || parent_missing {
        return Ok(done);
    }
    if holds_data(old) {
        if holds_data(root) {
            log::warn!(
                "{} and {} both hold knowledge; nothing is copied or moved",
                old.display(),
                root.display()
            );
            return Ok(done);
        }
        for name in DATA_DIRS {
            if old.join(name).is_dir() {
                let n = copy_tree(&old.join(name), &root.join(name))?;
                done.copied.push(alloc::format!("{n} files of {name}/"));
            }
        }
        for name in DATA_FILES {
            if old.join(name).is_file() {
                write_atomic(&root.join(name), &std::fs::read(old.join(name))?)?;
                done.copied.push(String::from(name));
            }
        }
        if !holds_copy_of(old, root) {
            log::warn!(
                "{} does not hold all of {} after copying; nothing is moved",
                root.display(),
                old.display()
            );
            return Ok(done);
        }
        log::info!(
            "Copied from {} to {}: {}",
            old.display(),
            root.display(),
            done.copied.join(", ")
        );
    } else {
        log::info!(
            "Nothing needed migrating from {}: our folders there are empty",
            old.display()
        );
    }
    let aside = aside_name(old).context("no name to move our old entries aside to")?;
    std::fs::create_dir(&aside).with_context(|| alloc::format!("creating {}", aside.display()))?;
    for name in &ours {
        std::fs::rename(old.join(name), aside.join(name))
            .with_context(|| alloc::format!("moving {name} into {}", aside.display()))?;
    }
    log::info!(
        "Moved our old entries ({}) into {}; the knowledge is in {} now and that folder is safe to delete. Other files in {} are left alone.",
        ours.join(", "),
        aside.display(),
        root.display(),
        old.display()
    );
    done.moved_aside = Some(aside);
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn doc(key: &str, title: &str, text: &str) -> Document {
        Document::new(
            SourceKind::Guide,
            key,
            String::from(title),
            String::from(text),
        )
    }

    #[test]
    fn adds_replaces_and_searches() {
        let root = temp("store");
        let e = HashEmbedder { dim: 256 };
        let mut store = Store::open(&root, &e).unwrap();
        let mut a = doc(
            "https://wiki/steelhead",
            "Steelhead",
            "# Strategy\n\nShoot the bomb on its head.",
        );
        a.source = SourceKind::Wiki;
        let b = doc("guide.md", "Tides", "At low tide the basket moves down.");
        store.add(&a, &e).unwrap();
        store.add(&b, &e).unwrap();
        store.add(&a, &e).unwrap();
        assert_eq!(store.index().len(), 2);
        // The Japanese name expands to "Steelhead" through the glossary
        let hits = store.search("バクダン", 1, &e).unwrap();
        assert_eq!(hits[0].entry.title, "Steelhead");
        assert_eq!(hits[0].entry.heading, "Strategy");
        store.save().unwrap();
        let mut reopened = Store::open(&root, &e).unwrap();
        assert_eq!(reopened.index().len(), 2);
        assert_eq!(reopened.reindex(&e).unwrap(), 2);
        let (counts, chunks) = reopened.stats().unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(chunks, 2);
    }

    #[test]
    fn deletes_documents() {
        let root = temp("delete");
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&root, &e).unwrap();
        let a = doc("a", "Eggs", "Bank the golden eggs.");
        let b = doc("b", "Tides", "Low tide moves the basket.");
        store.add(&a, &e).unwrap();
        store.add(&b, &e).unwrap();
        assert!(store.delete(&a.id).unwrap());
        assert!(!store.delete(&a.id).unwrap());
        assert!(store.delete("../../etc").is_err());
        assert!(!store.has("a") && store.has("b"));
        assert_eq!(store.index().len(), 1);
        assert_eq!(store.documents().unwrap().len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tolerates_what_a_sync_brings() {
        let root = temp("sync");
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&root, &e).unwrap();
        let a = doc("a", "Eggs", "Bank the golden eggs.");
        let b = doc("b", "Tides", "Low tide moves the basket.");
        store.add(&a, &e).unwrap();
        store.add(&b, &e).unwrap();
        store.save().unwrap();
        // Another machine deleted `a` and added `c`; a conflict copy and a
        // half-synced file arrived too
        let docs = root.join("docs");
        std::fs::remove_file(docs.join(alloc::format!("{}.json", a.id))).unwrap();
        let c = doc("c", "Stinger", "Kill the Stinger from below.");
        std::fs::write(
            docs.join(alloc::format!("{}.json", c.id)),
            serde_json::to_vec(&c).unwrap(),
        )
        .unwrap();
        std::fs::write(
            docs.join(alloc::format!("{} (conflicted copy).json", b.id)),
            b"{}",
        )
        .unwrap();
        std::fs::write(docs.join("00000000000000ff.json"), b"{\"id\":").unwrap();
        let mut store = Store::open(&root, &e).unwrap();
        assert_eq!(store.index().len(), 1);
        assert_eq!(store.catch_up(&e, &mut |_| true).unwrap().documents, 1);
        assert_eq!(store.catch_up(&e, &mut |_| true).unwrap().documents, 0);
        assert_eq!(store.documents().unwrap().len(), 2);
        // A half-synced index is rebuilt
        std::fs::write(root.join("index/vectors.f32"), b"xx").unwrap();
        let mut store = Store::open(&root, &e).unwrap();
        assert_eq!(store.index().len(), 0);
        assert_eq!(store.catch_up(&e, &mut |_| true).unwrap().documents, 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn catching_up_stops_when_told_and_skips_binary_documents() {
        let root = temp("catch-up");
        let e = HashEmbedder { dim: 16 };
        let long = "Bank the golden eggs before the tide changes.\n\n".repeat(2000);
        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let big = doc("big", "Eggs", &long);
        // What a zip read as text left behind
        let garbage = doc(
            "zip",
            "stat.ink.zip",
            "PK\u{3}\u{4}\u{14}\0\0\0\u{fffd}\u{fffd}",
        );
        for d in [&big, &garbage] {
            std::fs::write(
                docs.join(alloc::format!("{}.json", d.id)),
                serde_json::to_vec(d).unwrap(),
            )
            .unwrap();
        }
        let mut store = Store::open(&root, &e).unwrap();
        // Stopped after the first batch: nothing of the document stays
        let mut lines = Vec::new();
        let mut batches = 0;
        let out = store
            .catch_up(&e, &mut |line| {
                lines.push(String::from(line));
                if line.contains(" chunks (") {
                    batches += 1;
                }
                batches < 1
            })
            .unwrap();
        assert_eq!(
            out,
            CaughtUp {
                documents: 0,
                stopped: true
            }
        );
        assert_eq!(store.index().len(), 0);
        assert!(lines[0].starts_with("not embedding \"stat.ink.zip\""));
        assert!(lines.iter().any(|l| l.starts_with("embedding 32 of ")));
        // Going on embeds it whole, never the binary one
        let out = store.catch_up(&e, &mut |_| true).unwrap();
        assert_eq!(
            out,
            CaughtUp {
                documents: 1,
                stopped: false
            }
        );
        assert!(store.index().len() > EMBED_BATCH);
        assert!(store.index().entries().iter().all(|x| x.doc_id == big.id));
        // Nor is a binary document stored
        assert!(store.add(&garbage, &e).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// A folder of before: another program's files, and ours when `data`
    fn old_folder(name: &str, data: bool) -> PathBuf {
        let old = temp(name);
        std::fs::create_dir_all(old.join("WebKit")).unwrap();
        std::fs::write(old.join("hsts-storage.sqlite"), b"not ours").unwrap();
        std::fs::write(old.join("WebKit/cache"), b"not ours either").unwrap();
        std::fs::create_dir_all(old.join("docs")).unwrap();
        std::fs::create_dir_all(old.join("models/m")).unwrap();
        std::fs::write(old.join("models/m/w.bin"), b"weights").unwrap();
        if data {
            let e = HashEmbedder { dim: 64 };
            let mut store = Store::open(&old, &e).unwrap();
            store
                .add(&doc("a", "Eggs", "Bank the golden eggs."), &e)
                .unwrap();
            store.save().unwrap();
            std::fs::write(old.join("glossary.toml"), crate::glossary::SEED).unwrap();
        }
        old
    }

    /// The other program's files are there, unchanged
    fn foreign_untouched(old: &Path) {
        assert_eq!(
            std::fs::read(old.join("hsts-storage.sqlite")).unwrap(),
            b"not ours"
        );
        assert_eq!(
            std::fs::read(old.join("WebKit/cache")).unwrap(),
            b"not ours either"
        );
    }

    #[test]
    fn migrates_our_entries_only() {
        let old = old_folder("old", true);
        let new = temp("new");
        let done = migrate(&old, &new).unwrap();
        assert_eq!(done.copied.len(), 3, "{done:?}");
        let e = HashEmbedder { dim: 64 };
        assert_eq!(Store::open(&new, &e).unwrap().index().len(), 1);
        assert!(new.join("glossary.toml").is_file());
        assert!(!new.join("models").exists());
        // Ours moved into a folder of its own, nothing deleted; the other
        // program's files stay where they were
        let aside = done.moved_aside.unwrap();
        assert_eq!(aside.parent(), Some(old.as_path()));
        let name = aside.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("procon-migrated-"), "{name}");
        assert!(name.ends_with(".safe-to-delete"), "{name}");
        for ours in ["docs", "index", "models", "glossary.toml"] {
            assert!(!old.join(ours).exists(), "{ours}");
            assert!(aside.join(ours).exists(), "{ours}");
        }
        foreign_untouched(&old);
        assert_eq!(moved_aside(&old), core::slice::from_ref(&aside));
        // A second run finds nothing of ours
        assert_eq!(migrate(&old, &new).unwrap(), Migration::default());
        foreign_untouched(&old);
        for dir in [old, new] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn moves_empty_folders_without_copying() {
        let old = old_folder("old-empty", false);
        let new = temp("new-empty");
        let done = migrate(&old, &new).unwrap();
        assert!(done.copied.is_empty());
        let aside = done.moved_aside.unwrap();
        assert!(aside.join("docs").is_dir() && aside.join("models/m/w.bin").is_file());
        assert!(!old.join("docs").exists());
        assert!(!new.exists());
        foreign_untouched(&old);
        std::fs::remove_dir_all(old).unwrap();
    }

    #[test]
    fn moves_nothing_when_the_new_folder_has_data() {
        let old = old_folder("old-kept", true);
        let new = temp("new-kept");
        let e = HashEmbedder { dim: 64 };
        let mut other = Store::open(&new, &e).unwrap();
        other
            .add(&doc("b", "Tides", "Low tide moves the basket."), &e)
            .unwrap();
        other.save().unwrap();
        assert_eq!(migrate(&old, &new).unwrap(), Migration::default());
        assert!(old.join("docs").is_dir() && old.join("index/meta.json").is_file());
        assert!(moved_aside(&old).is_empty());
        foreign_untouched(&old);
        for dir in [old, new] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
