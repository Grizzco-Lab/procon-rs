//! A small evaluation set: questions with the sources that should be found
//! and the points a good answer makes.
//!
//! ```toml
//! [[case]]
//! question = "How do you take out a Flyfish?"
//! expect_sources = ["Flyfish"]            # in a top-k hit's title, heading or url
//! expect_points = ["bomb", "missile pod"] # phrases a good answer contains
//! ```
//!
//! Retrieval is checked without the model (`cuttlefish eval`), by each way
//! of ranking ([`compare`]: recall@1 and recall@5 of embeddings, BM25 and
//! both); answers are checked by phrase with `--answer`, a rough signal to
//! read next to the answers themselves. [`RETRIEVAL`] is the crate's own
//! set (`cuttlefish eval retrieval`), over the game-data fact cards, the
//! Inkipedia pages and the #vod-review expert comments of the store.

use crate::embed::Embedder;
use crate::store::{Hit, Retrieval, Store};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::Deserialize;

/// The retrieval set (`questions/retrieval.toml`): event and wave
/// questions, hazard levels, weapons and Salmonids by name, slang,
/// Chinese and Japanese questions over English documents, and expert
/// comments by their message id
pub const RETRIEVAL: &str = include_str!("../questions/retrieval.toml");

/// The ways of ranking [`compare`] measures, in its columns' order
pub const MODES: [Retrieval; 3] = [Retrieval::Embedding, Retrieval::Keyword, Retrieval::Hybrid];

/// Chunks [`compare`] retrieves per question (recall@5 needs five)
pub const COMPARE_K: usize = 5;

/// For each case, the rank (1-based) of its first expected source under
/// each of [`MODES`], `None` when not in the top [`COMPARE_K`]
pub fn compare(
    store: &Store,
    embedder: &dyn Embedder,
    set: &EvalSet,
) -> Result<Vec<[Option<usize>; 3]>> {
    let mut out = Vec::new();
    for case in &set.cases {
        let mut ranks = [None; 3];
        for (rank, mode) in ranks.iter_mut().zip(MODES) {
            let hits = store.search_with(mode, &case.question, COMPARE_K, embedder, &|_| true)?;
            *rank = case.rank(&hits);
        }
        out.push(ranks);
    }
    Ok(out)
}

/// Cases of [`compare`]'s ranks found within the top `n` by mode `m`
/// (an index into [`MODES`])
pub fn recall_at(ranks: &[[Option<usize>; 3]], m: usize, n: usize) -> usize {
    ranks
        .iter()
        .filter(|r| r[m].is_some_and(|r| r <= n))
        .count()
}

/// One question
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Case {
    /// The question, in any language
    pub question: String,
    /// Substrings of the title, heading or url of a source that should be
    /// retrieved (any one is enough)
    #[serde(default)]
    pub expect_sources: Vec<String>,
    /// Phrases a good answer contains
    #[serde(default)]
    pub expect_points: Vec<String>,
}

/// The cases of an evaluation file
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct EvalSet {
    /// Questions in file order
    #[serde(rename = "case", default)]
    pub cases: Vec<Case>,
}

impl EvalSet {
    /// Parses an evaluation file's content
    pub fn parse(toml_text: &str) -> Result<Self> {
        toml::from_str(toml_text).context("invalid evaluation file")
    }
}

impl Case {
    /// True if a hit matches an expected source (or none are expected)
    pub fn retrieved(&self, hits: &[Hit]) -> bool {
        self.expect_sources.is_empty() || self.rank(hits).is_some()
    }

    /// Position (1-based) of the first hit that matches an expected source
    pub fn rank(&self, hits: &[Hit]) -> Option<usize> {
        hits.iter()
            .position(|h| {
                let e = &h.entry;
                let place = alloc::format!(
                    "{} {} {}",
                    e.title,
                    e.heading,
                    e.url.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                self.expect_sources
                    .iter()
                    .any(|s| place.contains(&s.to_lowercase()))
            })
            .map(|i| i + 1)
    }

    /// The expected points an answer contains, ignoring case
    pub fn points_in(&self, answer: &str) -> Vec<&str> {
        let answer = answer.to_lowercase();
        self.expect_points
            .iter()
            .filter(|p| answer.contains(&p.to_lowercase()))
            .map(String::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::SourceKind;
    use crate::index::Entry;

    #[test]
    fn checks_retrieval_and_points() {
        let set = EvalSet::parse(
            "[[case]]\nquestion = \"Flyfish?\"\nexpect_sources = [\"flyfish\"]\n\
             expect_points = [\"Missile pod\", \"bomb\"]\n",
        )
        .unwrap();
        let case = &set.cases[0];
        let hit = Hit {
            score: 0.9,
            entry: Entry {
                doc_id: String::from("d"),
                ordinal: 0,
                title: String::from("Bosses"),
                heading: String::from("Flyfish"),
                url: None,
                source: SourceKind::Wiki,
                license: None,
                language: None,
                weight: 1.0,
                game: None,
                video: None,
                expert: None,
                text: String::new(),
            },
        };
        assert!(case.retrieved(core::slice::from_ref(&hit)));
        assert!(!case.retrieved(&[]));
        let mut other = hit.clone();
        other.entry.heading = String::from("Steelhead");
        assert_eq!(case.rank(&[other, hit]), Some(2));
        assert_eq!(recall_at(&[[Some(1), Some(3), None]], 1, 5), 1);
        assert_eq!(recall_at(&[[Some(1), Some(3), None]], 1, 1), 0);
        assert_eq!(
            case.points_in("Throw a bomb in each missile pod"),
            ["Missile pod", "bomb"]
        );
    }

    #[test]
    fn the_retrieval_set_parses() {
        let set = EvalSet::parse(RETRIEVAL).unwrap();
        assert!(set.cases.len() >= 30);
        assert!(set.cases.iter().all(|c| !c.expect_sources.is_empty()));
    }
}
