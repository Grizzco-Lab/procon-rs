//! The deep question bank: questions a high-level Salmon Run player asks,
//! whose answers the community knows (`questions/deep.toml`, [`SEED`]).
//!
//! Beyond fact questions ("how much health does a Steelhead have?"), these
//! ask for reasoning: why the opening kills are aggressive, which way a
//! Drizzler jumps, where to fight the Mothership on a stage and tide. The
//! page offers a few as chips next to the chat; `cuttlefish eval deep`
//! ([`crate::deep_eval`]) asks the model the ones that need no video; the
//! answers are reviewed and corrected into expert notes ([`crate::notes`]),
//! and a note that answers a question becomes its `reference`.

use crate::notes::Note;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// The bank shipped with the crate
pub const SEED: &str = include_str!("../questions/deep.toml");

/// The categories, with their names for the page: strategy, wave openings
/// and roles, boss mechanics, stage and tide specifics, known occurrences,
/// egg flow, weapon and special use, and questions about a moment of a
/// video
pub const CATEGORIES: [(&str, &str); 8] = [
    ("macro", "Macro and strategy"),
    ("openings", "Wave openings and roles"),
    ("bosses", "Boss mechanics"),
    ("stages", "Stages and tides"),
    ("events", "Known occurrences"),
    ("eggs", "Egg flow"),
    ("weapons", "Weapons and specials"),
    ("moments", "Moments of a video"),
];

/// What answering a question takes
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Needs {
    /// The knowledge store alone
    Knowledge,
    /// A moment of a video: frames, HUD, controller input
    VideoMoment,
    /// A stretch of a video
    VideoRange,
    /// The HUD read from the video (wave, timer, eggs)
    Hud,
    /// A Salmon Run object detector, which is not trained yet
    Detector,
}

impl Needs {
    /// Whether the question is about a video the player is watching
    pub fn video(self) -> bool {
        matches!(self, Needs::VideoMoment | Needs::VideoRange | Needs::Hud)
    }

    /// Whether the model can be asked it without a video
    pub fn askable(self) -> bool {
        self == Needs::Knowledge
    }
}

/// One question of the bank
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// Stable id (`drizzler-jump`)
    pub id: String,
    /// One of [`CATEGORIES`]
    pub category: String,
    pub needs: Needs,
    /// The question in English
    pub en: String,
    /// The question in Simplified Chinese
    pub zh: String,
    /// What is unsure about the question (a term's meaning, say)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The expert note that answers it, by id; empty until one is written
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reference: String,
}

impl Question {
    /// The question in a language (`zh`, else English)
    pub fn text(&self, lang: &str) -> &str {
        match lang.split('-').next().unwrap_or(lang) {
            "zh" => &self.zh,
            _ => &self.en,
        }
    }
}

/// The bank
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Bank {
    #[serde(rename = "question", default)]
    pub questions: Vec<Question>,
}

impl Bank {
    /// Parses a bank file: ids unique, categories known, both languages
    /// given
    pub fn parse(toml_text: &str) -> Result<Self> {
        let bank: Bank = toml::from_str(toml_text).context("invalid question bank")?;
        let mut ids: Vec<&str> = Vec::new();
        for q in &bank.questions {
            ensure!(
                !q.id.is_empty()
                    && q.id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "question id {:?} is not lowercase words joined by dashes",
                q.id
            );
            ensure!(!ids.contains(&q.id.as_str()), "question id {} twice", q.id);
            ids.push(&q.id);
            ensure!(
                CATEGORIES.iter().any(|(c, _)| *c == q.category),
                "question {} has an unknown category {:?}",
                q.id,
                q.category
            );
            ensure!(
                !q.en.trim().is_empty() && !q.zh.trim().is_empty(),
                "question {} lacks its English or Chinese text",
                q.id
            );
        }
        Ok(bank)
    }

    /// The bank shipped with the crate
    pub fn seed() -> Self {
        Self::parse(SEED).expect("the seed question bank parses")
    }

    /// Fills each question's `reference` with the newest note whose
    /// `question_id` names it, where the bank gives none
    pub fn with_references(mut self, notes: &[Note]) -> Self {
        for q in &mut self.questions {
            if q.reference.is_empty()
                && let Some(note) = notes
                    .iter()
                    .filter(|n| n.question_id.as_deref() == Some(q.id.as_str()))
                    .max_by_key(|n| n.date)
            {
                q.reference = note.id.clone();
            }
        }
        self
    }

    /// The questions the model can be asked without a video
    pub fn askable(&self) -> impl Iterator<Item = &Question> {
        self.questions.iter().filter(|q| q.needs.askable())
    }

    /// Questions per category, in [`CATEGORIES`] order
    pub fn counts(&self) -> Vec<(&'static str, usize)> {
        CATEGORIES
            .iter()
            .map(|(c, _)| {
                (
                    *c,
                    self.questions.iter().filter(|q| q.category == *c).count(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_bank_is_sound() {
        let bank = Bank::seed();
        assert!(bank.questions.len() >= 40, "{}", bank.questions.len());
        // Every category has questions
        for (category, n) in bank.counts() {
            assert!(n >= 4, "{category}: {n}");
        }
        // The player's own examples are in it
        let ids: Vec<&str> = bank.questions.iter().map(|q| q.id.as_str()).collect();
        for id in [
            "macro-aggressive-start",
            "macro-first-egg-left",
            "drizzler-jump",
            "opening-role-split",
            "mothership-where-and-when",
            "egg-snatcher-planting",
            "detector-spawn-list",
            "moment-better",
        ] {
            assert!(ids.contains(&id), "{id}");
        }
        let detector = bank
            .questions
            .iter()
            .find(|q| q.id == "detector-spawn-list")
            .unwrap();
        assert_eq!(detector.needs, Needs::Detector);
        assert!(!detector.needs.askable() && !detector.needs.video());
        assert!(Needs::Hud.video());
        // Unsure terms carry a note
        let smuggler = bank
            .questions
            .iter()
            .find(|q| q.id == "mothership-where-and-when")
            .unwrap();
        assert!(smuggler.note.contains("Mothership"));
        assert_eq!(smuggler.text("zh-Hans"), smuggler.zh);
        assert_eq!(smuggler.text("en"), smuggler.en);
        assert!(bank.askable().count() >= 30);
    }

    #[test]
    fn parsing_checks_ids_categories_and_languages() {
        let good = "[[question]]\nid = \"a-b\"\ncategory = \"eggs\"\nneeds = \"knowledge\"\nen = \"E\"\nzh = \"Z\"\n";
        assert_eq!(Bank::parse(good).unwrap().questions.len(), 1);
        for bad in [
            good.replace("a-b", "A_b"),
            good.replace("eggs", "misc"),
            good.replace("zh = \"Z\"", "zh = \" \""),
            alloc::format!("{good}{good}"),
        ] {
            assert!(Bank::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn notes_become_references() {
        let mut note = Note::new("Which way does the Drizzler jump?", "Away from you.");
        note.id = String::from("2026-09-27-drizzler");
        note.question_id = Some(String::from("drizzler-jump"));
        let bank = Bank::seed().with_references(core::slice::from_ref(&note));
        let q = bank
            .questions
            .iter()
            .find(|q| q.id == "drizzler-jump")
            .unwrap();
        assert_eq!(q.reference, "2026-09-27-drizzler");
        assert!(
            bank.questions
                .iter()
                .filter(|q| q.id != "drizzler-jump")
                .all(|q| q.reference.is_empty())
        );
    }
}
