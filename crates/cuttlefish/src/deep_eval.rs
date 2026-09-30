//! The deep eval: the bank's questions that need no video
//! ([`crate::questions::Bank::askable`]) asked through the configured
//! backend, a few at a time, the answers kept for review. It is a
//! benchmark the player runs when they choose, never on its own.
//!
//! Answers go to `<knowledge>/eval/deep-<date>.jsonl`, one [`Entry`] per
//! line with the sources the model cited and the backend and model that
//! answered; the file is rewritten after every batch, so a stopped run
//! keeps what it got. The lab's Knowledge view lists the files; the player
//! marks each answer good or wrong ([`mark`]) and turns a wrong one into an
//! expert note ([`crate::notes`]), whose id the entry then carries. That is
//! how the memory grows. A question can be asked again ([`ask_again`]) with
//! the store as it is then, the notes written since included; each answer
//! is kept beside the first in [`Entry::again`], with its own verdict.
//!
//! On the Claude CLI backend a question takes the agentic path
//! ([`review::ask_with_tools`]): the model looks the store up itself, and
//! the answer keeps what it looked up ([`Answered::lookups`]).

use crate::embed::Embedder;
use crate::llm::{AnsweredBy, Client};
use crate::questions::{Bank, Question};
use crate::review::{self, SourceRef};
use crate::store::{Store, write_atomic};
use crate::tools::{Library, Lookup, Session};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

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

/// One answer to a question, with the player's verdict on it
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answered {
    /// The model's answer; empty when it failed
    #[serde(default)]
    pub answer: String,
    /// The sources it cited
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    /// What the model looked up, on the agentic path
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lookups: Vec<Lookup>,
    /// Why there is no answer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub asked_at: DateTime<Utc>,
    /// How long the model took
    pub ms: u64,
    /// The backend and model that answered; absent in files written before
    /// they were recorded
    #[serde(flatten)]
    pub by: AnsweredBy,
    /// The player's verdict, once given
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

/// One question asked, with its answer, and the answers of each time it
/// was asked again
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// The question's id in the bank
    pub id: String,
    pub category: String,
    /// The language it was asked in (`en`, `zh`)
    pub lang: String,
    /// The question as asked
    pub question: String,
    /// The first answer
    #[serde(flatten)]
    pub answered: Answered,
    /// The expert note made from it, by id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The answers of each time it was asked again ([`ask_again`]), oldest
    /// first
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub again: Vec<Answered>,
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

/// What answers: the store (read-locked per lookup or question, never for
/// a whole run), its embedder, the model client and the knowledge tools'
/// caches
#[derive(Clone, Copy)]
pub struct Asker<'a> {
    pub store: &'a RwLock<Store>,
    pub embedder: &'a dyn Embedder,
    pub client: &'a Client,
    pub library: &'a Library,
}

/// Asks a question: on the agentic path when the client has the tools
/// ([`review::ask_with_tools`]), else with `k` excerpts
/// ([`review::ask`]); the answer, or why there is none
fn answer(asker: Asker<'_>, k: usize, question: &str) -> Answered {
    let started = std::time::Instant::now();
    let mut answered = Answered {
        answer: String::new(),
        sources: Vec::new(),
        lookups: Vec::new(),
        error: None,
        asked_at: Utc::now(),
        ms: 0,
        by: AnsweredBy {
            backend: Some(asker.client.backend()),
            model: Some(String::from(asker.client.model())),
            effort: Some(asker.client.settings.effort.clone()),
        },
        verdict: None,
    };
    let asked = if asker.client.has_tools() {
        let tools = Session::new(asker.store, asker.embedder, asker.library, 1);
        let asked = review::ask_with_tools(&tools, asker.client, question);
        // What it looked up is kept even when the answer failed
        answered.lookups = tools.lookups();
        asked
    } else {
        let store = asker.store.read().unwrap_or_else(PoisonError::into_inner);
        review::ask(&store, asker.embedder, asker.client, k, question)
    };
    match asked {
        Ok(answer) => {
            answered.answer = answer.text;
            answered.sources = answer.sources;
            answered.lookups = answer.lookups;
            answered.by = answer.by;
        }
        Err(e) => answered.error = Some(alloc::format!("{e:#}")),
    }
    answered.ms = started.elapsed().as_millis() as u64;
    answered
}

/// Asks one question of the bank
fn ask_one(asker: Asker<'_>, k: usize, q: &Question, lang: &str) -> Entry {
    let question = String::from(q.text(lang));
    Entry {
        id: q.id.clone(),
        category: q.category.clone(),
        lang: String::from(lang),
        answered: answer(asker, k, &question),
        question,
        note: None,
        again: Vec::new(),
    }
}

/// Asks an entry's question again, as it was asked, with `k` excerpts of
/// the store as it is now (the expert notes written since come first): the
/// answer to keep in [`Entry::again`] (see [`update`])
pub fn ask_again(asker: Asker<'_>, k: usize, entry: &Entry) -> Answered {
    answer(asker, k, &entry.question)
}

/// What retrieval gives every question of the bank in `lang`, the video
/// ones too (retrieval needs no video), without asking the model: the
/// excerpts in the order the model would get them (`cuttlefish eval deep
/// --dry-run`, to see what the answers stand on)
pub fn retrieval<'b>(
    store: &Store,
    embedder: &dyn Embedder,
    bank: &'b Bank,
    lang: &str,
    k: usize,
) -> Result<Vec<(&'b Question, Vec<crate::store::Hit>)>> {
    bank.questions
        .iter()
        .map(|q| Ok((q, review::retrieve(store, embedder, k, q.text(lang))?.0)))
        .collect()
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
            if entry.answered.error.is_some() {
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
            let first = |verdict| {
                entries
                    .iter()
                    .filter(|x| x.answered.verdict == Some(verdict))
                    .count()
            };
            Some(Listed {
                good: first(Verdict::Good),
                wrong: first(Verdict::Wrong),
                failed: entries
                    .iter()
                    .filter(|x| x.answered.error.is_some())
                    .count(),
                entries: entries.len(),
                file: name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.file.cmp(&a.file));
    out
}

/// One entry of an eval file, by question id
pub fn entry(root: &Path, file: &str, id: &str) -> Result<Entry> {
    check_file(file)?;
    read(&dir(root).join(file))?
        .into_iter()
        .find(|e| e.id == id)
        .with_context(|| alloc::format!("no question {id} in {file}"))
}

/// Changes one entry of an eval file with `change` and writes the file
/// back whole; answers with the entry as written. Reads and writes the
/// whole file, so callers that may overlap hold a lock around it.
pub fn update(
    root: &Path,
    file: &str,
    id: &str,
    change: impl FnOnce(&mut Entry) -> Result<()>,
) -> Result<Entry> {
    check_file(file)?;
    let path = dir(root).join(file);
    let mut entries = read(&path)?;
    let entry = entries
        .iter_mut()
        .find(|e| e.id == id)
        .with_context(|| alloc::format!("no question {id} in {file}"))?;
    change(entry)?;
    let changed = entry.clone();
    write(&path, &entries)?;
    Ok(changed)
}

/// Sets (or clears) the verdict on an answer of an entry, the first, or
/// with `again` the one asked again at that index; and the note made from
/// the entry, when given. Answers with the entry as written.
pub fn mark(
    root: &Path,
    file: &str,
    id: &str,
    verdict: Option<Verdict>,
    again: Option<usize>,
    note: Option<&str>,
) -> Result<Entry> {
    update(root, file, id, |entry| {
        let answered = match again {
            None => &mut entry.answered,
            Some(i) => entry
                .again
                .get_mut(i)
                .with_context(|| alloc::format!("{id} was not asked again {} times", i + 1))?,
        };
        answered.verdict = verdict;
        if let Some(note) = note {
            entry.note = Some(String::from(note));
        }
        Ok(())
    })
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
        // not retried); then one asked again
        let fake = Fake {
            replies: Mutex::new(alloc::vec![
                ok("Away from the shooter [S1]."),
                ok("Aggressive kills keep the basket fed."),
                (400, String::from("{\"error\": {\"message\": \"bad\"}}")),
                ok("The note says: away from the shooter [S1]."),
            ]),
            sent: sent.clone(),
        };
        let client = Client::with_transport(
            Box::new(fake),
            String::from("test-key"),
            Settings::default(),
        );
        let bank = Bank::seed();
        // Retrieval alone: every question of the bank, no model asked
        let found = retrieval(&store, &e, &bank, "en", 8).unwrap();
        assert_eq!(found.len(), bank.questions.len());
        assert!(found.iter().all(|(_, hits)| hits.len() == 1));
        assert!(sent.lock().unwrap().is_empty());
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
        let store = RwLock::new(store);
        let library = Library::new(&root, None);
        let asker = Asker {
            store: &store,
            embedder: &e,
            client: &client,
            library: &library,
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
        assert_eq!(entries[0].answered.sources.len(), 1);
        assert_eq!(entries[0].answered.sources[0].title, "Drizzlers");
        assert!(entries[2].answered.error.is_some());
        // Who answered, on every row, failed ones too
        let by = AnsweredBy {
            backend: Some(crate::llm::Backend::Api),
            model: Some(String::from(crate::llm::DEFAULT_MODEL)),
            effort: Some(String::from(crate::llm::DEFAULT_EFFORT)),
        };
        assert_eq!(entries[0].answered.by, by);
        assert_eq!(entries[2].answered.by, by);
        let line = std::fs::read_to_string(&out).unwrap();
        assert!(
            line.contains(
                "\"backend\":\"api\",\"model\":\"claude-opus-5-5\",\"effort\":\"medium\""
            )
        );
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
            None,
            Some("n-1"),
        )
        .unwrap();
        assert_eq!(marked.answered.verdict, Some(Verdict::Wrong));
        assert_eq!(marked.note.as_deref(), Some("n-1"));
        assert_eq!(list(&root)[0].wrong, 1);
        let cleared = mark(&root, &file, &entries[1].id, None, None, None).unwrap();
        assert_eq!(cleared.answered.verdict, None);
        assert_eq!(cleared.note.as_deref(), Some("n-1"));
        assert!(mark(&root, "../x.jsonl", "a", None, None, None).is_err());
        assert!(mark(&root, &file, "nope", None, None, None).is_err());
        // Asked again: the answer kept beside the first, with its own verdict
        assert!(mark(&root, &file, &entries[1].id, None, Some(0), None).is_err());
        let first = entry(&root, &file, &entries[1].id).unwrap();
        let again = ask_again(asker, 8, &first);
        assert_eq!(again.answer, "The note says: away from the shooter [S1].");
        assert_eq!(again.by, by);
        let kept = update(&root, &file, &first.id, |e| {
            e.again.push(again.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(kept.again, alloc::vec![again]);
        assert_eq!(kept.answered, first.answered);
        let good = mark(&root, &file, &first.id, Some(Verdict::Good), Some(0), None).unwrap();
        assert_eq!(good.again[0].verdict, Some(Verdict::Good));
        assert_eq!(good.answered.verdict, None);
        assert_eq!(read(&out).unwrap()[1], good);
        // A line written before answers were asked again or named their
        // model still reads
        let old = r#"{"id":"q","category":"macro","lang":"en","question":"Q?","answer":"A","sources":[],"asked_at":"2026-09-27T10:00:00Z","ms":5,"verdict":"good","note":"n"}"#;
        let old: Entry = serde_json::from_str(old).unwrap();
        assert_eq!(old.answered.verdict, Some(Verdict::Good));
        assert_eq!(old.answered.by, AnsweredBy::default());
        assert!(old.again.is_empty());
        assert!(!serde_json::to_string(&old).unwrap().contains("again"));
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
        assert_eq!(sent.lock().unwrap().len(), 4);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
