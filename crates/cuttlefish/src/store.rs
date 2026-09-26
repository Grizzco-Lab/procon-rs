//! The knowledge store: a data folder outside the repository, which may
//! be a synced folder (Dropbox, rclone).
//!
//! ```text
//! <data>/
//!   inbox/             anything dropped here, read by [`crate::inbox`]
//!   inbox.json         what the inbox import took from each file (hashes)
//!   glossary.toml      your glossary (the crate's seed until you add one)
//!   terms/<id>.json    name tables imported from the inbox ([`crate::tables`])
//!   assets.json        images and icons from the inbox ([`crate::assets`])
//!   reports/<t>.json   what each inbox import did
//!   digest.md          curated fundamentals, sent with every review
//!   raw/<kind>/...     fetched pages, subtitles and exports as received
//!   docs/<id>.json     processed documents
//!   index/             chunk vectors (see [`crate::index`])
//! ```
//!
//! The folder is `--data`, else `$CUTTLEFISH_DATA`, else
//! `~/.local/share/cuttlefish` (the studio passes its `[cuttlefish]
//! knowledge`). What needs no syncing stays on this machine, in
//! [`cache_dir`]: the embedding model, thumbnails, unpacked archives.
//! [`migrate`] copies a folder of the older layout (everything in
//! `~/.local/share/cuttlefish`) over and, once the copy checks out, renames
//! the old folder to `*.migrated-<date>.safe-to-delete`.
//!
//! Safe on a synced folder: every file is written whole through a
//! temporary file and a rename ([`write_atomic`]), nothing is locked, and
//! files that arrive half-synced or as conflict copies are skipped with a
//! warning. Documents are the truth and the index follows them: chunks of
//! documents that are gone are dropped on opening, an index that does not
//! read is rebuilt, and [`Store::catch_up`] embeds documents synced in from
//! elsewhere.

use crate::chunk::{ChunkConfig, chunk_text};
use crate::doc::{Document, SourceKind};
use crate::embed::{Embedder, Role};
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

/// The data folder of before, and the CLI's default without
/// `$CUTTLEFISH_DATA`: `$XDG_DATA_HOME/cuttlefish`, else
/// `~/.local/share/cuttlefish`
pub fn legacy_root() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home().join(".local/share"), PathBuf::from)
        .join("cuttlefish")
}

/// Folder on this machine for what is not synced (the embedding model,
/// thumbnails, unpacked archives): `$CUTTLEFISH_CACHE`, else
/// `$XDG_CACHE_HOME/cuttlefish`, else `~/.cache/cuttlefish`
pub fn cache_dir() -> PathBuf {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    if let Some(dir) = var("CUTTLEFISH_CACHE") {
        return PathBuf::from(dir);
    }
    var("XDG_CACHE_HOME")
        .map_or_else(|| home().join(".cache"), PathBuf::from)
        .join("cuttlefish")
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
    /// The default data folder: `$CUTTLEFISH_DATA` or [`legacy_root`]
    pub fn default_root() -> PathBuf {
        match std::env::var_os("CUTTLEFISH_DATA").filter(|d| !d.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => legacy_root(),
        }
    }

    /// Opens (or creates) a data folder for vectors of `embedder`. The
    /// folder's parent must exist, so a synced folder that is missing is not
    /// silently replaced by a new one. Chunks of documents that are gone
    /// are dropped; an index that does not read starts empty, for
    /// [`Store::catch_up`] to rebuild.
    pub fn open(root: &Path, embedder: &dyn Embedder) -> Result<Self> {
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

    /// The data folder's `glossary.toml` (or the seed without one), with
    /// the name tables imported from the inbox merged in
    pub fn load_glossary(root: &Path) -> Result<Glossary> {
        let path = root.join("glossary.toml");
        let mut glossary = if path.exists() {
            Glossary::load(&path)?
        } else {
            Glossary::seed()
        };
        for table in crate::tables::load_all(root) {
            glossary.merge(&table.terms);
        }
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
        self.doc_path(&crate::doc::doc_id(key)).exists()
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

    fn index_doc(&mut self, doc: &Document, embedder: &dyn Embedder) -> Result<usize> {
        self.index.remove_doc(&doc.id);
        let chunks = chunk_text(&doc.text, &self.chunking);
        let inputs: Vec<String> = chunks
            .iter()
            .map(|c| passage_text(&doc.title, &c.heading, &c.text))
            .collect();
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let vectors = embedder.embed(&refs, Role::Passage)?;
        for (c, v) in chunks.iter().zip(vectors) {
            let entry = Entry {
                doc_id: doc.id.clone(),
                ordinal: c.ordinal,
                title: doc.title.clone(),
                heading: c.heading.clone(),
                url: doc.url.clone(),
                source: doc.source,
                license: doc.license.clone(),
                language: doc.language.clone(),
                weight: doc.weight,
                text: c.text.clone(),
            };
            self.index.add(entry, v)?;
        }
        Ok(chunks.len())
    }

    /// Every stored document
    pub fn documents(&self) -> Result<Vec<Document>> {
        read_documents(&self.root)
    }

    /// Embeds the documents the index lacks (synced in from another
    /// machine, or all of them after the index was rebuilt); returns how
    /// many. Call [`Store::save`] afterwards when it is not 0.
    pub fn catch_up(&mut self, embedder: &dyn Embedder) -> Result<usize> {
        let indexed: BTreeSet<String> = self
            .index
            .entries()
            .iter()
            .map(|e| e.doc_id.clone())
            .collect();
        let mut n = 0;
        for doc in self.documents()? {
            if !indexed.contains(&doc.id) && !doc.text.trim().is_empty() {
                self.index_doc(&doc, embedder)?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Re-chunks and re-embeds every stored document into a fresh index
    /// (after changing the embedder or the chunk sizes)
    pub fn reindex(&mut self, embedder: &dyn Embedder) -> Result<usize> {
        self.index = FlatIndex::new(embedder.name(), embedder.dim());
        let mut n = 0;
        for doc in self.documents()? {
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
        if self.index.is_empty() {
            return Ok(Vec::new());
        }
        let q = self.glossary.expand(query);
        let v = embedder.embed(&[&q], Role::Query)?.remove(0);
        Ok(self
            .index
            .search(&v, k)
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

/// Whether a data folder holds anything of its own: documents, an index, a
/// glossary or a digest
fn holds_data(root: &Path) -> bool {
    doc_ids(root).is_ok_and(|ids| !ids.is_empty())
        || ["index/meta.json", "glossary.toml", "digest.md"]
            .iter()
            .any(|p| root.join(p).exists())
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

/// Whether `copy` has every file of `original`, with the same size
fn has_all(original: &Path, copy: &Path) -> bool {
    let copied = sizes(copy);
    sizes(original)
        .iter()
        .all(|(path, size)| copied.get(path) == Some(size))
}

/// Chunks in a data folder's index (0 without one); `None` if it does not
/// read
fn index_len(root: &Path) -> Option<usize> {
    match FlatIndex::load(&root.join("index")) {
        Ok(Some(i)) => Some(i.len()),
        Ok(None) => Some(0),
        Err(_) => None,
    }
}

/// Whether `root` holds the data of `old`: the same documents, as many
/// chunks in the index, every raw download, the same glossary and digest
fn holds_copy_of(old: &Path, root: &Path) -> bool {
    let same_file = |name: &str| {
        let a = std::fs::read(old.join(name)).ok();
        a.is_none() || a == std::fs::read(root.join(name)).ok()
    };
    doc_ids(old).ok() == doc_ids(root).ok()
        && index_len(old).is_some()
        && index_len(old) == index_len(root)
        && has_all(&old.join("raw"), &root.join("raw"))
        && same_file("glossary.toml")
        && same_file("digest.md")
}

/// The name `old` is moved aside to after [`migrate`]:
/// `<name>.migrated-<YYYY-MM-DD>.safe-to-delete` beside it
fn aside_name(old: &Path) -> Option<PathBuf> {
    let name = old.file_name()?.to_string_lossy();
    let date = chrono::Local::now().format("%Y-%m-%d");
    (1..100)
        .map(|n| match n {
            1 => alloc::format!("{name}.migrated-{date}.safe-to-delete"),
            n => alloc::format!("{name}.migrated-{date}-{n}.safe-to-delete"),
        })
        .map(|aside| old.with_file_name(aside))
        .find(|aside| !aside.exists())
}

/// Folders [`migrate`] moved aside from `old` that are still there
pub fn moved_aside(old: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (old.parent(), old.file_name()) else {
        return Vec::new();
    };
    let prefix = alloc::format!("{}.migrated-", name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&prefix) && n.ends_with(".safe-to-delete")
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
    /// Where the old folder was moved aside to
    pub moved_aside: Option<PathBuf>,
}

/// Brings a data folder of the older layout (`old`, usually
/// [`legacy_root`]) over: its embedding model is copied into `models` (see
/// [`models_dir`]), and its documents, index, raw downloads, glossary and
/// digest into `root` when `root` holds none of these yet. Then, if
/// everything of ours in `old` is found in its new place (every model file
/// with its size; the same documents and as many index entries, raw files,
/// glossary and digest), `old` is renamed to
/// `<name>.migrated-<date>.safe-to-delete` beside it, never deleted. It is
/// left as it is when `old` is `root`, when `root`'s parent is missing (an
/// unmounted synced folder) or when anything differs.
pub fn migrate(old: &Path, root: &Path, models: &Path) -> Result<Migration> {
    let mut done = Migration::default();
    if !old.is_dir() {
        return Ok(done);
    }
    let old_models = old.join("models");
    if old_models.is_dir() && old_models != models {
        let n = copy_tree(&old_models, models)?;
        if n > 0 {
            done.copied.push(alloc::format!(
                "{n} model files from {} to {}",
                old_models.display(),
                models.display()
            ));
        }
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
    let data = holds_data(old);
    if data && !holds_data(root) {
        for name in ["docs", "index", "raw"] {
            if old.join(name).is_dir() {
                let n = copy_tree(&old.join(name), &root.join(name))?;
                done.copied.push(alloc::format!("{n} files of {name}/"));
            }
        }
        for name in ["glossary.toml", "digest.md"] {
            let from = old.join(name);
            if from.is_file() && !root.join(name).exists() {
                write_atomic(&root.join(name), &std::fs::read(&from)?)?;
                done.copied.push(String::from(name));
            }
        }
    }
    if !done.copied.is_empty() {
        log::info!(
            "Copied from {} to {}: {}",
            old.display(),
            root.display(),
            done.copied.join(", ")
        );
    }
    let models_ok = !old_models.is_dir() || has_all(&old_models, models);
    if !models_ok || (data && !holds_copy_of(old, root)) {
        log::warn!(
            "{} is not moved aside: {} does not hold all of it; nothing was deleted",
            old.display(),
            if models_ok { root } else { models }.display()
        );
        return Ok(done);
    }
    let aside = aside_name(old).context("no name to move the old folder aside to")?;
    std::fs::rename(old, &aside).with_context(|| alloc::format!("renaming {}", old.display()))?;
    log::info!(
        "Moved the old knowledge folder {} aside to {}: its contents are copied to {} and {}; it is safe to delete",
        old.display(),
        aside.display(),
        root.display(),
        models.display()
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
        assert_eq!(store.catch_up(&e).unwrap(), 1);
        assert_eq!(store.catch_up(&e).unwrap(), 0);
        assert_eq!(store.documents().unwrap().len(), 2);
        // A half-synced index is rebuilt
        std::fs::write(root.join("index/vectors.f32"), b"xx").unwrap();
        let mut store = Store::open(&root, &e).unwrap();
        assert_eq!(store.index().len(), 0);
        assert_eq!(store.catch_up(&e).unwrap(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn migrates_by_copying() {
        let old = temp("old");
        let new = temp("new");
        let models = temp("models");
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&old, &e).unwrap();
        store
            .add(&doc("a", "Eggs", "Bank the golden eggs."), &e)
            .unwrap();
        store.save().unwrap();
        std::fs::create_dir_all(old.join("models/m")).unwrap();
        std::fs::write(old.join("models/m/w.bin"), b"weights").unwrap();
        std::fs::write(old.join("hsts-storage.sqlite"), b"not ours").unwrap();
        let done = migrate(&old, &new, &models).unwrap();
        assert_eq!(done.copied.len(), 3, "{done:?}");
        assert_eq!(std::fs::read(models.join("m/w.bin")).unwrap(), b"weights");
        assert!(!new.join("hsts-storage.sqlite").exists());
        assert!(!new.join("models").exists());
        assert_eq!(Store::open(&new, &e).unwrap().index().len(), 1);
        // Everything matched: the old folder is moved aside, not deleted
        let aside = done.moved_aside.unwrap();
        assert!(!old.exists());
        let name = aside.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.ends_with(".safe-to-delete"), "{name}");
        assert!(aside.join("docs").read_dir().unwrap().next().is_some());
        assert_eq!(moved_aside(&old), core::slice::from_ref(&aside));
        // A second run finds nothing to do
        assert_eq!(migrate(&old, &new, &models).unwrap(), Migration::default());
        for dir in [aside, new, models] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn keeps_the_old_folder_when_the_copy_differs() {
        let old = temp("old-kept");
        let new = temp("new-kept");
        let models = temp("models-kept");
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&old, &e).unwrap();
        store
            .add(&doc("a", "Eggs", "Bank the golden eggs."), &e)
            .unwrap();
        store.save().unwrap();
        // The new folder holds other documents already: nothing is copied
        // and the old folder stays where it is
        let mut other = Store::open(&new, &e).unwrap();
        other
            .add(&doc("b", "Tides", "Low tide moves the basket."), &e)
            .unwrap();
        other.save().unwrap();
        let done = migrate(&old, &new, &models).unwrap();
        assert_eq!(done, Migration::default());
        assert!(old.join("docs").is_dir());
        assert!(moved_aside(&old).is_empty());
        // Nor when a model file did not arrive whole
        std::fs::remove_dir_all(new.join("docs")).unwrap();
        std::fs::remove_dir_all(new.join("index")).unwrap();
        std::fs::create_dir_all(old.join("models")).unwrap();
        std::fs::write(old.join("models/w.bin"), b"weights").unwrap();
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("w.bin"), b"cut").unwrap();
        let done = migrate(&old, &new, &models).unwrap();
        assert_eq!(done.moved_aside, None);
        assert!(old.is_dir());
        for dir in [old, new, models] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
