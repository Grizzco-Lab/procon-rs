//! Vector index over chunks.
//!
//! [`FlatIndex`] compares the query with every vector: a few hundred
//! thousand chunks of 384 floats search in milliseconds, which covers every
//! source listed so far. A larger store can implement [`VectorIndex`] with an
//! approximate index (HNSW, IVF) or an external database.
//!
//! On disk, in the index folder: `meta.json` (embedder name, dimension),
//! `entries.jsonl` (one [`Entry`] per line) and `vectors.f32` (the vectors,
//! little-endian `f32`, in entry order).

use crate::doc::SourceKind;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::Path;

/// A chunk with what a citation needs from its document
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Document id
    pub doc_id: String,
    /// Chunk position in the document
    pub ordinal: u32,
    /// Document title
    pub title: String,
    /// Heading path inside the document
    pub heading: String,
    /// Document url
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Kind of source
    pub source: SourceKind,
    /// Document license
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Document language
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Retrieval weight
    pub weight: f32,
    /// Chunk text
    pub text: String,
}

/// How much a source weight moves the score: E5 cosines of related texts
/// sit within a few hundredths of each other (0.8-0.9), so a weight of 1.2
/// adds 0.02, enough to prefer the better source among close matches
/// without burying a clearly better match
pub const WEIGHT_SCALE: f32 = 0.1;

/// Ranking score: cosine similarity plus the source weight's bonus
pub fn score(cosine: f32, weight: f32) -> f32 {
    cosine + WEIGHT_SCALE * (weight - 1.0)
}

/// Search operations a store needs; implement it to swap the index
pub trait VectorIndex {
    /// Adds one chunk with its unit vector
    fn add(&mut self, entry: Entry, vector: Vec<f32>) -> Result<()>;
    /// Removes every chunk of a document
    fn remove_doc(&mut self, doc_id: &str);
    /// The `k` best entries for a unit query vector, best first, by
    /// [`score`]
    fn search(&self, query: &[f32], k: usize) -> Vec<(&Entry, f32)>;
    /// Number of chunks
    fn len(&self) -> usize;
    /// True without chunks
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    embedder: String,
    dim: usize,
}

/// Exhaustive cosine search
pub struct FlatIndex {
    embedder: String,
    dim: usize,
    entries: Vec<Entry>,
    vectors: Vec<f32>,
}

impl FlatIndex {
    /// An empty index for vectors of `embedder`
    pub fn new(embedder: &str, dim: usize) -> Self {
        FlatIndex {
            embedder: String::from(embedder),
            dim,
            entries: Vec::new(),
            vectors: Vec::new(),
        }
    }

    /// Name of the embedder the vectors came from
    pub fn embedder(&self) -> &str {
        &self.embedder
    }

    /// All entries, in insertion order
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Reads an index folder; `None` if it has none yet
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let meta_path = dir.join("meta.json");
        if !meta_path.exists() {
            return Ok(None);
        }
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
        let file = std::fs::File::open(dir.join("entries.jsonl"))?;
        let mut entries = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if !line.trim().is_empty() {
                entries.push(serde_json::from_str(&line).context("bad index entry")?);
            }
        }
        let bytes = std::fs::read(dir.join("vectors.f32"))?;
        let vectors: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if vectors.len() != entries.len() * meta.dim {
            bail!("index in {} is inconsistent; rebuild it", dir.display());
        }
        Ok(Some(FlatIndex {
            embedder: meta.embedder,
            dim: meta.dim,
            entries,
            vectors,
        }))
    }

    /// Writes the index folder (each file replaced whole)
    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let meta = Meta {
            embedder: self.embedder.clone(),
            dim: self.dim,
        };
        write_atomic(
            &dir.join("meta.json"),
            serde_json::to_string_pretty(&meta)?.as_bytes(),
        )?;
        let mut lines = Vec::new();
        for e in &self.entries {
            serde_json::to_writer(&mut lines, e)?;
            lines.push(b'\n');
        }
        write_atomic(&dir.join("entries.jsonl"), &lines)?;
        let bytes: Vec<u8> = self.vectors.iter().flat_map(|v| v.to_le_bytes()).collect();
        write_atomic(&dir.join("vectors.f32"), &bytes)
    }
}

/// Writes through a temporary file so a crash never leaves half a file
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl VectorIndex for FlatIndex {
    fn add(&mut self, entry: Entry, vector: Vec<f32>) -> Result<()> {
        if vector.len() != self.dim {
            bail!("vector has {} dimensions, index {}", vector.len(), self.dim);
        }
        self.entries.push(entry);
        self.vectors.extend(vector);
        Ok(())
    }

    fn remove_doc(&mut self, doc_id: &str) {
        let dim = self.dim;
        let mut keep_vectors = Vec::with_capacity(self.vectors.len());
        let mut keep_entries = Vec::with_capacity(self.entries.len());
        for (i, e) in self.entries.drain(..).enumerate() {
            if e.doc_id != doc_id {
                keep_vectors.extend_from_slice(&self.vectors[i * dim..(i + 1) * dim]);
                keep_entries.push(e);
            }
        }
        self.entries = keep_entries;
        self.vectors = keep_vectors;
    }

    fn search(&self, query: &[f32], k: usize) -> Vec<(&Entry, f32)> {
        let mut scored: Vec<(usize, f32)> = self
            .vectors
            .chunks_exact(self.dim)
            .enumerate()
            .map(|(i, v)| {
                let cos: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
                (i, score(cos, self.entries[i].weight))
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(i, s)| (&self.entries[i], s))
            .collect()
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::{Embedder, HashEmbedder, Role};

    fn entry(doc: &str, text: &str, weight: f32) -> Entry {
        Entry {
            doc_id: String::from(doc),
            ordinal: 0,
            title: String::from(doc),
            heading: String::new(),
            url: None,
            source: SourceKind::Wiki,
            license: None,
            language: None,
            weight,
            text: String::from(text),
        }
    }

    fn fixture() -> (HashEmbedder, FlatIndex) {
        let e = HashEmbedder { dim: 256 };
        let mut index = FlatIndex::new(e.name(), e.dim());
        let docs = [
            (
                "steelhead",
                "shoot the steelhead bomb before it throws",
                1.0,
            ),
            ("flyfish", "throw a bomb into each flyfish missile pod", 1.0),
            ("tides", "low tide moves the basket down the shore", 1.0),
        ];
        for (id, text, w) in docs {
            let v = e.embed(&[text], Role::Passage).unwrap().remove(0);
            index.add(entry(id, text, w), v).unwrap();
        }
        (e, index)
    }

    fn top(e: &HashEmbedder, index: &FlatIndex, q: &str) -> String {
        let v = e.embed(&[q], Role::Query).unwrap().remove(0);
        index.search(&v, 1)[0].0.doc_id.clone()
    }

    #[test]
    fn finds_the_matching_chunk() {
        let (e, index) = fixture();
        assert_eq!(top(&e, &index, "flyfish missile pod"), "flyfish");
        assert_eq!(top(&e, &index, "basket at low tide"), "tides");
        let v = e.embed(&["bomb"], Role::Query).unwrap().remove(0);
        assert_eq!(index.search(&v, 10).len(), 3);
    }

    #[test]
    fn weight_breaks_ties() {
        let e = HashEmbedder { dim: 256 };
        let mut index = FlatIndex::new(e.name(), e.dim());
        let v = e.embed(&["egg flow"], Role::Passage).unwrap().remove(0);
        index.add(entry("web", "egg flow", 0.9), v.clone()).unwrap();
        index.add(entry("vod", "egg flow", 1.2), v.clone()).unwrap();
        let hits = index.search(&v, 2);
        assert_eq!(hits[0].0.doc_id, "vod");
        assert!((hits[0].1 - 1.02).abs() < 1e-4);
        assert!((hits[1].1 - 0.99).abs() < 1e-4);
    }

    #[test]
    fn removes_and_round_trips() {
        let (e, mut index) = fixture();
        index.remove_doc("flyfish");
        assert_eq!(index.len(), 2);
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-index-{}", std::process::id()));
        index.save(&dir).unwrap();
        let loaded = FlatIndex::load(&dir).unwrap().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(loaded.entries(), index.entries());
        assert_eq!(top(&e, &loaded, "basket at low tide"), "tides");
        assert_eq!(loaded.embedder(), "hash");
    }

    #[test]
    fn rejects_wrong_dimension() {
        let (_, mut index) = fixture();
        assert!(
            index
                .add(entry("x", "x", 1.0), alloc::vec![0.0; 3])
                .is_err()
        );
    }
}
