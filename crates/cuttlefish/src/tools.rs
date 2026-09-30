//! The knowledge tools: what the model looks up itself while it answers,
//! offered to the Claude CLI as an MCP server ([`crate::mcp`]).
//!
//! Five read-only tools over the store ([`Session`], an
//! [`mcp::Tools`]):
//!
//! - `search`: hybrid retrieval ([`Store::search_where`]: E5 and BM25, the
//!   query expanded with the other-language names of the glossary terms it
//!   mentions) in any language, optionally only some source kinds or one
//!   game era; never a name table ([`Store::is_evidence`]), at most
//!   [`PER_DOCUMENT`] passages of a document; each result an id with its
//!   kind, era, place and a snippet
//! - `open`: a result whole, with the passages before and after it in its
//!   document, or a #vod-review message with the message it replies to and
//!   the replies to it
//! - `pedia`: a glossary term's entry as the Pedia shows it: official
//!   names in every language, slang, relations both ways, its game-data
//!   fact cards (numbers in players' units, [`crate::stats`]), the
//!   player's expert notes about it, the #vod-review comments that mention
//!   it ([`pedia::rank_quotes`]) and the examples the player recorded (the
//!   sessions' technique markers)
//! - `thread`: a #vod-review conversation of the corpus
//!   ([`crate::corpus`]) with the moments of its video
//! - `names`: the terms a name, nickname or sentence refers to, with
//!   their official names in English, Japanese and Chinese
//!
//! A [`Session`] serves one answer. The sources it shows get ids `S1`,
//! `S2`, ... in the order first shown, from the first id the conversation
//! has not used ([`crate::review::first_source_id`]), and the model cites
//! them as it cited the one-shot path's excerpts, so an answer's
//! citations resolve to [`SourceRef`]s ([`Session::cited`]). Every call
//! is kept as a [`Lookup`], shown under the answer as "what it looked up".
//! The store is read-locked for each call, never for a whole answer, so an
//! import or a note waits for a lookup, not for the model. What the tools
//! read besides the store (the corpus, the sessions' markers) is loaded
//! on first use and kept between answers in a [`Library`].

use crate::corpus::{self, Vod};
use crate::doc::SourceKind;
use crate::embed::Embedder;
use crate::expert::{self, ExpertComment};
use crate::game::Game;
use crate::glossary::{Glossary, Term};
use crate::index::Entry;
use crate::mcp;
use crate::notes;
use crate::pedia;
use crate::review::{PER_DOCUMENT, SourceRef};
use crate::store::{Hit, Store};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use core::fmt::Write as _;
use core::time::Duration;
use gameplay_data::session::{Marker, SessionInfo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Instant, SystemTime};

/// Tool calls an answer may make; later ones are told to answer with what
/// they have
pub const MAX_CALLS: usize = 40;
/// Results of a search unless asked for another number
pub const DEFAULT_RESULTS: usize = 8;
/// Results of a search at most
pub const MAX_RESULTS: usize = 20;
/// Characters of a search result's snippet
pub const SNIPPET_CHARS: usize = 280;
/// Passages shown before and after an opened one unless asked (at most 3)
pub const AROUND: u32 = 1;
/// #vod-review comments a Pedia entry quotes
pub const QUOTES: usize = 5;
/// Characters of a quote
const QUOTE_CHARS: usize = 360;
/// Fact cards a Pedia entry shows whole; a hazard level has dozens
pub const CARDS: usize = 4;
/// Messages of a thread shown at most
pub const THREAD_MESSAGES: usize = 80;
/// Characters of one message of a thread
pub const MESSAGE_CHARS: usize = 1500;
/// Terms `names` gives at most
pub const MAX_TERMS: usize = 8;
/// Recorded examples a Pedia entry lists
const EXAMPLES: usize = 5;
/// How long the sessions' markers are kept before they are read again
pub const MARKERS_FOR: Duration = Duration::from_secs(300);

/// One call of a tool, as the page lists it under the answer
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lookup {
    /// `search`, `open`, `pedia`, `thread` or `names`
    pub tool: String,
    /// Its arguments as the model gave them (`query`, `kinds`, `era`,
    /// `limit`; `id`; `term`; `text`)
    pub input: Value,
    /// What it showed: the ids of the sources with their titles, and for
    /// `pedia` and `names` the terms by id with their English names
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<Found>,
    /// Why it failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How long it took, in ms
    #[serde(default)]
    pub ms: u64,
}

/// A source or term a lookup showed
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Found {
    /// `S4`, or a term's id
    pub id: String,
    /// Where it stands (`Steelhead › Strategy`), or the term's name
    pub title: String,
}

/// The #vod-review corpus as the tools read it
struct Threads {
    /// The corpus file's size and time when read
    stamp: (u64, Option<SystemTime>),
    vods: Vec<Vod>,
    /// Each message's VOD and place in it, by its link
    by_url: BTreeMap<String, (usize, usize)>,
    /// Every expert comment ([`expert::comments`]), in VOD order
    comments: Vec<ExpertComment>,
    /// Each comment's VOD and its place among the VOD's comments (its
    /// chunk in the VOD's expert document)
    places: Vec<(usize, u32)>,
    /// The first comment of each message, by its link
    first_comment: BTreeMap<String, usize>,
}

/// A technique marker of a recorded session
struct Recorded {
    session: String,
    marker: Marker,
}

/// What the tools read besides the store, loaded on first use and kept
/// between answers: the #vod-review corpus (read again when its file
/// changes) and the markers of the recorded sessions (read again after
/// [`MARKERS_FOR`])
pub struct Library {
    root: PathBuf,
    sessions: Option<PathBuf>,
    threads: Mutex<Option<Arc<Threads>>>,
    markers: Mutex<Option<(Instant, Arc<Vec<Recorded>>)>>,
}

/// A file's size and time
fn stamp_of(path: &Path) -> (u64, Option<SystemTime>) {
    std::fs::metadata(path).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

impl Library {
    /// For the knowledge folder `root`, with the recorded sessions under
    /// `sessions` (the lab's `[inspect] root`), when known
    pub fn new(root: &Path, sessions: Option<PathBuf>) -> Self {
        Library {
            root: root.to_path_buf(),
            sessions,
            threads: Mutex::default(),
            markers: Mutex::default(),
        }
    }

    /// The corpus, read again when its file changed; empty before
    /// `cuttlefish corpus build`
    fn threads(&self) -> Result<Arc<Threads>> {
        let file = corpus::path(&self.root);
        let stamp = stamp_of(&file);
        let mut kept = self.threads.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = &*kept
            && t.stamp == stamp
        {
            return Ok(Arc::clone(t));
        }
        let vods = if stamp.1.is_none() {
            Vec::new()
        } else {
            corpus::read(&self.root)?.vods
        };
        let mut threads = Threads {
            stamp,
            by_url: BTreeMap::new(),
            comments: Vec::new(),
            places: Vec::new(),
            first_comment: BTreeMap::new(),
            vods: Vec::new(),
        };
        for (v, vod) in vods.iter().enumerate() {
            for (m, message) in vod.messages.iter().enumerate() {
                threads.by_url.insert(message.url.clone(), (v, m));
            }
            for (i, c) in expert::comments(vod).into_iter().enumerate() {
                threads
                    .first_comment
                    .entry(c.url.clone())
                    .or_insert(threads.comments.len());
                threads.places.push((v, i as u32));
                threads.comments.push(c);
            }
        }
        threads.vods = vods;
        let threads = Arc::new(threads);
        *kept = Some(Arc::clone(&threads));
        Ok(threads)
    }

    /// Every technique marker of the sessions, newest first
    fn recorded(&self) -> Arc<Vec<Recorded>> {
        let mut kept = self.markers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, markers)) = &*kept
            && at.elapsed() < MARKERS_FOR
        {
            return Arc::clone(markers);
        }
        let mut found: Vec<Recorded> = Vec::new();
        let dirs = self
            .sessions
            .as_deref()
            .and_then(|d| std::fs::read_dir(d).ok());
        for e in dirs.into_iter().flatten().flatten() {
            let Ok(info) = SessionInfo::read(&e.path()) else {
                continue;
            };
            let session = e.file_name().to_string_lossy().into_owned();
            found.extend(info.markers.into_iter().map(|marker| Recorded {
                session: session.clone(),
                marker,
            }));
        }
        found.sort_by_key(|r| core::cmp::Reverse(r.marker.t_start_ms));
        let found = Arc::new(found);
        *kept = Some((Instant::now(), Arc::clone(&found)));
        found
    }
}

/// What a session has shown and done so far
#[derive(Default)]
struct State {
    /// The number of the first id, `S<first>`
    first: usize,
    /// The sources shown, in order: the one at `i` is `S<first + i>`
    shown: Vec<SourceRef>,
    lookups: Vec<Lookup>,
}

/// Whether a source is a message (a #vod-review comment, a thread's
/// message), known by its link, rather than a passage of a document
fn message_like(r: &SourceRef) -> bool {
    r.expert.is_some() || r.doc.is_none()
}

/// Whether two sources are the same: messages by their link (the pieces
/// of a long comment are one source), passages by document and position
fn same_source(a: &SourceRef, b: &SourceRef) -> bool {
    match (message_like(a), message_like(b)) {
        (true, true) => a.url.is_some() && a.url == b.url,
        (false, false) => a.doc == b.doc && a.ordinal == b.ordinal,
        _ => false,
    }
}

impl State {
    /// The id of a source, given when it is first shown
    fn cite(&mut self, mut source: SourceRef) -> String {
        if let Some(i) = self.shown.iter().position(|s| same_source(s, &source)) {
            return self.shown[i].id.clone();
        }
        source.id = alloc::format!("S{}", self.first + self.shown.len());
        let id = source.id.clone();
        self.shown.push(source);
        id
    }

    /// A source shown, by its id (`S4`, `[S4]`, `s4`)
    fn find(&self, id: &str) -> Option<&SourceRef> {
        let id = id.trim().trim_matches(['[', ']']);
        self.shown.iter().find(|s| s.id.eq_ignore_ascii_case(id))
    }
}

/// The knowledge tools for one answer (see the module docs)
pub struct Session<'a> {
    store: &'a RwLock<Store>,
    embedder: &'a dyn Embedder,
    library: &'a Library,
    state: Mutex<State>,
}

/// A text argument, trimmed and not empty
fn text_arg<'v>(args: &'v Value, key: &str) -> Result<&'v str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .with_context(|| alloc::format!("give {key}"))
}

/// A source kind as written in configs, prompts and results
pub fn kind_name(kind: SourceKind) -> String {
    kind.to_possible_value()
        .map_or_else(String::new, |v| String::from(v.get_name()))
}

/// Every source kind's name
fn kind_names() -> Vec<String> {
    SourceKind::value_variants()
        .iter()
        .map(|k| kind_name(*k))
        .collect()
}

/// The `kinds` argument
fn kinds_arg(args: &Value) -> Result<Vec<SourceKind>> {
    let Some(list) = args["kinds"].as_array() else {
        return Ok(Vec::new());
    };
    list.iter()
        .filter_map(Value::as_str)
        .map(|k| {
            SourceKind::from_str(k.trim(), true).map_err(|_| {
                anyhow!(
                    "no source kind {k:?}; the kinds are {}",
                    kind_names().join(", ")
                )
            })
        })
        .collect()
}

/// The `era` argument: `S3` or `S2`
fn era_arg(args: &Value) -> Result<Option<Game>> {
    match args["era"].as_str().map(str::trim) {
        None | Some("") => Ok(None),
        Some(e) if e.eq_ignore_ascii_case("s3") => Ok(Some(Game::S3)),
        Some(e) if e.eq_ignore_ascii_case("s2") => Ok(Some(Game::S2)),
        Some(e) => bail!("no era {e:?}: S3 or S2"),
    }
}

/// Where a passage stands: an expert comment or a note by its label, then
/// its document's title; else the title and the section
fn place(title: &str, heading: &str, labelled: bool) -> String {
    match (heading.is_empty(), labelled) {
        (true, _) => String::from(title),
        (false, true) => alloc::format!("{heading} — {title}"),
        (false, false) => alloc::format!("{title} › {heading}"),
    }
}

/// A source's place ([`place`])
fn source_place(s: &SourceRef) -> String {
    let labelled = s.expert.is_some() || s.source == SourceKind::ExpertNote;
    place(&s.title, &s.heading, labelled)
}

/// ` · Splatoon 2 era` for the older game
fn era_note(game: Option<Game>) -> &'static str {
    match game {
        Some(Game::S2) => " · Splatoon 2 era",
        _ => "",
    }
}

/// A line of text, whitespace collapsed, at most `max` characters
fn clip(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let mut out: String = one.chars().take(max).collect();
    out.push('…');
    out
}

/// The words a snippet centres on: the query's Latin words of three
/// letters or more, and every name of the glossary terms it mentions
fn query_words(query: &str, glossary: &Glossary) -> Vec<String> {
    let mut words: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.is_ascii() && w.len() >= 3)
        .map(String::from)
        .collect();
    for t in glossary.find_in(query) {
        words.extend(t.names().map(String::from));
    }
    words
}

/// A term's main names in English, Japanese and Chinese: `en Steelhead |
/// ja バクダン | zh 炸弹鱼`
fn main_names(t: &Term) -> String {
    ["en", "ja", "zh"]
        .iter()
        .filter_map(|l| Some(alloc::format!("{l} {}", t.forms.get(*l)?.join(", "))))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// A term's approved slang, `zh 绿帽怪 (note)`, joined
fn slang(t: &Term) -> String {
    t.approved()
        .map(|a| {
            if a.note.is_empty() {
                alloc::format!("{} {}", a.lang, a.text)
            } else {
                alloc::format!("{} {} ({})", a.lang, a.text, a.note)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The term a name stands for: its id or a name, else the first a loose
/// search or the text's mentions give
fn term_named<'g>(g: &'g Glossary, name: &str) -> Option<&'g Term> {
    g.lookup(name)
        .or_else(|| g.search(name, 1).into_iter().next())
        .or_else(|| g.find_in(name).into_iter().next())
}

/// A VOD's title as the store has it
fn vod_title(vod: &Vod) -> String {
    alloc::format!("#vod-review: {}'s VOD, {}", vod.poster, vod.date)
}

/// `W2 :50 at 1:23 of the video`: where the moments of a message are
fn moments_text(m: &corpus::ReviewMessage) -> String {
    let clock = |s: f32| {
        let s = s.max(0.0).round() as u32;
        alloc::format!("{}:{:02}", s / 60, s % 60)
    };
    m.moments
        .iter()
        .map(|x| match x.t_s {
            Some(t) if x.aligned => alloc::format!("{} (at {} of the video)", x.raw, clock(t)),
            _ => x.raw.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl<'a> Session<'a> {
    /// A session over the store, whose ids start at `S<first>` (1 for a
    /// new conversation)
    pub fn new(
        store: &'a RwLock<Store>,
        embedder: &'a dyn Embedder,
        library: &'a Library,
        first: usize,
    ) -> Self {
        Session {
            store,
            embedder,
            library,
            state: Mutex::new(State {
                first: first.max(1),
                ..State::default()
            }),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read_store(&self) -> std::sync::RwLockReadGuard<'_, Store> {
        self.store.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// Something of the store, read under its lock (the system prompt's
    /// names and digest)
    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        f(&self.read_store())
    }

    /// Every source shown so far, in order
    pub fn shown(&self) -> Vec<SourceRef> {
        self.state().shown.clone()
    }

    /// Every call so far
    pub fn lookups(&self) -> Vec<Lookup> {
        self.state().lookups.clone()
    }

    /// The sources shown that `text` cites as `[S4]`, in the order shown
    pub fn cited(&self, text: &str) -> Vec<SourceRef> {
        self.state()
            .shown
            .iter()
            .filter(|s| text.contains(&alloc::format!("[{}]", s.id)))
            .cloned()
            .collect()
    }

    /// `search`
    fn search(&self, state: &mut State, args: &Value) -> Result<(String, Vec<Found>)> {
        let query = text_arg(args, "query")?;
        let kinds = kinds_arg(args)?;
        let era = era_arg(args)?;
        let limit = args["limit"]
            .as_u64()
            .map_or(DEFAULT_RESULTS, |n| n as usize)
            .clamp(1, MAX_RESULTS);
        let store = self.read_store();
        let keep = |e: &Entry| {
            store.is_evidence(e)
                && (kinds.is_empty() || kinds.contains(&e.source))
                && match era {
                    None => true,
                    Some(Game::S2) => e.game == Some(Game::S2),
                    Some(Game::S3) => e.game != Some(Game::S2),
                }
        };
        let hits = store.search_where(query, limit * 4, self.embedder, &keep)?;
        let mut per_document: BTreeMap<&str, usize> = BTreeMap::new();
        let hits: Vec<&Hit> = hits
            .iter()
            .filter(|h| {
                let n = per_document.entry(h.entry.doc_id.as_str()).or_default();
                *n += 1;
                *n <= PER_DOCUMENT
            })
            .take(limit)
            .collect();
        let mut filters: Vec<String> = kinds.iter().map(|k| kind_name(*k)).collect();
        if let Some(e) = era {
            filters.push(String::from(match e {
                Game::S2 => "Splatoon 2 era only",
                Game::S3 => "without the Splatoon 2 era",
            }));
        }
        let filters = if filters.is_empty() {
            String::new()
        } else {
            alloc::format!(" ({})", filters.join(", "))
        };
        if hits.is_empty() {
            return Ok((
                alloc::format!(
                    "Nothing found for \"{query}\"{filters}. Try other words, the other language, \
                     or fewer filters."
                ),
                Vec::new(),
            ));
        }
        let words = query_words(query, store.glossary());
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut out = alloc::format!(
            "{} result{} for \"{query}\"{filters}:\n",
            hits.len(),
            if hits.len() == 1 { "" } else { "s" }
        );
        let mut found = Vec::new();
        for h in hits {
            let e = &h.entry;
            let source = SourceRef::of(e, String::new());
            let where_ = source_place(&source);
            let id = state.cite(source);
            let _ = writeln!(
                out,
                "[{id}] {} · {where_}{}\n  {}",
                kind_name(e.source),
                era_note(e.game),
                pedia::snippet(&e.text, &words, SNIPPET_CHARS)
            );
            found.push(Found { id, title: where_ });
        }
        out.push_str("Open an id to read it whole before you cite it.");
        Ok((out, found))
    }

    /// `open`
    fn open(&self, state: &mut State, args: &Value) -> Result<(String, Vec<Found>)> {
        let id = text_arg(args, "id")?;
        let around = args["around"].as_u64().map_or(AROUND, |n| n.min(3) as u32);
        let shown = state.find(id).cloned();
        // A passage by its document and position: a source shown, or a
        // document id (`<16 hex digits>` or `<id>#<position>`)
        let passage = match &shown {
            Some(s) => s.doc.clone().zip(s.ordinal),
            None => {
                let (doc, at) = id.split_once('#').unwrap_or((id, "0"));
                crate::store::is_id(doc)
                    .then(|| at.parse().ok().map(|n| (String::from(doc), n)))
                    .flatten()
            }
        };
        if let Some((doc, ordinal)) = passage
            && let Some(text) = self.open_passage(state, &doc, ordinal, around)?
        {
            return Ok(text);
        }
        let url = shown
            .as_ref()
            .and_then(|s| s.url.clone())
            .unwrap_or_else(|| String::from(id));
        let threads = self.library.threads()?;
        if let Some(&(v, m)) = threads.by_url.get(&url) {
            return Ok(self.open_message(state, &threads, v, m));
        }
        match shown {
            Some(s) => bail!("{} ({}) is no longer in the store", s.id, source_place(&s)),
            None => bail!("no id {id} in this answer: give an id a tool showed, such as S1"),
        }
    }

    /// A passage of a document with `around` passages before and after
    /// it; `None` when the index lacks it
    fn open_passage(
        &self,
        state: &mut State,
        doc: &str,
        ordinal: u32,
        around: u32,
    ) -> Result<Option<(String, Vec<Found>)>> {
        let store = self.read_store();
        let mut entries: Vec<&Entry> = store
            .index()
            .entries()
            .iter()
            .filter(|e| e.doc_id == doc && store.is_evidence(e))
            .collect();
        entries.sort_by_key(|e| e.ordinal);
        let Some(at) = entries.iter().position(|e| e.ordinal == ordinal) else {
            return Ok(None);
        };
        let from = at.saturating_sub(around as usize);
        let to = (at + around as usize + 1).min(entries.len());
        let mut out = String::new();
        let mut found = Vec::new();
        for (i, e) in entries[from..to].iter().enumerate() {
            let source = SourceRef::of(e, String::new());
            let where_ = source_place(&source);
            let id = state.cite(source);
            let lead = match (from + i).cmp(&at) {
                core::cmp::Ordering::Less => "Before it, ",
                core::cmp::Ordering::Equal => "",
                core::cmp::Ordering::Greater => "After it, ",
            };
            let _ = write!(
                out,
                "{lead}[{id}] {} · {where_}{}",
                kind_name(e.source),
                era_note(e.game)
            );
            if from + i == at {
                let mut about: Vec<String> = Vec::new();
                if let Some(u) = &e.url {
                    about.push(u.clone());
                }
                if let Some(v) = e.video.as_ref().filter(|v| Some(*v) != e.url.as_ref()) {
                    about.push(alloc::format!("about the video {v}"));
                }
                if let Some(l) = &e.license {
                    about.push(l.clone());
                }
                if !about.is_empty() {
                    let _ = write!(out, "\n({})", about.join("; "));
                }
            }
            let _ = write!(out, "\n{}\n\n", e.text.trim());
            if from + i == at {
                found.push(Found { id, title: where_ });
            }
        }
        if entries[at].source == SourceKind::DiscordVodReview && entries[at].expert.is_some() {
            out.push_str("thread gives the whole conversation this comment is part of.");
        }
        Ok(Some((String::from(out.trim_end()), found)))
    }

    /// A source for a message of a #vod-review conversation
    fn message_source(threads: &Threads, v: usize, m: usize) -> SourceRef {
        let vod = &threads.vods[v];
        let message = &vod.messages[m];
        let expert = threads
            .first_comment
            .get(&message.url)
            .map(|&i| threads.comments[i].expert.clone());
        SourceRef {
            id: String::new(),
            title: vod_title(vod),
            heading: expert.as_ref().map_or_else(
                || alloc::format!("{}, {}", message.author, message.time.date_naive()),
                expert::Expert::label,
            ),
            url: Some(message.url.clone()),
            source: SourceKind::DiscordVodReview,
            license: Some(String::from(crate::discord::LICENSE)),
            expert,
            doc: None,
            ordinal: None,
        }
    }

    /// A #vod-review message whole, with the message it replies to and the
    /// replies to it
    fn open_message(
        &self,
        state: &mut State,
        threads: &Threads,
        v: usize,
        m: usize,
    ) -> (String, Vec<Found>) {
        let vod = &threads.vods[v];
        let message = &vod.messages[m];
        let line = |state: &mut State, i: usize| {
            let x = &vod.messages[i];
            let id = state.cite(Self::message_source(threads, v, i));
            alloc::format!(
                "[{id}] {}, {}: {}",
                x.author,
                x.time.format("%Y-%m-%d %H:%M"),
                clip(&x.text, MESSAGE_CHARS)
            )
        };
        let mut out = alloc::format!(
            "{}{} ({} messages; thread gives them all)\n",
            vod_title(vod),
            era_note(Some(vod.game)),
            vod.messages.len()
        );
        if let Some(i) = message
            .reply_to
            .as_ref()
            .and_then(|r| vod.messages.iter().position(|x| &x.id == r))
        {
            let _ = writeln!(out, "Replying to {}", line(state, i));
        }
        let head = line(state, m);
        let _ = writeln!(out, "{head}");
        let moments = moments_text(message);
        if !moments.is_empty() {
            let _ = writeln!(out, "  moments: {moments}");
        }
        for (i, x) in vod.messages.iter().enumerate() {
            if x.reply_to.as_deref() == Some(message.id.as_str()) {
                let _ = writeln!(out, "Reply: {}", line(state, i));
            }
        }
        let source = Self::message_source(threads, v, m);
        let found = alloc::vec![Found {
            id: state.cite(source.clone()),
            title: source_place(&source),
        }];
        (String::from(out.trim_end()), found)
    }

    /// `pedia`
    fn pedia(&self, state: &mut State, args: &Value) -> Result<(String, Vec<Found>)> {
        let name = text_arg(args, "term")?;
        // Read before the store's lock is taken: the first time, the corpus
        // file and the sessions' files are read
        let threads = self
            .library
            .threads()
            .map_err(|e| log::warn!("knowledge tools: no #vod-review corpus: {e:#}"))
            .ok();
        let recorded = self
            .library
            .sessions
            .is_some()
            .then(|| self.library.recorded());
        let store = self.read_store();
        let g = store.glossary();
        let t = term_named(g, name).with_context(|| {
            alloc::format!("no term named {name:?}; names finds terms in a sentence or by slang")
        })?;
        let english = pedia::english(t);
        let mut found = alloc::vec![Found {
            id: t.id.clone(),
            title: String::from(english),
        }];
        let mut out = alloc::format!("{english} (term {}", t.id);
        if let Some(kind) = &t.kind {
            let _ = write!(out, ", {kind}");
        }
        if let Some(game) = &t.game {
            let _ = write!(out, ", names from {game}");
        }
        out.push_str(")\n");
        let _ = writeln!(out, "Official names: {}", main_names(t));
        let others: Vec<String> = t
            .forms
            .iter()
            .filter(|(l, _)| !["en", "ja", "zh"].contains(&l.as_str()))
            .map(|(l, f)| alloc::format!("{l} {}", f.join(", ")))
            .collect();
        if !others.is_empty() {
            let _ = writeln!(out, "In other languages: {}", others.join(" | "));
        }
        let said = slang(t);
        if !said.is_empty() {
            let _ = writeln!(out, "Slang players use: {said}");
        }
        if !t.definition.is_empty() {
            let _ = writeln!(out, "Definition: {}", t.definition);
        }
        let mut related: Vec<String> = t.related.iter().map(|r| r.label()).collect();
        for o in &g.terms {
            if let Some(r) = o.related.as_ref().filter(|r| r.term == t.id) {
                related.push(alloc::format!(
                    "{} is {} it",
                    pedia::english(o),
                    r.kind.as_str()
                ));
            }
        }
        if !related.is_empty() {
            let _ = writeln!(out, "Related: {}", related.join("; "));
        }
        // Game data: the cards whose subject is this term
        let cards: Vec<&Entry> = store
            .index()
            .entries()
            .iter()
            .filter(|e| e.source == SourceKind::GameData && e.ordinal == 0)
            .filter(|e| card_is_about(g, e, t))
            .collect();
        if !cards.is_empty() {
            let _ = writeln!(
                out,
                "\nGame data ({} fact card{} from Lean's datamine; exact for the version named):",
                cards.len(),
                if cards.len() == 1 { "" } else { "s" }
            );
            for e in cards.iter().take(CARDS) {
                let source = SourceRef::of(e, String::new());
                let where_ = source_place(&source);
                let id = state.cite(source);
                let more = store
                    .index()
                    .entries()
                    .iter()
                    .filter(|x| x.doc_id == e.doc_id)
                    .count()
                    - 1;
                let _ = writeln!(out, "[{id}] {}\n{}", e.title, e.text.trim());
                if more > 0 {
                    let _ = writeln!(
                        out,
                        "({more} more passage{} of raw parameters: open {id} with around {})",
                        if more == 1 { "" } else { "s" },
                        more.min(3)
                    );
                }
                found.push(Found { id, title: where_ });
            }
            for e in cards.iter().skip(CARDS) {
                let source = SourceRef::of(e, String::new());
                let id = state.cite(source);
                let _ = writeln!(out, "[{id}] {} (open it to read it)", e.title);
            }
        }
        // The player's notes about it
        let notes: Vec<notes::Note> = notes::list(store.root())
            .unwrap_or_default()
            .into_iter()
            .filter(|n| pedia::note_is_about(n, t))
            .collect();
        if !notes.is_empty() {
            out.push_str("\nThe player's expert notes about it (the most trusted):\n");
            for n in &notes {
                let doc = n.document();
                let source = SourceRef {
                    id: String::new(),
                    title: n.question.clone(),
                    heading: n.label(),
                    url: doc.url.clone(),
                    source: SourceKind::ExpertNote,
                    license: None,
                    expert: None,
                    doc: Some(doc.id.clone()),
                    ordinal: Some(0),
                };
                let where_ = source_place(&source);
                let id = state.cite(source);
                let _ = writeln!(out, "[{id}] {where_}\n{}", n.body.trim());
                found.push(Found { id, title: where_ });
            }
        }
        // What #vod-review says of it
        let comments: &[ExpertComment] = threads.as_ref().map_or(&[], |x| &x.comments);
        let ids = BTreeSet::from([t.id.as_str()]);
        let mentions = pedia::mentions(g, &ids, comments);
        let mentioned: &[usize] = mentions.get(&t.id).map_or(&[], Vec::as_slice);
        if let Some(threads) = threads.as_ref().filter(|_| !mentioned.is_empty()) {
            let names: Vec<&str> = t.names().collect();
            let _ = writeln!(
                out,
                "\n#vod-review: {} messages mention it; the best comments:",
                pedia::messages(mentioned, &threads.comments)
            );
            let best =
                pedia::rank_quotes(mentioned, &threads.comments, &|c| c.expert.t_s.is_some());
            for i in best.into_iter().take(QUOTES) {
                let c = &threads.comments[i];
                let (v, ordinal) = threads.places[i];
                let vod = &threads.vods[v];
                let source = SourceRef {
                    id: String::new(),
                    title: vod_title(vod),
                    heading: c.expert.label(),
                    url: Some(c.url.clone()),
                    source: SourceKind::DiscordVodReview,
                    license: Some(String::from(crate::discord::LICENSE)),
                    expert: Some(c.expert.clone()),
                    doc: Some(crate::doc::doc_id(&expert::doc_key(vod))),
                    ordinal: Some(ordinal),
                };
                let where_ = source_place(&source);
                let id = state.cite(source);
                let _ = writeln!(
                    out,
                    "[{id}] {}: \"{}\"",
                    c.expert.label(),
                    pedia::snippet(&c.text, &names, QUOTE_CHARS)
                );
                found.push(Found { id, title: where_ });
            }
        }
        // What the player recorded of it
        if let Some(recorded) = recorded {
            let examples: Vec<&Recorded> = recorded
                .iter()
                .filter(|r| match &r.marker.term {
                    Some(term) => *term == t.id,
                    None => t.has_name(&r.marker.label),
                })
                .collect();
            if !examples.is_empty() {
                let _ = writeln!(
                    out,
                    "\nThe player recorded {} example{} of it (technique markers of their sessions):",
                    examples.len(),
                    if examples.len() == 1 { "" } else { "s" }
                );
                for r in examples.iter().take(EXAMPLES) {
                    let m = &r.marker;
                    let _ = writeln!(
                        out,
                        "- {}: {}, {:.1} s",
                        r.session,
                        m.label,
                        m.t_end_ms.saturating_sub(m.t_start_ms) as f64 / 1000.0
                    );
                }
            }
        }
        Ok((String::from(out.trim_end()), found))
    }

    /// `thread`
    fn thread(&self, state: &mut State, args: &Value) -> Result<(String, Vec<Found>)> {
        let id = text_arg(args, "id")?;
        let threads = self.library.threads()?;
        ensure_corpus(&threads)?;
        let shown = state.find(id).cloned();
        let key = shown
            .as_ref()
            .and_then(|s| s.url.clone())
            .unwrap_or_else(|| String::from(id));
        // A message of it, its first message's link, its id, or the video
        // a conversation chunk is about
        let video = shown.as_ref().and_then(|s| s.doc.as_ref()).and_then(|doc| {
            let store = self.read_store();
            store
                .index()
                .entries()
                .iter()
                .find(|e| &e.doc_id == doc)
                .and_then(|e| e.video.clone())
        });
        let v = threads
            .by_url
            .get(&key)
            .map(|&(v, _)| v)
            .or_else(|| {
                threads
                    .vods
                    .iter()
                    .position(|x| x.url == key || x.id == key)
            })
            .or_else(|| {
                let video = video.as_ref()?;
                threads.vods.iter().position(|x| &x.video.url == video)
            })
            .with_context(|| {
                alloc::format!(
                    "no #vod-review conversation for {id}: give the id of a #vod-review result, a \
                     Discord message link or a VOD id"
                )
            })?;
        let focus = threads.by_url.get(&key).map(|&(_, m)| m);
        let vod = &threads.vods[v];
        let mut out = vod_title(vod);
        let _ = write!(out, " ({}", vod.game.name());
        if let Some(t) = &vod.thread {
            let _ = write!(out, "; thread \"{t}\"");
        }
        if let Some(n) = vod.eggstra_event {
            let _ = write!(out, "; probably Eggstra Work event #{n}");
        }
        let _ = writeln!(
            out,
            "); video {}; posted by {}; {} messages",
            vod.video.url,
            vod.poster,
            vod.messages.len()
        );
        let mut found = Vec::new();
        for (m, message) in vod.messages.iter().enumerate().take(THREAD_MESSAGES) {
            let source = Self::message_source(&threads, v, m);
            let id = state.cite(source.clone());
            let mut who = message.author.clone();
            if message.author == vod.poster {
                who.push_str(" (the poster)");
            }
            if let Some(to) = &message.reply_author {
                let _ = write!(who, ", replying to {to}");
            }
            let mark = if Some(m) == focus { " ←" } else { "" };
            let _ = writeln!(
                out,
                "[{id}]{mark} {} {who}: {}",
                message.time.format("%Y-%m-%d %H:%M"),
                clip(&message.text, MESSAGE_CHARS)
            );
            let moments = moments_text(message);
            if !moments.is_empty() {
                let _ = writeln!(out, "  moments: {moments}");
            }
            if Some(m) == focus || (m == 0 && focus.is_none()) {
                found.push(Found {
                    id,
                    title: source_place(&source),
                });
            }
        }
        if vod.messages.len() > THREAD_MESSAGES {
            let _ = writeln!(
                out,
                "({} more messages not shown)",
                vod.messages.len() - THREAD_MESSAGES
            );
        }
        Ok((String::from(out.trim_end()), found))
    }

    /// `names`
    fn names(&self, _state: &mut State, args: &Value) -> Result<(String, Vec<Found>)> {
        let text = text_arg(args, "text")?;
        let store = self.read_store();
        let g = store.glossary();
        let exact = g.lookup(text);
        let mut terms: Vec<&Term> = match exact {
            Some(t) => alloc::vec![t],
            None => g.find_in(text),
        };
        if terms.is_empty() {
            terms = g.search(text, MAX_TERMS);
        }
        terms.truncate(MAX_TERMS);
        let partial = if exact.is_none() {
            g.partial_in(text)
        } else {
            Vec::new()
        };
        if terms.is_empty() && partial.is_empty() {
            return Ok((
                alloc::format!(
                    "No glossary term for \"{text}\". It may be slang the glossary lacks: say you \
                     are not sure what it means."
                ),
                Vec::new(),
            ));
        }
        let mut out = String::new();
        let mut found = Vec::new();
        for t in &terms {
            let _ = write!(out, "- {} (term {}", pedia::english(t), t.id);
            if let Some(kind) = &t.kind {
                let _ = write!(out, ", {kind}");
            }
            let _ = write!(out, "): {}", main_names(t));
            let said = slang(t);
            if !said.is_empty() {
                let _ = write!(out, "; slang: {said}");
            }
            if let Some(r) = &t.related {
                let _ = write!(out, "; {}", r.label());
            }
            if !t.definition.is_empty() {
                let _ = write!(out, "; {}", clip(&t.definition, 200));
            }
            out.push('\n');
            found.push(Found {
                id: t.id.clone(),
                title: String::from(pedia::english(t)),
            });
        }
        for (short, full, t) in &partial {
            let _ = writeln!(
                out,
                "- \"{short}\" may be short for {full} ({}, term {}), or an ordinary word",
                pedia::english(t),
                t.id
            );
        }
        out.push_str("pedia gives a term's whole entry.");
        Ok((out, found))
    }
}

/// An error when the corpus is empty
fn ensure_corpus(threads: &Threads) -> Result<()> {
    if threads.vods.is_empty() {
        bail!("the #vod-review corpus is not built (cuttlefish corpus build)");
    }
    Ok(())
}

/// Whether a fact card (a game-data passage at position 0) is about `t`:
/// its subject (the title before ` (`) names it, else, when the subject is
/// no term, the first term its title names besides Salmon Run itself, as
/// the Pedia files the cards ([`pedia::fact_cards`])
fn card_is_about(g: &Glossary, card: &Entry, t: &Term) -> bool {
    if !pedia::names_term(&card.title, t) && !t.has_name(&card.title) {
        return false;
    }
    let subject = card.title.split(" (").next().unwrap_or(&card.title);
    if t.has_name(subject) {
        return true;
    }
    g.lookup(subject).is_none()
        && g.find_in(&card.title)
            .into_iter()
            .find(|x| x.id != "salmon-run")
            .is_some_and(|x| x.id == t.id)
}

/// The tools' descriptions and input schemas, as `tools/list` gives them
pub fn definitions() -> Vec<Value> {
    let read_only = json!({"readOnlyHint": true, "openWorldHint": false});
    alloc::vec![
        json!({
            "name": "search",
            "description": "Search Cuttlefish's Salmon Run knowledge store by meaning and keywords: \
                the player's expert notes, #vod-review expert comments, game-data fact cards \
                (Lean's datamine), Inkipedia, Discord, RedNote and X. Keywords match only their \
                own language, so search in English and in Chinese, with official names. Results \
                are ids with a snippet; open an id to read it whole before you cite it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to look for, in any language"},
                    "kinds": {
                        "type": "array",
                        "items": {"type": "string", "enum": kind_names()},
                        "description": "Only these kinds of source (expert-note: the player's notes; \
                            discord-vod-review: #vod-review; game-data: fact cards; wiki: Inkipedia)"
                    },
                    "era": {
                        "type": "string",
                        "enum": ["S3", "S2"],
                        "description": "S3 leaves out material about Splatoon 2; S2 keeps only that"
                    },
                    "limit": {
                        "type": "integer", "minimum": 1, "maximum": MAX_RESULTS,
                        "description": "How many results (8 unless given)"
                    }
                },
                "required": ["query"]
            },
            "annotations": read_only,
        }),
        json!({
            "name": "open",
            "description": "Read a result whole: an id that search, pedia or thread gave (S4), \
                with the passages just before and after it in its document (each with an id of \
                its own); a #vod-review message comes with the message it replies to and its \
                replies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The id, such as S4"},
                    "around": {
                        "type": "integer", "minimum": 0, "maximum": 3,
                        "description": "Passages before and after it (1 unless given)"
                    }
                },
                "required": ["id"]
            },
            "annotations": read_only,
        }),
        json!({
            "name": "pedia",
            "description": "A term's entry in the Overfishing Pedia (the glossary): its official \
                names in English, Japanese, Chinese and other languages, the slang players use, \
                related terms, its game-data fact cards with exact numbers in players' units \
                (damage, HP, hits to defeat, frames at 60 per second), the player's expert notes \
                about it, #vod-review comments that mention it, and the examples the player \
                recorded. Give a name in any language, a nickname or a term id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "term": {"type": "string", "description": "A name, nickname or term id"}
                },
                "required": ["term"]
            },
            "annotations": read_only,
        }),
        json!({
            "name": "thread",
            "description": "A whole #vod-review conversation: the VOD (poster, date, era, video) \
                and its messages in order, each with an id and the moments it points at (wave \
                timer, or the time in the video). Give the id of a #vod-review result or quote \
                (S7), a Discord message link or a VOD id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "An id (S7), a message link or a VOD id"}
                },
                "required": ["id"]
            },
            "annotations": read_only,
        }),
        json!({
            "name": "names",
            "description": "Resolve names: the glossary terms a name, nickname, abbreviation or \
                sentence refers to, in any language, with their official names in English, \
                Japanese and Chinese and the slang players use for them. Use it to turn a \
                player's words into official names before you search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "A name or a sentence"}
                },
                "required": ["text"]
            },
            "annotations": read_only,
        }),
    ]
}

impl mcp::Tools for Session<'_> {
    fn list(&self) -> Vec<Value> {
        definitions()
    }

    fn call(&self, name: &str, arguments: &Value) -> (String, bool) {
        let started = Instant::now();
        let mut state = self.state();
        if state.lookups.len() >= MAX_CALLS {
            return (
                alloc::format!(
                    "That makes {MAX_CALLS} lookups for this answer: answer now with what you found."
                ),
                true,
            );
        }
        let result = match name {
            "search" => self.search(&mut state, arguments),
            "open" => self.open(&mut state, arguments),
            "pedia" => self.pedia(&mut state, arguments),
            "thread" => self.thread(&mut state, arguments),
            "names" => self.names(&mut state, arguments),
            other => Err(anyhow!("no tool {other}")),
        };
        let ms = started.elapsed().as_millis() as u64;
        let (text, found, error) = match result {
            Ok((text, found)) => (text, found, None),
            Err(e) => {
                let e = alloc::format!("{e:#}");
                (e.clone(), Vec::new(), Some(e))
            }
        };
        log::debug!(
            "knowledge tool {name} {arguments} in {ms} ms: {} shown{}",
            found.len(),
            error
                .as_ref()
                .map(|e| alloc::format!(", {e}"))
                .unwrap_or_default()
        );
        let failed = error.is_some();
        state.lookups.push(Lookup {
            tool: name.to_string(),
            input: arguments.clone(),
            found,
            error,
            ms,
        });
        (text, failed)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::doc::Document;
    use crate::embed::HashEmbedder;
    use crate::mcp::Tools;

    /// A scratch store: a wiki page of three sections, a game-data card, a
    /// Splatoon 2 era page, an expert note, a name table page, and the
    /// corpus of `corpus::tests::fixture` with its expert comments indexed
    pub(crate) fn scratch(name: &str) -> (PathBuf, RwLock<Store>, HashEmbedder) {
        let root = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-tools-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let corpus_root = crate::corpus::tests::fixture(&alloc::format!("tools-{name}"));
        std::fs::create_dir_all(&root).unwrap();
        let built = crate::corpus::build(&corpus_root).unwrap();
        crate::corpus::write(&root, &built).unwrap();
        std::fs::remove_dir_all(&corpus_root).unwrap();
        let e = HashEmbedder { dim: 128 };
        let mut store = Store::open(&root, &e).unwrap();
        // Sections long enough to be chunks of their own
        let section = |heading: &str, sentence: &str| {
            alloc::format!("# {heading}\n\n{}\n\n", sentence.repeat(12).trim())
        };
        let mut wiki = Document::new(
            SourceKind::Wiki,
            "https://splatoonwiki.org/wiki/Steelhead",
            String::from("Steelhead"),
            section(
                "Behavior",
                "The Steelhead grows a bomb on its head and throws it at a player. ",
            ) + &section(
                "Strategy",
                "Shoot the bomb on its head to splat the Steelhead at once, early. ",
            ) + &section(
                "Names in other languages",
                "Japanese: Bakudan. Chinese: zhadanyu. Spanish: Salmonborg bomb. ",
            ),
        );
        wiki.license = Some(String::from("CC BY-NC-SA 3.0"));
        wiki.url = Some(String::from("https://splatoonwiki.org/wiki/Steelhead"));
        store.add(&wiki, &e).unwrap();
        let mut card = Document::new(
            SourceKind::GameData,
            "leanny:enemy/SakelienBomber",
            String::from("Steelhead (Salmonid, game data)"),
            String::from("# Steelhead (Salmonid)\n\nThe bomb explodes 180 frames after the throw."),
        );
        card.game = Some(Game::S3);
        store.add(&card, &e).unwrap();
        let mut old = Document::new(
            SourceKind::Wiki,
            "https://example.org/s2-steelhead",
            String::from("Steelhead in Splatoon 2"),
            String::from("# Bomb\n\nIn Splatoon 2 the Steelhead bomb was slower."),
        );
        old.game = Some(Game::S2);
        store.add(&old, &e).unwrap();
        let mut note = notes::Note::new(
            "When do I shoot the Steelhead bomb?",
            "Shoot the Steelhead bomb as it grows, before the throw.",
        );
        note.id = String::from("2026-09-27-steelhead-bomb");
        note.terms = alloc::vec![String::from("steelhead")];
        notes::save(&root, &note).unwrap();
        store.add(&note.document(), &e).unwrap();
        crate::expert::index(&mut store, &e, &built, &mut |_| {}).unwrap();
        store.save().unwrap();
        (root, RwLock::new(store), e)
    }

    fn call(session: &Session, name: &str, args: Value) -> String {
        let (text, failed) = session.call(name, &args);
        assert!(!failed, "{name} {args} failed: {text}");
        text
    }

    #[test]
    fn searches_with_ids_filters_and_no_name_tables() {
        let (root, store, e) = scratch("search");
        let library = Library::new(&root, None);
        let session = Session::new(&store, &e, &library, 1);
        let text = call(&session, "search", json!({"query": "Steelhead bomb"}));
        assert!(text.contains("results for \"Steelhead bomb\":"), "{text}");
        assert!(text.contains("[S1] "), "{text}");
        assert!(!text.contains("Names in other languages"), "{text}");
        assert!(!text.contains("Bakudan"), "{text}");
        assert!(text.contains(" · Splatoon 2 era"), "{text}");
        assert!(text.ends_with("Open an id to read it whole before you cite it."));
        // The same passage keeps its id; the ids go on
        let again = call(
            &session,
            "search",
            json!({"query": "Steelhead bomb", "limit": 2}),
        );
        let first_line = |t: &str| String::from(t.lines().nth(1).unwrap());
        assert_eq!(first_line(&text), first_line(&again));
        // Kinds and eras filter
        let cards = call(
            &session,
            "search",
            json!({"query": "Steelhead bomb", "kinds": ["game-data"]}),
        );
        assert!(cards.contains("1 result for"), "{cards}");
        assert!(cards.contains("game-data · Steelhead (Salmonid, game data)"));
        assert!(cards.contains("(game-data)"));
        let s3 = call(
            &session,
            "search",
            json!({"query": "Steelhead bomb", "era": "S3"}),
        );
        assert!(s3.contains("(without the Splatoon 2 era)"), "{s3}");
        assert!(!s3.contains(" · Splatoon 2 era"), "{s3}");
        let s2 = call(
            &session,
            "search",
            json!({"query": "Steelhead bomb", "era": "s2"}),
        );
        assert!(
            s2.contains("1 result for") && s2.contains(" · Splatoon 2 era"),
            "{s2}"
        );
        // Unknown kinds, eras and tools fail with what is known
        let (bad, failed) = session.call("search", &json!({"query": "x", "kinds": ["blogs"]}));
        assert!(failed && bad.contains("expert-note"), "{bad}");
        let (bad, failed) = session.call("search", &json!({"query": "x", "era": "S1"}));
        assert!(failed && bad.contains("S3 or S2"));
        let (bad, failed) = session.call("fly", &json!({}));
        assert!(failed && bad == "no tool fly");
        let (bad, failed) = session.call("search", &json!({}));
        assert!(failed && bad == "give query");
        // Every call is kept, the failed ones with their error
        let lookups = session.lookups();
        assert_eq!(lookups.len(), 9);
        assert_eq!(lookups[0].tool, "search");
        assert_eq!(lookups[0].input["query"], "Steelhead bomb");
        assert_eq!(lookups[0].found[0].id, "S1");
        assert_eq!(lookups[5].error.as_deref(), Some(bad_kind_error().as_str()));
        assert!(lookups[8].found.is_empty());
        // The citations of a text resolve to the sources shown
        let cited = session.cited("It explodes late [S2], see [S1] and [S99].");
        assert_eq!(cited.len(), 2);
        assert_eq!(cited[0].id, "S1");
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn bad_kind_error() -> String {
        alloc::format!(
            "no source kind \"blogs\"; the kinds are {}",
            kind_names().join(", ")
        )
    }

    #[test]
    fn ids_start_after_the_conversations() {
        let (root, store, e) = scratch("first");
        let library = Library::new(&root, None);
        let session = Session::new(&store, &e, &library, 7);
        let text = call(
            &session,
            "search",
            json!({"query": "Steelhead", "limit": 1}),
        );
        assert!(text.contains("[S7] "), "{text}");
        assert_eq!(session.shown()[0].id, "S7");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn opens_passages_with_their_neighbours() {
        let (root, store, e) = scratch("open");
        let library = Library::new(&root, None);
        let session = Session::new(&store, &e, &library, 1);
        call(
            &session,
            "search",
            json!({"query": "shoot the bomb on its head", "kinds": ["wiki"], "era": "S3"}),
        );
        let strategy = session
            .shown()
            .into_iter()
            .find(|s| s.heading == "Strategy")
            .unwrap();
        let text = call(&session, "open", json!({"id": strategy.id}));
        assert!(text.contains("Shoot the bomb on its head"), "{text}");
        assert!(text.contains("Before it, [S"), "{text}");
        assert!(text.contains("grows a bomb"), "{text}");
        assert!(text.contains("https://splatoonwiki.org/wiki/Steelhead"));
        assert!(text.contains("CC BY-NC-SA 3.0"));
        // The name table after it is never shown
        assert!(!text.contains("After it"), "{text}");
        assert!(!text.contains("Bakudan"));
        // Without neighbours; by a document id; unknown ids
        let alone = call(&session, "open", json!({"id": strategy.id, "around": 0}));
        assert!(!alone.contains("Before it"));
        let doc = strategy.doc.clone().unwrap();
        let by_doc = call(
            &session,
            "open",
            json!({"id": alloc::format!("{doc}#1"), "around": 0}),
        );
        assert!(
            by_doc.starts_with(&alloc::format!("[{}]", strategy.id)),
            "{by_doc}"
        );
        let (text, failed) = session.call("open", &json!({"id": "S99"}));
        assert!(failed && text.contains("no id S99"), "{text}");
        let lookups = session.lookups();
        assert_eq!(lookups[1].found[0].id, strategy.id);
        assert_eq!(lookups[1].found[0].title, "Steelhead › Strategy");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn opens_threads_and_their_messages() {
        let (root, store, e) = scratch("thread");
        let library = Library::new(&root, None);
        let session = Session::new(&store, &e, &library, 1);
        // An expert comment found by search, then its whole conversation
        let text = call(
            &session,
            "search",
            json!({"query": "basket", "kinds": ["discord-vod-review"]}),
        );
        assert!(text.contains("discord-vod-review · "), "{text}");
        let comment = session
            .shown()
            .into_iter()
            .find(|s| s.expert.is_some())
            .unwrap();
        let thread = call(&session, "thread", json!({"id": comment.id}));
        assert!(thread.starts_with("#vod-review: "), "{thread}");
        assert!(thread.contains("(the poster)"), "{thread}");
        assert!(thread.contains(" ←"), "{thread}");
        // The comment keeps its id in the thread
        assert!(
            thread.contains(&alloc::format!("[{}] ←", comment.id)),
            "{thread}"
        );
        // A message opens with its neighbours in the conversation
        let poster = session
            .shown()
            .into_iter()
            .find(|s| s.expert.is_none() && s.doc.is_none())
            .unwrap();
        let opened = call(&session, "open", json!({"id": poster.id}));
        assert!(opened.contains("thread gives them all"), "{opened}");
        let (text, failed) = session.call("thread", &json!({"id": "nothing-like-it"}));
        assert!(
            failed && text.contains("no #vod-review conversation"),
            "{text}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn gives_pedia_entries_and_names() {
        let (root, store, e) = scratch("pedia");
        // A recorded session with a marker of the term
        let sessions = root.join("sessions");
        let session_dir = sessions.join("2026-09-28_10-00-00");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("session.json"),
            json!({
                "controller": {"file": "controller.bin"},
                "video": {"segments": []},
                "markers": [{"kind": "technique", "label": "Steelhead", "term": "steelhead",
                             "t_start_ms": 1000, "t_end_ms": 3500, "created_ms": 4000}]
            })
            .to_string(),
        )
        .unwrap();
        let library = Library::new(&root, Some(sessions));
        let session = Session::new(&store, &e, &library, 1);
        let text = call(&session, "pedia", json!({"term": "炸弹鱼"}));
        assert!(text.starts_with("Steelhead (term steelhead"), "{text}");
        assert!(
            text.contains("Official names: en Steelhead | ja バクダン | zh 炸弹鱼"),
            "{text}"
        );
        assert!(
            text.contains("Game data (1 fact card from Lean's datamine"),
            "{text}"
        );
        assert!(text.contains("The bomb explodes 180 frames after the throw."));
        assert!(
            text.contains("The player's expert notes about it"),
            "{text}"
        );
        assert!(text.contains("Shoot the Steelhead bomb as it grows"));
        assert!(
            text.contains("The player recorded 1 example of it"),
            "{text}"
        );
        assert!(
            text.contains("- 2026-09-28_10-00-00: Steelhead, 2.5 s"),
            "{text}"
        );
        let found = &session.lookups()[0].found;
        assert_eq!(found[0].id, "steelhead");
        assert_eq!(found[0].title, "Steelhead");
        assert!(found.iter().any(|f| f.title.contains("Expert note (user)")));
        let (text, failed) = session.call("pedia", &json!({"term": "xyzzy"}));
        assert!(failed && text.contains("no term named"));
        // Names: an alias, a sentence, nothing
        let names = call(&session, "names", json!({"text": "Steelhead"}));
        assert!(
            names.starts_with("- Steelhead (term steelhead): en Steelhead"),
            "{names}"
        );
        let sentence = call(
            &session,
            "names",
            json!({"text": "kill the Steelhead at low tide"}),
        );
        assert!(
            sentence.contains("term steelhead") && sentence.contains("term low-tide"),
            "{sentence}"
        );
        let none = call(&session, "names", json!({"text": "qwertyuiop"}));
        assert!(none.starts_with("No glossary term"), "{none}");
        assert_eq!(session.lookups()[2].found[0].id, "steelhead");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stops_after_the_last_lookup() {
        let (root, store, e) = scratch("limit");
        let library = Library::new(&root, None);
        let session = Session::new(&store, &e, &library, 1);
        for _ in 0..MAX_CALLS {
            session.call("names", &json!({"text": "Steelhead"}));
        }
        let (text, failed) = session.call("names", &json!({"text": "Steelhead"}));
        assert!(failed && text.contains("answer now"), "{text}");
        assert_eq!(session.lookups().len(), MAX_CALLS);
        // Serialized for the page and the eval files
        let v = serde_json::to_value(&session.lookups()[0]).unwrap();
        assert_eq!(v["tool"], "names");
        assert_eq!(v["found"][0]["id"], "steelhead");
        assert!(v.get("error").is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lists_the_tools() {
        let names: Vec<String> = definitions()
            .iter()
            .map(|t| String::from(t["name"].as_str().unwrap()))
            .collect();
        assert_eq!(names, ["search", "open", "pedia", "thread", "names"]);
        for t in definitions() {
            assert_eq!(t["inputSchema"]["type"], "object");
            assert_eq!(t["annotations"]["readOnlyHint"], true);
        }
        let kinds = &definitions()[0]["inputSchema"]["properties"]["kinds"]["items"]["enum"];
        assert!(
            kinds
                .as_array()
                .unwrap()
                .contains(&json!("discord-vod-review"))
        );
        assert_eq!(kind_name(SourceKind::GameData), "game-data");
    }
}
