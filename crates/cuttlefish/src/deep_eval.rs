//! The deep eval: the bank's questions that need no video
//! ([`crate::questions::Bank::askable`]) asked through the configured
//! backend, a few at a time, the answers kept for review.
//!
//! Answers go to `<knowledge>/eval/deep-<date>.jsonl`, one [`Entry`] per
//! line with the sources the model was given and cited; the file is
//! rewritten after every batch, so a stopped run keeps what it got. The
//! lab's Knowledge view lists the files; the player marks each answer
//! good or wrong ([`mark`]) and turns a wrong one into an expert note
//! ([`crate::notes`]), whose id the entry then carries. That is how the
//! memory grows.

use crate::embed::Embedder;
use crate::llm::Client;
use crate::questions::{Bank, Question};
use crate::review::{self, SourceRef};
use crate::store::{Store, write_atomic};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The eval folder in the knowledge folder
pub const DIR: &str = "eval";

/// Questions asked at once by default
pub const DEFAULT_PARALLEL: usize = 3;

/// Questions asked at once at most (the Claude CLI runs eight at most)
pub const MAX_PARALLEL: usize = 8;

/// The player's verdict on an answer
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Good,
    Wrong,
}

/// One question asked, with its answer
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The question's id in the bank
    pub id: String,
    pub category: String,
    /// The language it was asked in (`en`, `zh`)
    pub lang: String,
    /// The question as asked
    pub question: String,
    /// The model's answer; empty when it failed
    #[serde(default)]
    pub answer: String,
    /// The sources it cited
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    /// Why there is no answer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub asked_at: DateTime<Utc>,
    /// How long the model took
    pub ms: u64,
    /// The player's verdict, once given
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// The expert note made from it, by id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What to ask
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    /// The language the questions are asked in (`en`, `zh`)
    pub lang: String,
    /// Questions asked at once
    pub parallel: usize,
    /// At most this many questions
    pub max: Option<usize>,
    /// Only these question ids, when given
    pub only: Vec<String>,
    /// Knowledge excerpts retrieved per question
    pub k: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            lang: String::from("en"),
            parallel: DEFAULT_PARALLEL,
            max: None,
            only: Vec::new(),
            k: 8,
        }
    }
}

/// What a run did
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub answered: usize,
    pub failed: usize,
    /// Told to stop before the end
    pub stopped: bool,
}

impl core::fmt::Display for Summary {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} answered, {} failed", self.answered, self.failed)?;
        if self.stopped {
            write!(f, ", stopped early")?;
        }
        Ok(())
    }
}

/// The eval folder
pub fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
}

/// A file name for a run now: `deep-<date>.jsonl`, with the time when the
/// day's file exists
pub fn new_file(root: &Path, now: DateTime<Utc>) -> PathBuf {
    let local = now.with_timezone(&chrono::Local);
    let day = dir(root).join(alloc::format!("deep-{}.jsonl", local.format("%Y-%m-%d")));
    if !day.exists() {
        return day;
    }
    dir(root).join(alloc::format!(
        "deep-{}.jsonl",
        local.format("%Y-%m-%d_%H-%M-%S")
    ))
}

/// A file name is `deep-....jsonl` in the eval folder, nothing else
pub fn check_file(name: &str) -> Result<()> {
    ensure!(
        name.starts_with("deep-")
            && name.ends_with(".jsonl")
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            && !name.contains(".."),
        "not an eval file: {name:?}"
    );
    Ok(())
}

/// The questions a run asks: the askable ones in the language, `only`
/// when given, at most `max`
pub fn pick<'a>(bank: &'a Bank, opts: &Options) -> Vec<&'a Question> {
    let mut out: Vec<&Question> = bank
        .askable()
        .filter(|q| opts.only.is_empty() || opts.only.contains(&q.id))
        .collect();
    if let Some(max) = opts.max {
        out.truncate(max);
    }
    out
}

/// What answers: the store, its embedder and the model client
#[derive(Clone, Copy)]
pub struct Asker<'a> {
    pub store: &'a Store,
    pub embedder: &'a dyn Embedder,
    pub client: &'a Client,
}

/// Asks one question
fn ask_one(asker: Asker<'_>, k: usize, q: &Question, lang: &str) -> Entry {
    let started = std::time::Instant::now();
    let asked_at = Utc::now();
    let question = String::from(q.text(lang));
    let mut entry = Entry {
        id: q.id.clone(),
        category: q.category.clone(),
        lang: String::from(lang),
        question,
        answer: String::new(),
        sources: Vec::new(),
        error: None,
        asked_at,
        ms: 0,
        verdict: None,
        note: None,
    };
    match review::ask(
        asker.store,
        asker.embedder,
        asker.client,
        k,
        &entry.question,
    ) {
        Ok(answer) => {
            entry.answer = answer.text;
            entry.sources = answer.sources;
        }
        Err(e) => entry.error = Some(alloc::format!("{e:#}")),
    }
    entry.ms = started.elapsed().as_millis() as u64;
    entry
}

/// Asks the picked questions, `parallel` at a time, writing `out` after
/// each batch; `report` hears each entry with how many are done of how
/// many; `cancelled` is asked between batches
pub fn run(
    asker: Asker<'_>,
    bank: &Bank,
    opts: &Options,
    out: &Path,
    cancelled: &dyn Fn() -> bool,
    report: &mut dyn FnMut(usize, usize, &Entry),
) -> Result<Summary> {
    let questions = pick(bank, opts);
    let parallel = opts.parallel.clamp(1, MAX_PARALLEL);
    let mut entries: Vec<Entry> = Vec::new();
    let mut summary = Summary::default();
    for batch in questions.chunks(parallel) {
        if cancelled() {
            summary.stopped = true;
            break;
        }
        let got: Vec<Entry> = std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .map(|q| scope.spawn(move || ask_one(asker, opts.k, q, &opts.lang)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("a question thread panicked"))
                .collect()
        });
        for entry in got {
            if entry.error.is_some() {
                summary.failed += 1;
            } else {
                summary.answered += 1;
            }
            entries.push(entry);
            report(entries.len(), questions.len(), entries.last().unwrap());
        }
        write(out, &entries)?;
    }
    Ok(summary)
}

/// Writes an eval file whole
pub fn write(path: &Path, entries: &[Entry]) -> Result<()> {
    let mut text = String::new();
    for e in entries {
        text.push_str(&serde_json::to_string(e)?);
        text.push('\n');
    }
    write_atomic(path, text.as_bytes())
}

/// Reads an eval file; lines that do not parse are skipped with a warning
pub fn read(path: &Path) -> Result<Vec<Entry>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| alloc::format!("reading {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| match serde_json::from_str::<Entry>(l) {
            Ok(e) => Some(e),
            Err(e) => {
                log::warn!("skipping a line of {}: {e}", path.display());
                None
            }
        })
        .collect())
}

/// An eval file as listed: its name, how many entries, how many marked
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Listed {
    pub file: String,
    pub entries: usize,
    pub good: usize,
    pub wrong: usize,
    pub failed: usize,
}

/// The eval files, newest first
pub fn list(root: &Path) -> Vec<Listed> {
    let Ok(dir) = std::fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut out: Vec<Listed> = dir
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            check_file(&name).ok()?;
            let entries = read(&e.path()).ok()?;
            Some(Listed {
                good: entries
                    .iter()
                    .filter(|x| x.verdict == Some(Verdict::Good))
                    .count(),
                wrong: entries
                    .iter()
                    .filter(|x| x.verdict == Some(Verdict::Wrong))
                    .count(),
                failed: entries.iter().filter(|x| x.error.is_some()).count(),
                entries: entries.len(),
                file: name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.file.cmp(&a.file));
    out
}

/// Sets (or clears) the verdict on an entry, and the note made from it;
/// answers with the entry as written
pub fn mark(
    root: &Path,
    file: &str,
    id: &str,
    verdict: Option<Verdict>,
    note: Option<&str>,
) -> Result<Entry> {
    check_file(file)?;
    let path = dir(root).join(file);
    let mut entries = read(&path)?;
    let entry = entries
        .iter_mut()
        .find(|e| e.id == id)
        .with_context(|| alloc::format!("no question {id} in {file}"))?;
    entry.verdict = verdict;
    if let Some(note) = note {
        entry.note = Some(String::from(note));
    }
    let marked = entry.clone();
    write(&path, &entries)?;
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Document, SourceKind};
    use crate::embed::HashEmbedder;
    use crate::llm::Settings;
    use crate::llm::tests::{Fake, ok};
    use std::sync::{Arc, Mutex};

    #[test]
    fn asks_writes_lists_and_marks() {
        let root = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-deep-eval-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&root, &e).unwrap();
        let doc = Document::new(
            SourceKind::Guide,
            "guide",
            String::from("Drizzlers"),
            String::from("# Drizzler\n\nIt jumps away from the shooter."),
        );
        store.add(&doc, &e).unwrap();
        let sent = Arc::new(Mutex::new(Vec::new()));
        // Three questions: two answers, one error from the API (a 400 is
        // not retried)
        let fake = Fake {
            replies: Mutex::new(alloc::vec![
                ok("Away from the shooter [S1]."),
                ok("Aggressive kills keep the basket fed."),
                (400, String::from("{\"error\": {\"message\": \"bad\"}}")),
            ]),
            sent: sent.clone(),
        };
        let client = Client::with_transport(
            Box::new(fake),
            String::from("test-key"),
            Settings::default(),
        );
        let bank = Bank::seed();
        let opts = Options {
            lang: String::from("zh"),
            parallel: 1,
            max: Some(3),
            ..Options::default()
        };
        assert_eq!(pick(&bank, &opts).len(), 3);
        let only = Options {
            only: alloc::vec![String::from("drizzler-jump")],
            ..Options::default()
        };
        assert_eq!(pick(&bank, &only)[0].id, "drizzler-jump");
        let out = new_file(&root, Utc::now());
        assert!(
            out.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("deep-")
        );
        let mut seen = Vec::new();
        let asker = Asker {
            store: &store,
            embedder: &e,
            client: &client,
        };
        let summary = run(
            asker,
            &bank,
            &opts,
            &out,
            &|| false,
            &mut |done, total, entry| seen.push((done, total, entry.id.clone())),
        )
        .unwrap();
        assert_eq!(
            summary,
            Summary {
                answered: 2,
                failed: 1,
                stopped: false
            }
        );
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[2].0, 3);
        assert_eq!(seen[2].1, 3);
        let entries = read(&out).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].lang, "zh");
        assert_eq!(entries[0].question, bank.questions[0].zh);
        assert_eq!(entries[0].sources.len(), 1);
        assert_eq!(entries[0].sources[0].title, "Drizzlers");
        assert!(entries[2].error.is_some());
        // The day's file exists now: the next run gets a timed name
        assert_ne!(new_file(&root, Utc::now()), out);
        let listed = list(&root);
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].entries, listed[0].failed), (3, 1));
        let file = listed[0].file.clone();
        let marked = mark(
            &root,
            &file,
            &entries[1].id,
            Some(Verdict::Wrong),
            Some("n-1"),
        )
        .unwrap();
        assert_eq!(marked.verdict, Some(Verdict::Wrong));
        assert_eq!(marked.note.as_deref(), Some("n-1"));
        assert_eq!(list(&root)[0].wrong, 1);
        let cleared = mark(&root, &file, &entries[1].id, None, None).unwrap();
        assert_eq!(cleared.verdict, None);
        assert_eq!(cleared.note.as_deref(), Some("n-1"));
        assert!(mark(&root, "../x.jsonl", "a", None, None).is_err());
        assert!(mark(&root, &file, "nope", None, None).is_err());
        // Stopped before the first batch: nothing asked
        let stopped = run(
            asker,
            &bank,
            &opts,
            &root.join("eval/deep-x.jsonl"),
            &|| true,
            &mut |_, _, _| {},
        )
        .unwrap();
        assert!(stopped.stopped);
        assert_eq!(sent.lock().unwrap().len(), 3);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
