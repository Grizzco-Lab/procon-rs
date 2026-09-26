//! The knowledge store: a data folder outside the repository.
//!
//! ```text
//! <data>/
//!   glossary.toml      your glossary (the crate's seed until you add one)
//!   digest.md          curated fundamentals, sent with every review
//!   raw/<kind>/...     fetched pages, subtitles and exports as received
//!   docs/<id>.json     processed documents
//!   index/             chunk vectors (see [`crate::index`])
//!   models/            embedding model files
//! ```
//!
//! The folder is `--data`, else `$CUTTLEFISH_DATA`, else
//! `~/.local/share/cuttlefish`.

use crate::chunk::{ChunkConfig, chunk_text};
use crate::doc::{Document, SourceKind};
use crate::embed::{Embedder, Role};
use crate::glossary::Glossary;
use crate::index::{Entry, FlatIndex, VectorIndex};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use serde::Serialize;
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

/// Documents, their index and the glossary in one data folder
pub struct Store {
    root: PathBuf,
    index: FlatIndex,
    glossary: Glossary,
    chunking: ChunkConfig,
}

impl Store {
    /// The default data folder: `$CUTTLEFISH_DATA` or
    /// `~/.local/share/cuttlefish`
    pub fn default_root() -> PathBuf {
        if let Some(d) = std::env::var_os("CUTTLEFISH_DATA").filter(|d| !d.is_empty()) {
            return PathBuf::from(d);
        }
        let home = std::env::var_os("HOME").unwrap_or_default();
        PathBuf::from(home).join(".local/share/cuttlefish")
    }

    /// Opens (or creates) a data folder for vectors of `embedder`
    pub fn open(root: &Path, embedder: &dyn Embedder) -> Result<Self> {
        std::fs::create_dir_all(root.join("docs"))
            .with_context(|| alloc::format!("creating {}", root.display()))?;
        let index = match FlatIndex::load(&root.join("index"))? {
            Some(i) if i.embedder() != embedder.name() => bail!(
                "the index in {} was built with {}, not {}; remove the index folder and re-embed with `cuttlefish reindex`",
                root.display(),
                i.embedder(),
                embedder.name()
            ),
            Some(i) => i,
            None => FlatIndex::new(embedder.name(), embedder.dim()),
        };
        Ok(Store {
            root: root.to_path_buf(),
            index,
            glossary: Self::load_glossary(root)?,
            chunking: ChunkConfig::default(),
        })
    }

    /// The data folder's `glossary.toml`, or the seed without one
    pub fn load_glossary(root: &Path) -> Result<Glossary> {
        let path = root.join("glossary.toml");
        if path.exists() {
            Glossary::load(&path)
        } else {
            Ok(Glossary::seed())
        }
    }

    /// The data folder
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the document of this url or path is stored
    pub fn has(&self, key: &str) -> bool {
        self.root
            .join("docs")
            .join(alloc::format!("{}.json", crate::doc::doc_id(key)))
            .exists()
    }

    /// Folder for raw downloads of one kind (`web`, `youtube`, `discord`)
    pub fn raw_dir(&self, kind: &str) -> PathBuf {
        self.root.join("raw").join(kind)
    }

    /// Folder for embedding model files
    pub fn models_dir(root: &Path) -> PathBuf {
        root.join("models")
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
        let path = self
            .root
            .join("docs")
            .join(alloc::format!("{}.json", doc.id));
        std::fs::write(&path, serde_json::to_vec_pretty(doc)?)?;
        self.index_doc(doc, embedder)
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
        let mut docs = Vec::new();
        for e in std::fs::read_dir(self.root.join("docs"))? {
            let path = e?.path();
            if path.extension().is_some_and(|x| x == "json") {
                docs.push(serde_json::from_slice(&std::fs::read(&path)?)?);
            }
        }
        docs.sort_by(|a: &Document, b| a.id.cmp(&b.id));
        Ok(docs)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;

    #[test]
    fn adds_replaces_and_searches() {
        let root =
            std::env::temp_dir().join(alloc::format!("cuttlefish-store-{}", std::process::id()));
        let e = HashEmbedder { dim: 256 };
        let mut store = Store::open(&root, &e).unwrap();
        let a = Document::new(
            SourceKind::Wiki,
            "https://wiki/steelhead",
            String::from("Steelhead"),
            String::from("# Strategy\n\nShoot the bomb on its head."),
        );
        let b = Document::new(
            SourceKind::Guide,
            "guide.md",
            String::from("Tides"),
            String::from("At low tide the basket moves down."),
        );
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
}
