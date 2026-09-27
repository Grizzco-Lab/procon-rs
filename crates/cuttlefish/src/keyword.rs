//! Keyword index over chunks: BM25 over each chunk's title, heading and
//! text, next to the vector index ([`crate::index`]).
//!
//! E5 embeddings place near-identical cards (the waves of one Eggstra Work
//! event, the same wave of two events) within a few thousandths of each
//! other, so a question naming "#7" or "wave 3" can land on the wrong one.
//! Exact terms decide there, and this index matches them.
//!
//! Tokens ([`tokens`]), for mixed English, Chinese and Japanese:
//!
//! - Latin script: lowercase words, a plural `s` dropped, a few English
//!   stop words left out;
//! - numbers as tokens (`7`, `2.5`), and as identifiers: `#7` (also from
//!   "work 7", "event 7" and a number before the CJK counters for the n-th
//!   time, `NTH`), `wave 3` (also from `W3`, `wave3` and a number before
//!   the CJK character for wave, `WAVE`) and `333%`;
//! - CJK (kanji, kana, hangul): overlapping character pairs, a lone
//!   character as itself.
//!
//! The chunks are kept in the vector index's entry order, so the two stay
//! aligned: [`KeywordIndex::add`] and [`KeywordIndex::remove_doc`] follow
//! the vector index's own operations. On disk it is `keywords.json` in the
//! index folder; one that is missing, from another tokenizer version, or
//! not aligned with the entries is rebuilt from them
//! ([`KeywordIndex::from_entries`]; no embedding needed).

use crate::glossary::Glossary;
use crate::index::Entry;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Version of [`tokens`]; an index written by another version is rebuilt
pub const TOKENIZER_VERSION: u32 = 1;

/// BM25 term-frequency saturation
const K1: f32 = 1.2;

/// BM25 length normalisation
const B: f32 = 0.75;

/// The CJK character for a wave (Chinese), after its number
const WAVE: char = '\u{6ce2}';

/// CJK counters for the n-th time (Chinese, Japanese), after an event's
/// number
const NTH: [char; 2] = ['\u{6b21}', '\u{56de}'];

/// English words too common to tell chunks apart
const STOP_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "if", "in", "is", "it", "its", "me", "my", "of", "on", "or", "so", "that", "the", "their",
    "them", "there", "this", "to", "was", "what", "when", "where", "which", "who", "why", "with",
    "you", "your", "should",
];

/// True for kanji, kana and hangul, which are written without spaces
fn is_cjk(c: char) -> bool {
    crate::glossary::is_cjk(c)
}

/// A Latin word as indexed: a plural `s` dropped from longer words
fn word(w: &str) -> String {
    let plural = w.len() > 3
        && w.ends_with('s')
        && !w.ends_with("ss")
        && w.chars().all(|c| c.is_alphabetic());
    String::from(if plural { &w[..w.len() - 1] } else { w })
}

/// True for a number: digits, maybe with a decimal point
fn is_number(w: &str) -> bool {
    w.starts_with(|c: char| c.is_ascii_digit()) && w.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The tokens of a text, in order, repeated as often as they occur (see the
/// module docs)
pub fn tokens(text: &str) -> Vec<String> {
    let text = text.to_lowercase();
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    // The Latin word before the current one, if only spaces lie between
    let mut prev: Option<String> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            let start = i;
            while i < chars.len() && is_cjk(chars[i]) {
                i += 1;
            }
            let run = &chars[start..i];
            if run.len() == 1 {
                out.push(String::from(run[0]));
            }
            for pair in run.windows(2) {
                out.push(pair.iter().collect());
            }
            prev = None;
        } else if c.is_alphanumeric() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() && !is_cjk(chars[i])
                    || chars[i] == '.'
                        && chars[i - 1].is_ascii_digit()
                        && chars.get(i + 1).is_some_and(char::is_ascii_digit))
            {
                i += 1;
            }
            let w: String = chars[start..i].iter().collect();
            let before = start.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i).copied();
            if is_number(&w) {
                out.push(w.clone());
                let after_word = prev.as_deref();
                if before == Some('#')
                    || matches!(after_word, Some("work" | "event"))
                    || after.is_some_and(|c| NTH.contains(&c))
                {
                    out.push(alloc::format!("#{w}"));
                }
                if after_word == Some("wave") || after == Some(WAVE) {
                    out.push(alloc::format!("wave {w}"));
                }
                if after == Some('%') {
                    out.push(alloc::format!("{w}%"));
                }
            } else {
                // `w3` and `wave3` are wave 3
                let digits = w.trim_start_matches(|c: char| c.is_alphabetic());
                let letters = &w[..w.len() - digits.len()];
                if matches!(letters, "w" | "wave") && !digits.is_empty() && is_number(digits) {
                    out.push(alloc::format!("wave {digits}"));
                }
                if !STOP_WORDS.contains(&w.as_str()) {
                    out.push(word(&w));
                }
            }
            prev = Some(w);
        } else {
            if !c.is_whitespace() && c != '#' {
                prev = None;
            }
            i += 1;
        }
    }
    out
}

/// True for an identifier token: an event number (`#7`), a wave (`wave
/// 3`) or a percentage (`333%`)
pub fn is_identifier(token: &str) -> bool {
    token.starts_with('#') || token.starts_with("wave ") || token.ends_with('%')
}

/// A query as [`KeywordIndex::scores`] matches it: its own words, and the
/// glossary terms it mentions ([`Glossary::find_in`]) with every name of
/// each (official ones in every language and approved aliases). A term
/// counts once, as one word: by its best-matching name, averaged over the
/// name's tokens, so a page listing the name in ten languages does not
/// outweigh one that uses it, nor a long CJK name (many character pairs)
/// a short English one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeywordQuery {
    /// Tokens of the query outside the terms' names, each once
    words: Vec<String>,
    /// Per term, the tokens of each of its names
    terms: Vec<Vec<Vec<String>>>,
}

impl KeywordQuery {
    /// The query of a text, expanded through the glossary
    pub fn new(text: &str, glossary: &Glossary) -> Self {
        let mut named: BTreeSet<String> = BTreeSet::new();
        let mut terms = Vec::new();
        for term in glossary.find_in(text) {
            let mut names: Vec<Vec<String>> =
                term.names().map(tokens).filter(|t| !t.is_empty()).collect();
            names.sort();
            names.dedup();
            named.extend(names.iter().flatten().cloned());
            terms.push(names);
        }
        let mut words: Vec<String> = tokens(text)
            .into_iter()
            .filter(|t| !named.contains(t))
            .collect();
        words.sort();
        words.dedup();
        KeywordQuery { words, terms }
    }
}

/// What is indexed of a chunk: its title, heading and text
pub fn chunk_tokens(entry: &Entry) -> Vec<String> {
    let mut t = tokens(&entry.title);
    t.extend(tokens(&entry.heading));
    t.extend(tokens(&entry.text));
    t
}

/// One chunk: its place, token count and term frequencies
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Chunk {
    /// Document id
    doc: String,
    /// Chunk position in the document
    ord: u32,
    /// Tokens in the chunk
    len: u32,
    /// (term, count), sorted by term
    terms: Vec<(u32, u32)>,
}

/// The file's content
#[derive(Serialize, Deserialize)]
struct Saved {
    tokenizer: u32,
    vocab: Vec<String>,
    chunks: Vec<Chunk>,
}

/// BM25 over chunks, in the vector index's entry order
#[derive(Clone, Debug, Default)]
pub struct KeywordIndex {
    vocab: Vec<String>,
    ids: BTreeMap<String, u32>,
    /// Chunks holding each term
    df: Vec<u32>,
    chunks: Vec<Chunk>,
    /// Sum of the chunks' lengths
    total_len: u64,
}

impl KeywordIndex {
    /// The index of these entries, in their order
    pub fn from_entries(entries: &[Entry]) -> Self {
        let mut index = KeywordIndex::default();
        for e in entries {
            index.add(e);
        }
        index
    }

    fn term(&mut self, t: String) -> u32 {
        if let Some(&id) = self.ids.get(&t) {
            return id;
        }
        let id = self.vocab.len() as u32;
        self.vocab.push(t.clone());
        self.ids.insert(t, id);
        self.df.push(0);
        id
    }

    fn push(&mut self, chunk: Chunk) {
        for &(t, _) in &chunk.terms {
            self.df[t as usize] += 1;
        }
        self.total_len += u64::from(chunk.len);
        self.chunks.push(chunk);
    }

    /// Appends a chunk (after the vector index added its entry)
    pub fn add(&mut self, entry: &Entry) {
        let tokens = chunk_tokens(entry);
        let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
        for t in &tokens {
            let id = self.term(t.clone());
            *counts.entry(id).or_default() += 1;
        }
        self.push(Chunk {
            doc: entry.doc_id.clone(),
            ord: entry.ordinal,
            len: tokens.len() as u32,
            terms: counts.into_iter().collect(),
        });
    }

    /// Removes every chunk of a document, keeping the others' order
    pub fn remove_doc(&mut self, doc_id: &str) {
        if !self.chunks.iter().any(|c| c.doc == doc_id) {
            return;
        }
        let chunks = core::mem::take(&mut self.chunks);
        for c in &chunks {
            if c.doc == doc_id {
                for &(t, _) in &c.terms {
                    self.df[t as usize] -= 1;
                }
                self.total_len -= u64::from(c.len);
            }
        }
        self.chunks = chunks.into_iter().filter(|c| c.doc != doc_id).collect();
    }

    /// Number of chunks
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// True without chunks
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Whether it holds the chunks of `entries`, in their order
    pub fn aligned_with(&self, entries: &[Entry]) -> bool {
        self.chunks.len() == entries.len()
            && self
                .chunks
                .iter()
                .zip(entries)
                .all(|(c, e)| c.doc == e.doc_id && c.ord == e.ordinal)
    }

    /// A token's id and inverse document frequency, if a chunk holds it
    fn idf(&self, token: &str) -> Option<(u32, f32)> {
        let &t = self.ids.get(token)?;
        let df = self.df[t as usize] as f32;
        let n = self.chunks.len() as f32;
        (df > 0.0).then(|| (t, (1.0 + (n - df + 0.5) / (df + 0.5)).ln()))
    }

    /// Adds one token's BM25 contribution to every chunk's score
    fn add_token(&self, token: &str, scores: &mut [f32]) {
        let Some((t, idf)) = self.idf(token) else {
            return;
        };
        let avg = (self.total_len as f32 / self.chunks.len() as f32).max(1.0);
        for (c, score) in self.chunks.iter().zip(scores.iter_mut()) {
            if let Ok(i) = c.terms.binary_search_by_key(&t, |&(id, _)| id) {
                let tf = c.terms[i].1 as f32;
                let norm = K1 * (1.0 - B + B * c.len as f32 / avg);
                *score += idf * tf * (K1 + 1.0) / (tf + norm);
            }
        }
    }

    /// BM25 score of every chunk, in order: the sum over the query's words,
    /// and for each glossary term it names, its best-matching name (see
    /// [`KeywordQuery`])
    pub fn scores(&self, query: &KeywordQuery) -> Vec<f32> {
        let mut out = alloc::vec![0.0; self.chunks.len()];
        for t in &query.words {
            self.add_token(t, &mut out);
        }
        for names in &query.terms {
            let mut best = alloc::vec![0.0f32; self.chunks.len()];
            for name in names {
                let mut s = alloc::vec![0.0; self.chunks.len()];
                for t in name {
                    self.add_token(t, &mut s);
                }
                let per_token = 1.0 / name.len() as f32;
                for (b, s) in best.iter_mut().zip(s) {
                    *b = b.max(s * per_token);
                }
            }
            for (o, b) in out.iter_mut().zip(best) {
                *o += b;
            }
        }
        out
    }

    /// Reads `keywords.json` from an index folder; `None` when it is
    /// missing, from another tokenizer version or does not read
    pub fn load(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(dir.join("keywords.json")).ok()?;
        let saved: Saved = match serde_json::from_slice(&bytes) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("the keyword index in {} does not read: {e}", dir.display());
                return None;
            }
        };
        if saved.tokenizer != TOKENIZER_VERSION
            || saved
                .chunks
                .iter()
                .flat_map(|c| &c.terms)
                .any(|&(t, _)| t as usize >= saved.vocab.len())
        {
            return None;
        }
        let mut index = KeywordIndex {
            ids: saved
                .vocab
                .iter()
                .enumerate()
                .map(|(i, t)| (t.clone(), i as u32))
                .collect(),
            df: alloc::vec![0; saved.vocab.len()],
            vocab: saved.vocab,
            ..KeywordIndex::default()
        };
        for c in saved.chunks {
            index.push(c);
        }
        Some(index)
    }

    /// Writes `keywords.json` into an index folder (replaced whole), with
    /// only the terms still used
    pub fn save(&self, dir: &Path) -> Result<()> {
        let mut map: BTreeMap<u32, u32> = BTreeMap::new();
        let mut vocab = Vec::new();
        for (t, &df) in self.df.iter().enumerate() {
            if df > 0 {
                map.insert(t as u32, vocab.len() as u32);
                vocab.push(self.vocab[t].clone());
            }
        }
        let chunks = self
            .chunks
            .iter()
            .map(|c| Chunk {
                terms: c.terms.iter().map(|&(t, n)| (map[&t], n)).collect(),
                ..c.clone()
            })
            .collect();
        let saved = Saved {
            tokenizer: TOKENIZER_VERSION,
            vocab,
            chunks,
        };
        crate::store::write_atomic(&dir.join("keywords.json"), &serde_json::to_vec(&saved)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::SourceKind;

    fn entry(doc: &str, ordinal: u32, title: &str, text: &str) -> Entry {
        Entry {
            doc_id: String::from(doc),
            ordinal,
            title: String::from(title),
            heading: String::new(),
            url: None,
            source: SourceKind::GameData,
            license: None,
            language: None,
            weight: 1.0,
            game: None,
            video: None,
            expert: None,
            text: String::from(text),
        }
    }

    fn query(text: &str) -> KeywordQuery {
        KeywordQuery::new(text, &Glossary::default())
    }

    #[test]
    fn a_term_counts_once_by_its_best_name() {
        let g = Glossary::seed();
        // The alias finds the term, whose names are matched as one
        let q = KeywordQuery::new("鬼坝 wave 3", &g);
        assert_eq!(q.words, ["3", "wave", "wave 3"]);
        assert_eq!(q.terms.len(), 1);
        assert!(q.terms[0].contains(&alloc::vec![
            String::from("spawning"),
            String::from("ground")
        ]));
        let entries = [
            entry("en", 0, "Stage", "Spawning Grounds"),
            entry("ja", 0, "Stage", "シェケナダム"),
            entry("both", 0, "Stage", "Spawning Grounds シェケナダム"),
            entry("other", 0, "Stage", "Sockeye Station"),
        ];
        let s = KeywordIndex::from_entries(&entries).scores(&q);
        // A chunk with two of its names scores as its better one, a little
        // less for being longer
        assert!(s[2] > 0.0 && s[2] < s[0].max(s[1]), "{s:?}");
        assert_eq!(s[3], 0.0);
    }

    fn has(tokens: &[String], t: &str) -> bool {
        tokens.iter().any(|x| x == t)
    }

    #[test]
    fn tokenizes_mixed_scripts_and_identifiers() {
        let t = tokens("What spawned in Wave 3 of Eggstra Work #7 at 333%?");
        for want in [
            "spawned", "wave", "3", "wave 3", "eggstra", "work", "#7", "333%",
        ] {
            assert!(has(&t, want), "{want} in {t:?}");
        }
        assert!(!has(&t, "what") && !has(&t, "of"));
        assert!(has(&tokens("Eggstra Work 7"), "#7"));
        assert!(has(&tokens("W3 was rough"), "wave 3"));
        assert!(has(&tokens("第3波出了什么"), "wave 3"));
        assert!(has(&tokens("第7次团队打工竞赛"), "#7"));
        assert!(has(&tokens("第12回のWAVE3"), "#12"));
        assert!(has(&tokens("第12回のWAVE3"), "wave 3"));
        let zh = tokens("鬼坝的满潮");
        for want in ["鬼坝", "坝的", "满潮"] {
            assert!(has(&zh, want), "{want} in {zh:?}");
        }
        assert_eq!(tokens("鲑"), ["鲑"]);
        let ja = tokens("キンシャケ 2.5倍");
        assert!(has(&ja, "キン") && has(&ja, "2.5"));
        assert_eq!(tokens("Steelheads"), ["steelhead"]);
        assert!(is_identifier("#7") && is_identifier("wave 3") && is_identifier("90%"));
        assert!(!is_identifier("wave"));
    }

    #[test]
    fn ranks_the_card_with_the_exact_numbers_first() {
        let entries = [
            entry("a", 0, "Eggstra Work #1, wave 3", "Stinger from B0"),
            entry("b", 0, "Eggstra Work #7, wave 1", "Stinger from A0"),
            entry("c", 0, "Eggstra Work #7, wave 3", "Scrapper from B1"),
            entry("d", 0, "Tides", "Low tide moves the basket down"),
        ];
        let index = KeywordIndex::from_entries(&entries);
        let s = index.scores(&query("wave 3 of Eggstra Work #7"));
        let best = (0..4).max_by(|&a, &b| s[a].total_cmp(&s[b])).unwrap();
        assert_eq!(entries[best].doc_id, "c");
        assert_eq!(s[3], 0.0);
    }

    #[test]
    fn follows_the_vector_index_and_round_trips() {
        let entries = alloc::vec![
            entry("a", 0, "Steelhead", "shoot the bomb"),
            entry("a", 1, "Steelhead", "from below"),
            entry("b", 0, "Tides", "low tide"),
        ];
        let mut index = KeywordIndex::from_entries(&entries);
        assert!(index.aligned_with(&entries));
        index.remove_doc("a");
        assert!(index.aligned_with(&entries[2..]));
        assert_eq!(index.scores(&query("bomb")), [0.0]);
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-keywords-{}", std::process::id()));
        index.save(&dir).unwrap();
        let loaded = KeywordIndex::load(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(loaded.aligned_with(&entries[2..]));
        assert_eq!(loaded.vocab, ["tide", "low"]);
        assert_eq!(loaded.scores(&query("low")), index.scores(&query("low")));
        assert!(KeywordIndex::load(Path::new("/nonexistent")).is_none());
    }
}
