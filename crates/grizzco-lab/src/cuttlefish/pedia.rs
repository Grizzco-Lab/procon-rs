//! Overfishing Pedia, Cuttlefish's fourth view (`/cuttlefish/pedia`): the
//! glossary as an encyclopedia of Salmon Run (`cuttlefish::pedia`), with
//! what #vod-review says about each term.
//!
//! The glossary is read from the knowledge folder on every request (as the
//! Translate view does), so edits through the slang endpoints show at
//! once. The mentions in the corpus's expert comments are searched once
//! and kept until the corpus file or the names of the terms in scope
//! change ([`Wild`]). The knowledge and reviews folders may be a network
//! mount that other work loads heavily, so nothing a request answers with
//! reads many files from them: the fact cards of game data (every stored
//! document) are read on a thread when the store's index file changed,
//! and the entry says `cards_pending` until they are there ([`Cards`]);
//! the reviews of #vod-review VODs are listed once a minute
//! ([`REVIEWED_FOR`]), not looked up per quoted comment.
//!
//! Endpoints under `/api/cuttlefish/` (see [`crate::cuttlefish`]):
//!
//! - `GET pedia`: every term in scope, with its section, kind, games,
//!   facets (official, community, user), how many #vod-review comments
//!   mention it, its main names per language, every name and slang (for
//!   searching in any language) and its definition; the sections with
//!   their counts; the corpus's size
//! - `GET pedia/<term id>?quotes=`: one entry: the term (official names,
//!   aliases with their note, source and, for those of the user file, the
//!   alias's id), the user file's own record of a new term, the relations
//!   both ways, a stat.ink icon from the asset catalogue, fact cards
//!   (`cards_pending` while a thread still reads them: ask again), the
//!   top `quotes` comments that mention it (default [`QUOTES`], each with
//!   reviewer, date, era, the Discord link and, when the VOD is a review
//!   here with the moment placed, the review to open at `t_s`), the
//!   expert notes about it and the deep questions that name it
//! - `GET source?url=&doc=&ordinal=&title=&heading=`: the context of a
//!   source an answer cites or a quote comes from, for the page's source
//!   popover ([`Pedia::source`]): a #vod-review message with the message it
//!   replies to, the replies to it, the VOD and its placed moments (with
//!   the review to open at each); else the chunk of a document (`doc` and
//!   `ordinal`, as `cuttlefish::review::SourceRef` has them, else the
//!   document of `url` and the chunk under `heading`) with its title,
//!   licence and credit

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use anyhow::{Context, Result};
use cuttlefish::assets::Catalogue;
use cuttlefish::chunk::{ChunkConfig, chunk_text};
use cuttlefish::corpus::{self, ReviewMessage, Vod};
use cuttlefish::corpus_reviews::ID_PREFIX;
use cuttlefish::doc::{Document, doc_id};
use cuttlefish::expert::{self, ExpertComment};
use cuttlefish::glossary::{AliasStatus, Glossary, RelationKind, Term};
use cuttlefish::index::FlatIndex;
use cuttlefish::notes;
use cuttlefish::pedia::{self, FactCard, Section};
use cuttlefish::questions::Bank;
use cuttlefish::slang::{self, UserGlossary};
use cuttlefish::store::Store;
use cuttlefish::tables;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// Quotes an entry shows unless asked for more
pub const QUOTES: usize = 6;
/// Quotes an entry shows at most
pub const MAX_QUOTES: usize = 50;
/// Characters of a quote
const QUOTE_CHARS: usize = 360;

/// The expert comments of the corpus and the terms each mentions, for one
/// corpus file and one set of names
pub struct Wild {
    /// The knowledge folder, the corpus file's size and time, and the key
    /// of the names searched
    stamp: (PathBuf, u64, Option<SystemTime>, u64),
    /// The corpus
    vods: Vec<Vod>,
    /// Each message's VOD and place in it, by its link
    messages: BTreeMap<String, (usize, usize)>,
    comments: Vec<ExpertComment>,
    /// Term id → indices into `comments`
    mentions: BTreeMap<String, Vec<usize>>,
}

/// The fact cards as read, with the knowledge folder and the size and
/// time of the store's index file they were read for (a file: every
/// import rewrites it, while a folder's time may never change on a
/// network mount)
struct Cards {
    stamp: (PathBuf, u64, Option<SystemTime>),
    cards: Arc<Vec<FactCard>>,
}

/// The fact cards between requests: the ones read last, and whether a
/// thread is reading them now
#[derive(Default)]
struct CardState {
    read: Option<Cards>,
    reading: bool,
}

/// How long the list of reviews here is trusted
const REVIEWED_FOR: Duration = Duration::from_secs(60);

/// The size and time of the files a [`Loaded`] is made from
/// (`glossary.toml`, the `terms/` folder, `glossary-user.toml`, the corpus)
type Inputs = (PathBuf, [(u64, Option<SystemTime>); 4]);

/// What the Pedia keeps between requests
#[derive(Default)]
pub struct Pedia {
    loaded: Mutex<Option<(Inputs, Arc<Loaded>)>>,
    wild: Mutex<Option<Arc<Wild>>>,
    cards: Arc<Mutex<CardState>>,
    /// The ids of the reviews here of #vod-review VODs, and when they were
    /// listed
    reviewed: Mutex<Option<(Instant, Arc<BTreeSet<String>>)>>,
}

/// The glossary with what the Pedia adds to it
struct Loaded {
    glossary: Glossary,
    user: UserGlossary,
    /// The games of every term, by id
    games: BTreeMap<String, Vec<String>>,
    /// The terms in scope, in glossary order
    scope: Vec<usize>,
    wild: Arc<Wild>,
}

impl Loaded {
    fn term(&self, id: &str) -> Option<&Term> {
        self.glossary.terms.iter().find(|t| t.id == id)
    }

    /// Messages mentioning a term
    fn count(&self, id: &str) -> usize {
        self.wild
            .mentions
            .get(id)
            .map_or(0, |found| pedia::messages(found, &self.wild.comments))
    }

    /// A term's main name in each language
    fn main_names(t: &Term) -> BTreeMap<&str, &str> {
        t.forms
            .iter()
            .filter_map(|(lang, names)| Some((lang.as_str(), names.first()?.as_str())))
            .collect()
    }

    /// A related term as the page links it
    fn link(&self, id: &str, name: &str, kind: RelationKind) -> Value {
        let found = self.term(id);
        json!({
            "id": found.map_or(id, |t| t.id.as_str()),
            "kind": kind,
            "name": found.map_or(name, pedia::english),
            "names": found.map(Self::main_names),
            "known": found.is_some(),
        })
    }
}

/// When a file or folder last changed, and its size
fn stamp_of(path: &Path) -> (u64, Option<SystemTime>) {
    std::fs::metadata(path).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

/// The inputs of a [`Loaded`] as they are now
fn inputs(root: &Path) -> Inputs {
    let files = [
        root.join("glossary.toml"),
        tables::dir(root),
        root.join(slang::FILE),
        corpus::path(root),
    ];
    (root.to_path_buf(), files.map(|f| stamp_of(&f)))
}

impl Pedia {
    /// The glossary and the terms in scope, made again when one of their
    /// files changed (an edit through the slang endpoints writes the user
    /// file)
    fn load(&self, root: &Path) -> Result<Arc<Loaded>> {
        let now = inputs(root);
        if let Some((seen, loaded)) = &*self.loaded.lock().unwrap()
            && *seen == now
        {
            return Ok(Arc::clone(loaded));
        }
        let started = Instant::now();
        let loaded = Arc::new(self.make(root)?);
        log::debug!(
            "Pedia: glossary of {} terms loaded in {} ms",
            loaded.glossary.terms.len(),
            started.elapsed().as_millis()
        );
        *self.loaded.lock().unwrap() = Some((now, Arc::clone(&loaded)));
        Ok(loaded)
    }

    /// The glossary as `Store::load_glossary` makes it, keeping the name
    /// tables for the games of the names, and the terms in scope with
    /// their mentions
    fn make(&self, root: &Path) -> Result<Loaded> {
        let tables = tables::load_all(root);
        let mut glossary = Store::own_glossary(root)?;
        let seed: BTreeSet<String> = glossary.terms.iter().map(|t| t.id.clone()).collect();
        for table in &tables {
            glossary.merge(&table.terms);
        }
        let user = UserGlossary::load(root)?;
        user.apply(&mut glossary);
        let table_games = pedia::table_games(&tables);
        let seed: BTreeSet<&str> = seed.iter().map(String::as_str).collect();
        // The seed is about Splatoon 3, whatever older tables it merged
        let games: BTreeMap<String, Vec<String>> = glossary
            .terms
            .iter()
            .map(|t| {
                let mut games = pedia::games(t, &table_games);
                if seed.contains(t.id.as_str()) && !games.iter().any(|g| g == "S3") {
                    games.insert(0, String::from("S3"));
                }
                (t.id.clone(), games)
            })
            .collect();
        let candidates: BTreeSet<&str> = glossary
            .terms
            .iter()
            .filter(|t| pedia::candidate(t, &glossary, &seed, &user, &games[&t.id]))
            .map(|t| t.id.as_str())
            .collect();
        let wild = self.wild(root, &glossary, &candidates)?;
        let count = |id: &str| {
            wild.mentions
                .get(id)
                .map_or(0, |found| pedia::messages(found, &wild.comments))
        };
        let mut keep: BTreeSet<&str> = glossary
            .terms
            .iter()
            .filter(|t| candidates.contains(t.id.as_str()))
            .filter(|t| pedia::in_scope(t, &seed, &user, count(&t.id)))
            .map(|t| t.id.as_str())
            .collect();
        // The broader terms they belong to are entries too
        let targets: Vec<&str> = glossary
            .terms
            .iter()
            .filter(|t| keep.contains(t.id.as_str()))
            .filter_map(|t| t.related.as_ref().map(|r| r.term.as_str()))
            .collect();
        keep.extend(targets);
        let scope = glossary
            .terms
            .iter()
            .enumerate()
            .filter(|(_, t)| keep.contains(t.id.as_str()))
            .map(|(i, _)| i)
            .collect();
        Ok(Loaded {
            glossary,
            user,
            games,
            scope,
            wild,
        })
    }

    /// The corpus's comments and their mentions of `terms`, searched again
    /// when the corpus or the terms' names changed
    fn wild(&self, root: &Path, g: &Glossary, terms: &BTreeSet<&str>) -> Result<Arc<Wild>> {
        let (len, time) = stamp_of(&corpus::path(root));
        let names = pedia::names_key(g.terms.iter().filter(|t| terms.contains(t.id.as_str())));
        let stamp = (root.to_path_buf(), len, time, names);
        let mut cached = self.wild.lock().unwrap();
        if let Some(wild) = &*cached
            && wild.stamp == stamp
        {
            return Ok(Arc::clone(wild));
        }
        let started = std::time::Instant::now();
        let vods = match corpus::read(root) {
            Ok(c) => c.vods,
            Err(e) if time.is_none() => {
                log::info!("Pedia: no #vod-review corpus yet ({e:#})");
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        let comments: Vec<ExpertComment> = vods.iter().flat_map(expert::comments).collect();
        let mentions = pedia::mentions(g, terms, &comments);
        log::info!(
            "Pedia: {} terms searched in {} comments of {} VODs in {:.1} s",
            terms.len(),
            comments.len(),
            vods.len(),
            started.elapsed().as_secs_f32()
        );
        let messages = vods
            .iter()
            .enumerate()
            .flat_map(|(v, vod)| {
                vod.messages
                    .iter()
                    .enumerate()
                    .map(move |(m, message)| (message.url.clone(), (v, m)))
            })
            .collect();
        let wild = Arc::new(Wild {
            stamp,
            vods,
            messages,
            comments,
            mentions,
        });
        *cached = Some(Arc::clone(&wild));
        Ok(wild)
    }

    /// The fact cards of game data as read last (none before the first
    /// read is done), and whether a thread is reading them now: one starts
    /// when the store's index file changed since, so no request waits for
    /// every stored document to be read
    fn cards(&self, root: &Path, loaded: &Arc<Loaded>) -> (Arc<Vec<FactCard>>, bool) {
        let (len, time) = stamp_of(&FlatIndex::entries_file(&root.join("index")));
        let stamp = (root.to_path_buf(), len, time);
        let mut state = self.cards.lock().unwrap();
        let have = state
            .read
            .as_ref()
            .map(|c| Arc::clone(&c.cards))
            .unwrap_or_default();
        if state.read.as_ref().is_some_and(|c| c.stamp == stamp) {
            return (have, false);
        }
        if !state.reading {
            state.reading = true;
            let state = Arc::clone(&self.cards);
            let loaded = Arc::clone(loaded);
            let root = root.to_path_buf();
            std::thread::spawn(move || {
                let started = Instant::now();
                let cards = pedia::fact_cards(&root, &loaded.glossary);
                log::info!(
                    "Pedia: {} fact cards read in {:.1} s",
                    cards.len(),
                    started.elapsed().as_secs_f32()
                );
                let mut state = state.lock().unwrap();
                state.read = Some(Cards {
                    stamp,
                    cards: Arc::new(cards),
                });
                state.reading = false;
            });
        }
        (have, true)
    }

    /// The ids of the reviews here of #vod-review VODs (folders named
    /// `discord-<vod>`), listed again after [`REVIEWED_FOR`]: an entry
    /// asks for the VODs of hundreds of comments
    fn reviewed(&self, reviews: &Path) -> Arc<BTreeSet<String>> {
        let mut cached = self.reviewed.lock().unwrap();
        if let Some((at, ids)) = &*cached
            && at.elapsed() < REVIEWED_FOR
        {
            return Arc::clone(ids);
        }
        let ids: BTreeSet<String> = std::fs::read_dir(reviews)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|id| id.starts_with(ID_PREFIX))
            .collect();
        let ids = Arc::new(ids);
        *cached = Some((Instant::now(), Arc::clone(&ids)));
        ids
    }

    /// `GET pedia`: every term in scope and the sections
    pub fn list(&self, root: &Path) -> Result<Value> {
        let loaded = self.load(root)?;
        let mut sections: BTreeMap<Section, usize> = BTreeMap::new();
        let terms: Vec<Value> = loaded
            .scope
            .iter()
            .map(|&i| {
                let t = &loaded.glossary.terms[i];
                let section = pedia::section(&loaded.glossary, t);
                *sections.entry(section).or_default() += 1;
                let mut names: Vec<&str> = Vec::new();
                for n in t.names() {
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
                json!({
                    "id": t.id,
                    "name": pedia::english(t),
                    "names": Loaded::main_names(t),
                    "search": names,
                    "section": section,
                    "kind": t.kind,
                    "games": loaded.games[&t.id],
                    "facets": pedia::facets(t, &loaded.user),
                    "mentions": loaded.count(&t.id),
                    "definition": t.definition,
                    "related": t.related,
                })
            })
            .collect();
        let sections: Vec<Value> = Section::ALL
            .iter()
            .map(|s| json!({ "id": s, "count": sections.get(s).copied().unwrap_or(0) }))
            .collect();
        Ok(json!({
            "terms": terms,
            "sections": sections,
            "glossary": loaded.glossary.terms.len(),
            "corpus": {
                "vods": loaded.wild.vods.len(),
                "comments": loaded.wild.comments.len(),
            },
        }))
    }

    /// `GET pedia/<id>`: one entry, with up to `quotes` quotes
    pub fn entry(
        &self,
        root: &Path,
        reviews: &Path,
        catalogue: &Catalogue,
        id: &str,
        quotes: usize,
    ) -> Result<Value> {
        let started = Instant::now();
        let loaded = self.load(root)?;
        let g = &loaded.glossary;
        let t = loaded
            .term(id)
            .or_else(|| g.lookup(id))
            .with_context(|| format!("no glossary term {id}"))?;
        let in_scope = loaded.scope.iter().any(|&i| g.terms[i].id == t.id);

        // Aliases, with the user file's id when it has them
        let place = g.terms.iter().position(|o| o.id == t.id);
        let aliases: Vec<Value> = t
            .aliases
            .iter()
            .map(|a| {
                let own = loaded.user.aliases.iter().find(|u| {
                    u.lang == a.lang
                        && u.text.trim().eq_ignore_ascii_case(a.text.trim())
                        && u.find(g) == place
                });
                let mut value = json!(a);
                value["id"] = json!(own.map(|u| &u.id));
                value["auto"] = json!(own.is_some_and(|u| u.auto));
                value
            })
            .collect();

        // Relations both ways: its broader term, and the terms that name
        // it as theirs
        let outgoing: Vec<Value> = t
            .related
            .iter()
            .map(|r| loaded.link(&r.term, &r.name, r.kind))
            .collect();
        let incoming: Vec<Value> = g
            .terms
            .iter()
            .filter_map(|o| {
                let r = o.related.as_ref().filter(|r| r.term == t.id)?;
                Some(loaded.link(&o.id, pedia::english(o), r.kind))
            })
            .collect();

        // A stat.ink icon linked to it: an SVG first, else the largest
        let icon = catalogue
            .assets
            .iter()
            .filter(|a| a.term.as_deref() == Some(t.id.as_str()))
            .max_by_key(|a| (a.format == "svg", a.width.unwrap_or(0)))
            .map(|a| a.id.clone());

        let (all_cards, cards_pending) = self.cards(root, &loaded);
        let cards: Vec<&FactCard> = all_cards.iter().filter(|c| c.term == t.id).collect();
        // Expert notes about it, and the deep questions that name it (with
        // the note answering each, if one does)
        let all_notes = notes::list(root)?;
        let questions: Vec<_> = Bank::seed()
            .with_references(&all_notes)
            .questions
            .into_iter()
            .filter(|q| pedia::names_term(&q.en, t) || pedia::names_term(&q.zh, t))
            .collect();
        let notes: Vec<_> = all_notes
            .into_iter()
            .filter(|n| pedia::note_is_about(n, t))
            .collect();

        // In the wild: the best comments, placed ones first
        let wild = &loaded.wild;
        let found: &[usize] = wild.mentions.get(&t.id).map_or(&[], Vec::as_slice);
        let reviewed = self.reviewed(reviews);
        let review_of = |c: &ExpertComment| {
            let id = format!("{ID_PREFIX}{}", c.expert.vod);
            reviewed.contains(&id).then_some(id)
        };
        let names: Vec<&str> = t.names().collect();
        let shown: Vec<Value> = pedia::rank_quotes(found, &wild.comments, &|c| {
            c.expert.t_s.is_some() && review_of(c).is_some()
        })
        .into_iter()
        .take(quotes.clamp(1, MAX_QUOTES))
        .map(|i| {
            let c = &wild.comments[i];
            let e = &c.expert;
            json!({
                "text": pedia::snippet(&c.text, &names, QUOTE_CHARS),
                "url": c.url,
                "reviewer": e.reviewer,
                "date": e.date,
                "game": e.game,
                "moment": e.moment(),
                "t_s": e.t_s,
                "review": e.t_s.and_then(|_| review_of(c)),
            })
        })
        .collect();

        let own = loaded.user.term(&t.id);
        // The user's edit of a glossary term
        let over = loaded
            .user
            .overrides
            .iter()
            .find(|o| slang::find_term(g, &o.term, &o.term_name) == place);
        // Slang suggested for it, waiting for the user
        let pending: Vec<_> = loaded
            .user
            .aliases
            .iter()
            .filter(|a| a.status == AliasStatus::Pending && a.find(g) == place)
            .collect();
        log::debug!("Pedia: entry {id} in {} ms", started.elapsed().as_millis());
        Ok(json!({
            "term": t,
            "name": pedia::english(t),
            "in_scope": in_scope,
            "section": pedia::section(g, t),
            "games": loaded.games.get(&t.id),
            "facets": pedia::facets(t, &loaded.user),
            "user_term": own,
            "override": over,
            "aliases": aliases,
            "pending": pending,
            "related": { "out": outgoing, "in": incoming },
            "icon": icon,
            "cards": cards,
            "cards_pending": cards_pending,
            "notes": notes,
            "questions": questions,
            "mentions": loaded.count(&t.id),
            "quotes": shown,
        }))
    }
}

/// A message of a VOD conversation as the popover shows it
fn message_value(m: &ReviewMessage) -> Value {
    json!({
        "id": m.id,
        "url": m.url,
        "author": m.author,
        "time": m.time,
        "text": m.text,
    })
}

impl Pedia {
    /// `GET source`: the context of a cited source or a quote (see the
    /// module's endpoints)
    pub fn source(
        &self,
        root: &Path,
        reviews: &Path,
        query: &std::collections::HashMap<String, String>,
    ) -> Result<Value> {
        let arg = |key: &str| query.get(key).map_or("", String::as_str).trim();
        let url = arg("url");
        if !url.is_empty() {
            let loaded = self.load(root)?;
            if let Some(&(v, m)) = loaded.wild.messages.get(url) {
                let reviewed = self.reviewed(reviews);
                return Ok(Self::comment(&loaded.wild.vods[v], m, &reviewed));
            }
        }
        // A document's chunk: by id and position, else the document of the
        // link and the chunk under the heading
        let id = match arg("doc") {
            "" if !url.is_empty() => doc_id(url),
            id => id.to_string(),
        };
        let doc: Option<Document> = cuttlefish::store::is_id(&id)
            .then(|| std::fs::read(root.join("docs").join(format!("{id}.json"))).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let Some(doc) = doc else {
            return Ok(json!({
                "kind": "chunk",
                "title": arg("title"),
                "heading": arg("heading"),
                "url": (!url.is_empty()).then_some(url),
                "text": null,
            }));
        };
        let chunks = if doc.expert_comments.is_empty() {
            chunk_text(&doc.text, &ChunkConfig::default())
        } else {
            doc.expert_comments
                .iter()
                .enumerate()
                .map(|(i, c)| cuttlefish::chunk::Chunk {
                    ordinal: i as u32,
                    heading: c.expert.label(),
                    text: c.text.clone(),
                })
                .collect()
        };
        let ordinal: Option<u32> = arg("ordinal").parse().ok();
        let heading = arg("heading");
        let chunk = chunks
            .iter()
            .find(|c| Some(c.ordinal) == ordinal)
            .or_else(|| {
                chunks
                    .iter()
                    .find(|c| !heading.is_empty() && c.heading == heading)
            })
            .or(chunks.first());
        Ok(json!({
            "kind": "chunk",
            "title": doc.title,
            "heading": chunk.map_or(heading, |c| c.heading.as_str()),
            "text": chunk.map(|c| &c.text),
            "url": doc.url,
            "source": doc.source,
            "license": doc.license,
            "attribution": doc.attribution,
            "game": doc.era(),
            "language": doc.language,
        }))
    }

    /// A #vod-review message in its conversation: the message it replies
    /// to, the replies to it, the VOD and its moments placed in the video,
    /// each with the review to open when the VOD is one here (`reviewed`,
    /// [`Pedia::reviewed`])
    fn comment(vod: &Vod, at: usize, reviewed: &BTreeSet<String>) -> Value {
        let m = &vod.messages[at];
        let reply_to = m
            .reply_to
            .as_ref()
            .and_then(|id| vod.messages.iter().find(|p| &p.id == id));
        let replies: Vec<Value> = vod
            .messages
            .iter()
            .filter(|r| r.reply_to.as_deref() == Some(m.id.as_str()))
            .map(message_value)
            .collect();
        let review = format!("{ID_PREFIX}{}", vod.id);
        let has_review = reviewed.contains(&review);
        let moments: Vec<Value> = m
            .moments
            .iter()
            .filter(|x| x.aligned)
            .filter_map(|x| Some(json!({ "raw": x.raw, "wave": x.wave, "t_s": x.t_s? })))
            .collect();
        json!({
            "kind": "comment",
            "message": message_value(m),
            "poster": m.author == vod.poster,
            "reply_to": reply_to.map(message_value),
            "replies": replies,
            "game": vod.game,
            "vod": {
                "url": vod.url,
                "poster": vod.poster,
                "date": vod.date,
                "thread": vod.thread,
                "video": vod.video.url,
            },
            "moments": moments,
            "review": has_review.then_some(review),
        })
    }
}
