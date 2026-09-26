//! Jargon across languages.
//!
//! A glossary is a TOML file of `[[term]]` tables (see `glossary.toml` in
//! this crate for the seed):
//!
//! ```toml
//! [[term]]
//! id = "steelhead"
//! definition = "Boss that grows a bomb on its head; shoot the bomb."
//! forms = { en = ["Steelhead"], ja = ["バクダン"] }
//! ```
//!
//! Retrieval finds the terms used in a query and adds their other-language
//! forms ([`Glossary::expand`]), so a Japanese question also matches English
//! notes; prompts list the matched terms ([`Glossary::prompt_lines`]) so the
//! model uses the community's names.
//!
//! Name tables imported from the inbox ([`crate::tables`]) add terms without
//! definitions, each with where it came from ([`Term::from`]);
//! [`Glossary::merge`] folds them into the terms that share a name.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The seed shipped with the crate
pub const SEED: &str = include_str!("../glossary.toml");

/// One term
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Term {
    /// Stable id (`steelhead`)
    pub id: String,
    /// Short English definition; empty for imported names
    #[serde(default)]
    pub definition: String,
    /// Names per language code, official name first
    pub forms: BTreeMap<String, Vec<String>>,
    /// Where imported names came from: `<file>#<key>`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
}

impl Term {
    /// The main name in `lang`, if the glossary has one
    pub fn name(&self, lang: &str) -> Option<&str> {
        self.forms.get(lang)?.first().map(String::as_str)
    }
}

/// A set of terms
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Glossary {
    /// The terms, in file order
    #[serde(rename = "term", default)]
    pub terms: Vec<Term>,
}

/// True for scripts written without spaces, where a form may sit inside a
/// longer word
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af)
}

impl Glossary {
    /// Parses a glossary file's content
    pub fn parse(toml_text: &str) -> Result<Self> {
        toml::from_str(toml_text).context("invalid glossary")
    }

    /// Reads a glossary file
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| alloc::format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| alloc::format!("in {}", path.display()))
    }

    /// The seed glossary shipped with the crate
    pub fn seed() -> Self {
        Self::parse(SEED).expect("the seed glossary parses")
    }

    /// The term with this id or any form equal to `name` (ignoring case)
    pub fn lookup(&self, name: &str) -> Option<&Term> {
        let name = name.trim().to_lowercase();
        self.terms
            .iter()
            .find(|t| t.id == name || t.forms.values().flatten().any(|f| f.to_lowercase() == name))
    }

    /// Terms mentioned in `text`, in order of first mention. Latin forms must
    /// stand as whole words; longer matches win over forms inside them
    /// (`キンシャケ` is Goldie, not Chum).
    pub fn find_in(&self, text: &str) -> Vec<&Term> {
        let hay = text.to_lowercase();
        // (start, end, term index) of every occurrence
        let mut hits: Vec<(usize, usize, usize)> = Vec::new();
        for (ti, term) in self.terms.iter().enumerate() {
            for form in term.forms.values().flatten() {
                let needle = form.to_lowercase();
                if needle.is_empty() {
                    continue;
                }
                let cjk = needle.chars().any(is_cjk);
                for (start, _) in hay.match_indices(&needle) {
                    let end = start + needle.len();
                    let before = hay[..start].chars().next_back();
                    let after = hay[end..].chars().next();
                    let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
                    if cjk || (bounded(before) && bounded(after)) {
                        hits.push((start, end, ti));
                    }
                }
            }
        }
        // Longest first, dropping matches that overlap a longer one
        hits.sort_by_key(|&(s, e, _)| (core::cmp::Reverse(e - s), s));
        let mut taken: Vec<(usize, usize, usize)> = Vec::new();
        for h in hits {
            if taken.iter().all(|t| h.1 <= t.0 || h.0 >= t.1) {
                taken.push(h);
            }
        }
        taken.sort_by_key(|&(s, _, _)| s);
        let mut out: Vec<&Term> = Vec::new();
        for (_, _, ti) in taken {
            let term = &self.terms[ti];
            if !out.iter().any(|t| t.id == term.id) {
                out.push(term);
            }
        }
        out
    }

    /// `text` followed by every form of the terms it mentions, so the query
    /// embedding also points at documents in other languages
    pub fn expand(&self, text: &str) -> String {
        let mut out = String::from(text);
        for term in self.find_in(text) {
            for form in term.forms.values().flatten() {
                if !text.to_lowercase().contains(&form.to_lowercase()) {
                    out.push_str(" / ");
                    out.push_str(form);
                }
            }
        }
        out
    }

    /// Adds imported terms: a term sharing a name (ignoring case) with one
    /// already here adds its names and origin to it; others are appended,
    /// with their id made unique
    pub fn merge(&mut self, terms: &[Term]) {
        // Lowercase name or id to the index of its term
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        let key = |s: &str| s.trim().to_lowercase();
        for (i, t) in self.terms.iter().enumerate() {
            names.entry(key(&t.id)).or_insert(i);
            for f in t.forms.values().flatten() {
                names.entry(key(f)).or_insert(i);
            }
        }
        for t in terms {
            let found = t.forms.values().flatten().find_map(|f| names.get(&key(f)));
            let i = match found {
                Some(&i) => i,
                None => {
                    let mut new = Term {
                        forms: BTreeMap::new(),
                        from: Vec::new(),
                        ..t.clone()
                    };
                    let mut n = 1;
                    while names.contains_key(&key(&new.id)) {
                        n += 1;
                        new.id = alloc::format!("{}-{n}", t.id);
                    }
                    names.insert(key(&new.id), self.terms.len());
                    self.terms.push(new);
                    self.terms.len() - 1
                }
            };
            let term = &mut self.terms[i];
            for (lang, forms) in &t.forms {
                let list = term.forms.entry(lang.clone()).or_default();
                for f in forms {
                    if !list.iter().any(|g| key(g) == key(f)) {
                        list.push(f.clone());
                    }
                    names.entry(key(f)).or_insert(i);
                }
            }
            for f in &t.from {
                if !term.from.contains(f) {
                    term.from.push(f.clone());
                }
            }
        }
    }

    /// Number of terms with a name in each language
    pub fn languages(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for t in &self.terms {
            for (lang, forms) in &t.forms {
                if !forms.is_empty() {
                    *counts.entry(lang.clone()).or_default() += 1;
                }
            }
        }
        counts
    }

    /// One line per term for a prompt: its names and definition, with the
    /// `lang` name first when given
    pub fn prompt_lines(terms: &[&Term], lang: Option<&str>) -> String {
        let mut out = String::new();
        for t in terms {
            let mut names: Vec<String> = Vec::new();
            if let Some(n) = lang.and_then(|l| t.name(l)) {
                names.push(alloc::format!("{}: {n}", lang.unwrap_or_default()));
            }
            for (l, forms) in &t.forms {
                if Some(l.as_str()) != lang {
                    names.push(alloc::format!("{l}: {}", forms.join(", ")));
                }
            }
            out.push_str(&alloc::format!("- {} ({})", t.id, names.join("; ")));
            if !t.definition.is_empty() {
                out.push_str(": ");
                out.push_str(&t.definition);
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_parses() {
        let g = Glossary::seed();
        assert!(g.terms.len() > 30);
        assert_eq!(g.lookup("steelhead").unwrap().name("ja"), Some("バクダン"));
    }

    #[test]
    fn looks_up_any_form() {
        let g = Glossary::seed();
        assert_eq!(g.lookup("バクダン").unwrap().id, "steelhead");
        assert_eq!(g.lookup("  STEELHEAD ").unwrap().id, "steelhead");
        assert!(g.lookup("nothing").is_none());
    }

    #[test]
    fn finds_whole_words_only() {
        let g = Glossary::seed();
        let ids: Vec<_> = g
            .find_in("Kill the Flyfish, then the steelhead. Chumming is not Chum.")
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["flyfish", "steelhead", "chum"]);
    }

    #[test]
    fn longer_japanese_forms_win() {
        let g = Glossary::seed();
        let ids: Vec<_> = g
            .find_in("巨大タツマキでキンシャケを見る")
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, ["giant-tornado", "goldie"]);
    }

    #[test]
    fn expands_across_languages() {
        let g = Glossary::seed();
        let q = g.expand("バクダンの処理");
        assert!(q.contains("Steelhead"), "{q}");
    }

    #[test]
    fn merges_imported_names() {
        let mut g = Glossary::seed();
        let size = g.terms.len();
        let imported: Glossary = Glossary::parse(
            r#"
            [[term]]
            id = "steelhead"
            forms = { en = ["Steelhead"], zh = ["Bomb Salmon"], fr = ["Tête-de-pneu"] }
            from = ["inbox/names.csv#SakelienBomber"]
            [[term]]
            id = "maws"
            forms = { de = ["Maws DE"] }
            [[term]]
            id = "maws"
            forms = { en = ["Something else"] }
            "#,
        )
        .unwrap();
        g.merge(&imported.terms);
        let steelhead = g.lookup("Tête-de-pneu").unwrap();
        assert_eq!(steelhead.id, "steelhead");
        assert_eq!(steelhead.name("ja"), Some("バクダン"));
        assert_eq!(steelhead.from, ["inbox/names.csv#SakelienBomber"]);
        assert!(!steelhead.definition.is_empty());
        // No shared name: a new term, with an id of its own
        assert_eq!(g.terms.len(), size + 2);
        assert_eq!(g.lookup("Maws DE").unwrap().id, "maws-2");
        assert_eq!(g.lookup("Something else").unwrap().id, "maws-3");
        assert!(g.languages()["fr"] >= 1);
        let line = Glossary::prompt_lines(&[g.lookup("maws-2").unwrap()], None);
        assert_eq!(line, "- maws-2 (de: Maws DE)\n");
    }

    #[test]
    fn prompt_lines_put_target_first() {
        let g = Glossary::seed();
        let t = g.lookup("Maws").unwrap();
        let s = Glossary::prompt_lines(&[t], Some("ja"));
        assert!(s.starts_with("- maws (ja: モグラ; en: Maws)"), "{s}");
    }
}
