//! Expert notes: the player's own corrections and explanations, Cuttlefish's
//! "memory".
//!
//! When an answer is wrong or shallow, the player edits it into the correct
//! explanation and saves it as a note. Each note is a Markdown file in
//! `<knowledge>/notes/<id>.md` with YAML front matter:
//!
//! ```markdown
//! ---
//! question: Which way does the Drizzler jump?
//! question_id: drizzler-jump
//! tags: [bosses, drizzler]
//! terms: [drizzler, egg-basket]
//! author: user
//! date: 2026-09-27
//! era: S3
//! version: 10.0.0
//! from: chat 2026-09-27_20-15-00
//! ---
//! It jumps away from the player who shot its umbrella...
//! ```
//!
//! A note is a document of source kind [`SourceKind::ExpertNote`], the most
//! trusted kind (the highest retrieval weight), titled by its question and
//! headed by its label ("Expert note (user), 2026-09-27"), which every chunk
//! carries as its heading, so prompts and citations show who wrote it and
//! when. The files are the truth: [`sync`] brings the store's documents in
//! line with them when the store opens, so a note edited by hand or synced
//! from another machine is indexed too.

use crate::doc::{Document, SourceKind};
use crate::embed::Embedder;
use crate::game::Game;
use crate::store::{Store, write_atomic};
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail, ensure};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The notes folder in the knowledge folder
pub const DIR: &str = "notes";

/// Longest slug in a note id
const SLUG_CHARS: usize = 48;

/// The author of notes written in the studio
pub const USER: &str = "user";

/// One expert note
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// File stem: `2026-09-27-drizzler-which-direction`
    pub id: String,
    /// The question it answers
    pub question: String,
    /// The explanation, Markdown
    pub body: String,
    /// Topics (`bosses`, `drizzler`)
    #[serde(default)]
    pub tags: Vec<String>,
    /// Glossary term ids it is about
    #[serde(default)]
    pub terms: Vec<String>,
    /// Who wrote it (`user`)
    pub author: String,
    /// The day it was written or last corrected
    pub date: NaiveDate,
    /// The game era it is about
    pub era: Game,
    /// The game version it was checked against, when known (`10.0.0`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The bank question it answers ([`crate::questions`]), when one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
    /// Where it came from: a chat's review, an eval file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The front matter: a note without its id and body
#[derive(Serialize, Deserialize)]
struct FrontMatter {
    question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    question_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    terms: Vec<String>,
    #[serde(default = "user")]
    author: String,
    date: NaiveDate,
    #[serde(default = "s3")]
    era: Game,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<String>,
}

fn user() -> String {
    String::from(USER)
}

fn s3() -> Game {
    Game::S3
}

impl Note {
    /// A note by the user, today, about Splatoon 3, without an id yet
    /// ([`new_id`] gives one)
    pub fn new(question: &str, body: &str) -> Self {
        Note {
            id: String::new(),
            question: String::from(question.trim()),
            body: String::from(body.trim()),
            tags: Vec::new(),
            terms: Vec::new(),
            author: user(),
            date: chrono::Local::now().date_naive(),
            era: Game::S3,
            version: None,
            question_id: None,
            from: None,
        }
    }

    /// How prompts and citations name it: `Expert note (user), 2026-09-27`
    pub fn label(&self) -> String {
        alloc::format!("Expert note ({}), {}", self.author, self.date)
    }

    /// The key of its document ([`crate::doc::doc_id`] of it)
    pub fn doc_key(&self) -> String {
        doc_key(&self.id)
    }

    /// The note as a document: source `expert-note`, the question as its
    /// title, the label as the heading over the body, linked to the
    /// studio's Notes panel
    pub fn document(&self) -> Document {
        let text = alloc::format!("# {}\n\n{}", self.label(), self.body.trim());
        let mut doc = Document::new(
            SourceKind::ExpertNote,
            &self.doc_key(),
            self.question.clone(),
            text,
        );
        doc.url = Some(alloc::format!("/cuttlefish/knowledge?note={}", self.id));
        doc.attribution = Some(self.author.clone());
        doc.game = Some(self.era);
        doc
    }

    /// The file's text: front matter, then the body
    pub fn to_markdown(&self) -> Result<String> {
        let front = FrontMatter {
            question: self.question.clone(),
            question_id: self.question_id.clone(),
            tags: self.tags.clone(),
            terms: self.terms.clone(),
            author: self.author.clone(),
            date: self.date,
            era: self.era,
            version: self.version.clone(),
            from: self.from.clone(),
        };
        let yaml = serde_yaml_ng::to_string(&front).context("writing the note's front matter")?;
        Ok(alloc::format!("---\n{}---\n{}\n", yaml, self.body.trim()))
    }

    /// A note from a file's text
    pub fn parse(id: &str, text: &str) -> Result<Self> {
        let text = text.trim_start_matches('\u{feff}');
        let rest = text
            .strip_prefix("---")
            .context("a note starts with a --- front matter")?;
        let rest = rest
            .strip_prefix('\n')
            .or_else(|| rest.strip_prefix("\r\n"));
        let rest = rest.context("a note starts with a --- front matter")?;
        let end = rest
            .find("\n---")
            .context("the note's front matter never ends")?;
        let front: FrontMatter =
            serde_yaml_ng::from_str(&rest[..end]).context("in the note's front matter")?;
        let body = rest[end + 4..].trim_start_matches(['-']).trim();
        ensure!(
            !front.question.trim().is_empty(),
            "the note has no question"
        );
        ensure!(!body.is_empty(), "the note has no body");
        Ok(Note {
            id: String::from(id),
            question: String::from(front.question.trim()),
            body: String::from(body),
            tags: front.tags,
            terms: front.terms,
            author: front.author,
            date: front.date,
            era: front.era,
            version: front.version,
            question_id: front.question_id,
            from: front.from,
        })
    }
}

/// The key of a note's document
pub fn doc_key(id: &str) -> String {
    alloc::format!("expert-note:{id}")
}

/// The notes folder
pub fn dir(root: &Path) -> PathBuf {
    root.join(DIR)
}

/// A note's file
pub fn path(root: &Path, id: &str) -> Result<PathBuf> {
    check_id(id)?;
    Ok(dir(root).join(alloc::format!("{id}.md")))
}

/// A note id is lowercase words, digits and dashes
pub fn check_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 80
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "not a note id: {id:?}"
    );
    Ok(())
}

/// A new id for a note: the date, then the question's ASCII words (else a
/// hash of it), unused in the folder
pub fn new_id(root: &Path, question: &str, date: NaiveDate) -> String {
    let mut slug = String::new();
    for word in question
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if slug.len() + word.len() + 1 > SLUG_CHARS {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }
    if slug.is_empty() {
        slug = crate::doc::doc_id(question)[..8].into();
    }
    let base = alloc::format!("{date}-{slug}");
    (1..)
        .map(|n| {
            if n == 1 {
                base.clone()
            } else {
                alloc::format!("{base}-{n}")
            }
        })
        .find(|id| !dir(root).join(alloc::format!("{id}.md")).exists())
        .unwrap()
}

/// Every note, newest first (by date, then id); a file that does not read
/// is skipped with a warning
pub fn list(root: &Path) -> Result<Vec<Note>> {
    let mut out = Vec::new();
    let dir = dir(root);
    if !dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&dir)? {
        let path = e?.path();
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|x| x != "md") || check_id(id).is_err() {
            continue;
        }
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|t| Note::parse(id, &t))
        {
            Ok(note) => out.push(note),
            Err(e) => log::warn!("skipping note {}: {e:#}", path.display()),
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then_with(|| b.id.cmp(&a.id)));
    Ok(out)
}

/// One note
pub fn load(root: &Path, id: &str) -> Result<Note> {
    let path = path(root, id)?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| alloc::format!("no note {id} ({})", path.display()))?;
    Note::parse(id, &text)
}

/// Writes a note (whole, through a temporary file); the note needs an id
pub fn save(root: &Path, note: &Note) -> Result<PathBuf> {
    ensure!(
        !note.question.trim().is_empty(),
        "the note needs a question"
    );
    ensure!(!note.body.trim().is_empty(), "the note needs a body");
    let path = path(root, &note.id)?;
    write_atomic(&path, note.to_markdown()?.as_bytes())?;
    Ok(path)
}

/// Removes a note's file; false if there was none
pub fn remove(root: &Path, id: &str) -> Result<bool> {
    let path = path(root, id)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => bail!("removing {}: {e}", path.display()),
    }
}

/// What [`sync`] did
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Notes embedded (new or changed)
    pub embedded: usize,
    /// Documents of notes whose file is gone
    pub removed: usize,
}

impl Synced {
    /// Whether the index changed, so it needs saving
    pub fn changed(self) -> bool {
        self.embedded + self.removed > 0
    }
}

/// Brings the store's note documents in line with the files: a note whose
/// document is missing or differs is embedded (added or replaced), a note
/// document whose file is gone is removed. Call [`Store::save`] when
/// [`Synced::changed`].
pub fn sync(store: &mut Store, embedder: &dyn Embedder) -> Result<Synced> {
    let mut done = Synced::default();
    let notes = list(store.root())?;
    let mut ids = BTreeSet::new();
    let indexed: BTreeSet<String> = store
        .index()
        .entries()
        .iter()
        .map(|e| e.doc_id.clone())
        .collect();
    for note in &notes {
        let doc = note.document();
        ids.insert(doc.id.clone());
        let same = store
            .document(&note.doc_key())
            .is_some_and(|old| old.text == doc.text && old.title == doc.title)
            && indexed.contains(&doc.id);
        if !same {
            store.add(&doc, embedder)?;
            done.embedded += 1;
        }
    }
    for old in store.documents()? {
        if old.source == SourceKind::ExpertNote && !ids.contains(&old.id) {
            store.delete(&old.id)?;
            done.removed += 1;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::HashEmbedder;
    use crate::index::VectorIndex;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-notes-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn notes_round_trip_through_markdown() {
        let mut note = Note::new(
            "  Which way does the Drizzler jump? ",
            "Away from whoever shot its umbrella.\n\n## Why\n\nIt flees.",
        );
        note.id = String::from("2026-09-27-drizzler");
        note.date = date("2026-09-27");
        note.tags = alloc::vec![String::from("bosses")];
        note.terms = alloc::vec![String::from("drizzler")];
        note.question_id = Some(String::from("drizzler-jump"));
        note.version = Some(String::from("10.0.0"));
        let text = note.to_markdown().unwrap();
        assert!(text.starts_with("---\nquestion: Which way does the Drizzler jump?\n"));
        assert!(text.contains("\ndate: 2026-09-27\n"));
        assert!(text.contains("\nera: S3\n"));
        assert!(
            text.ends_with("---\nAway from whoever shot its umbrella.\n\n## Why\n\nIt flees.\n")
        );
        let back = Note::parse("2026-09-27-drizzler", &text).unwrap();
        assert_eq!(back, note);
        assert_eq!(note.label(), "Expert note (user), 2026-09-27");
        // The document: the most trusted kind, titled by the question,
        // headed by the label
        let doc = note.document();
        assert_eq!(doc.source, SourceKind::ExpertNote);
        assert_eq!(doc.weight, 1.3);
        assert_eq!(doc.title, "Which way does the Drizzler jump?");
        assert!(
            doc.text
                .starts_with("# Expert note (user), 2026-09-27\n\nAway")
        );
        assert_eq!(
            doc.url.as_deref(),
            Some("/cuttlefish/knowledge?note=2026-09-27-drizzler")
        );
        assert_eq!(doc.game, Some(Game::S3));
        // Missing fields take their defaults
        let sparse = Note::parse("x", "---\nquestion: Q\ndate: 2026-01-02\n---\nBody\n").unwrap();
        assert_eq!((sparse.author.as_str(), sparse.era), ("user", Game::S3));
        assert!(Note::parse("x", "no front matter").is_err());
        assert!(Note::parse("x", "---\nquestion: Q\ndate: 2026-01-02\n---\n\n").is_err());
    }

    #[test]
    fn ids_come_from_the_date_and_the_question() {
        let root = temp("ids");
        let d = date("2026-09-27");
        assert_eq!(
            new_id(&root, "Why does the first kill drop an egg?", d),
            "2026-09-27-why-does-the-first-kill-drop-an-egg"
        );
        // Chinese: no ASCII words, so a hash
        let id = new_id(&root, "蝙蝠鱼往哪个方向跳？", d);
        assert_eq!(id.len(), "2026-09-27-".len() + 8, "{id}");
        // A long question is cut at a word
        let long = new_id(&root, &"word ".repeat(30), d);
        assert!(long.len() <= "2026-09-27-".len() + SLUG_CHARS);
        assert!(!long.ends_with('-'));
        // Taken ids get a number
        let mut note = Note::new("Why", "Because.");
        note.id = new_id(&root, "Why", d);
        save(&root, &note).unwrap();
        assert_eq!(new_id(&root, "Why", d), "2026-09-27-why-2");
        assert!(check_id("../x").is_err());
        assert!(path(&root, "A").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_are_the_truth_for_the_store() {
        let root = temp("sync");
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&root, &e).unwrap();
        let mut a = Note::new("Drizzler jump?", "Away from the shooter.");
        a.id = new_id(&root, &a.question, a.date);
        save(&root, &a).unwrap();
        let mut b = Note::new("Griller tail?", "Turn it with a jump.");
        b.id = new_id(&root, &b.question, b.date);
        save(&root, &b).unwrap();
        // A stray file is not a note
        std::fs::write(dir(&root).join("README.txt"), "hi").unwrap();
        let listed = list(&root).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(load(&root, &a.id).unwrap(), a);
        let done = sync(&mut store, &e).unwrap();
        assert_eq!(
            done,
            Synced {
                embedded: 2,
                removed: 0
            }
        );
        assert!(done.changed());
        assert_eq!(store.index().len(), 2);
        // Again: nothing to do
        assert_eq!(sync(&mut store, &e).unwrap(), Synced::default());
        // Edited by hand: re-embedded; removed: its document goes
        a.body = String::from("Away from the shooter, after its torpedo.");
        save(&root, &a).unwrap();
        assert!(remove(&root, &b.id).unwrap());
        assert!(!remove(&root, &b.id).unwrap());
        let done = sync(&mut store, &e).unwrap();
        assert_eq!(
            done,
            Synced {
                embedded: 1,
                removed: 1
            }
        );
        let docs = store.documents().unwrap();
        assert_eq!(docs.len(), 1);
        assert!(docs[0].text.contains("after its torpedo"));
        assert_eq!(store.index().len(), 1);
        // Found by its question, labelled by its heading
        let hits = store.search("Drizzler jump", 1, &e).unwrap();
        assert_eq!(hits[0].entry.source, SourceKind::ExpertNote);
        assert_eq!(hits[0].entry.heading, a.label());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
