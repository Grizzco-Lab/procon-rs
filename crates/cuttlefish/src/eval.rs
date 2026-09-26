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
//! Retrieval is checked without the model (`cuttlefish eval`); answers are
//! checked by phrase with `--answer`, a rough signal to read next to the
//! answers themselves.

use crate::store::Hit;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::Deserialize;

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
        if self.expect_sources.is_empty() {
            return true;
        }
        hits.iter().any(|h| {
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
                text: String::new(),
            },
        };
        assert!(case.retrieved(core::slice::from_ref(&hit)));
        assert!(!case.retrieved(&[]));
        assert_eq!(
            case.points_in("Throw a bomb in each missile pod"),
            ["Missile pod", "bomb"]
        );
    }
}
