//! Overfishing Pedia: the glossary as an encyclopedia of Salmon Run for new
//! players.
//!
//! The glossary holds the community's knowledge in compressed form:
//! official names in every language, the slang players use, the
//! techniques and boss parts the slang suggestions added as new terms, and
//! how they relate. The Pedia shows the part of it about Salmon Run
//! ([`in_scope`]): every term of the seed and of the user file, every term
//! with slang or a definition, the names of the Salmon Run tables (bosses,
//! events, tides, stages, titles, and Lean's datamine's Salmonids, stages,
//! Salmon Run weapons and specials), and the other weapons, specials and
//! subs of Splatoon 2 and 3 that #vod-review talks about. Each term goes into a
//! friendly [`Section`] ([`section`]) from its kind, a few well-known ids
//! and the broader term it belongs to.
//!
//! "In the wild" is what the community says: every expert comment of the
//! #vod-review corpus ([`crate::expert::comments`]) is searched once for
//! the names of the terms in scope ([`mentions`]), which gives each term
//! its comments (for quotes, [`rank_quotes`]) and how often it is
//! discussed. The caller keeps the result until the corpus or the names
//! change ([`names_key`]).
//!
//! Fact cards from game data ([`FactCard`], documents of source kind
//! `game-data`), the user's expert notes ([`crate::notes`],
//! [`note_is_about`]) and the deep questions naming a term ([`names_term`])
//! join its entry when they exist.

use crate::expert::ExpertComment;
use crate::glossary::{AliasSource, Glossary, RelationKind, Term};
use crate::notes::Note;
use crate::slang::UserGlossary;
use crate::stats::{self, Facts, Named, Summary};
use crate::tables::Table;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A part of the Pedia, as browsed
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    /// Movement techniques: inertia cancel, strafes, squid rolls
    Movement,
    /// Bosses, lesser Salmonids, and their parts and attacks
    Bosses,
    /// King Salmonids
    Kings,
    /// Special events (known occurrences) and tides
    Events,
    /// Golden Eggs, the basket and how eggs get there
    Eggs,
    /// Roles, team play and strategy
    Strategy,
    /// Grizzco and rental weapons, specials and subs
    Weapons,
    /// Salmon Run stages
    Stages,
    /// Modes, ranks and mechanics that fit nowhere else
    Other,
}

impl Section {
    /// In the order browsed
    pub const ALL: [Section; 9] = [
        Section::Movement,
        Section::Bosses,
        Section::Kings,
        Section::Events,
        Section::Eggs,
        Section::Strategy,
        Section::Weapons,
        Section::Stages,
        Section::Other,
    ];
}

/// Sections of the terms whose kind does not tell: the seed's own terms
/// (which carry no kind), and table names the kind files under something
/// broader (a King Salmonid is a `boss`)
const KNOWN: &[(&str, Section)] = &[
    ("inertia-cancel", Section::Movement),
    ("golden-egg", Section::Eggs),
    ("power-egg", Section::Eggs),
    ("egg-basket", Section::Eggs),
    ("egg-run", Section::Eggs),
    ("fetch-eggs", Section::Eggs),
    ("outer-eggs", Section::Eggs),
    ("boss", Section::Bosses),
    ("boss-salmonid", Section::Bosses),
    ("boss-salmonids", Section::Bosses),
    ("king", Section::Kings),
    ("king-salmonid", Section::Kings),
    ("king-salmonids", Section::Kings),
    ("cohozuna", Section::Kings),
    ("horrorboros", Section::Kings),
    ("megalodontia", Section::Kings),
    ("triumvirate", Section::Kings),
    ("smallfry", Section::Bosses),
    ("chum", Section::Bosses),
    ("cohock", Section::Bosses),
    ("goldie", Section::Bosses),
    ("griller", Section::Bosses),
    ("mudmouth", Section::Bosses),
    ("steelhead", Section::Bosses),
    ("flyfish", Section::Bosses),
    ("steel-eel", Section::Bosses),
    ("scrapper", Section::Bosses),
    ("stinger", Section::Bosses),
    ("maws", Section::Bosses),
    ("drizzler", Section::Bosses),
    ("fish-stick", Section::Bosses),
    ("flipper-flopper", Section::Bosses),
    ("big-shot", Section::Bosses),
    ("slammin-lid", Section::Bosses),
    ("rush", Section::Events),
    ("fog", Section::Events),
    ("goldie-seeking", Section::Events),
    ("the-mothership", Section::Events),
    ("cohock-charge", Section::Events),
    ("giant-tornado", Section::Events),
    ("mudmouth-eruptions", Section::Events),
    ("high-tide", Section::Events),
    ("normal-tide", Section::Events),
    ("low-tide", Section::Events),
    ("xtrawave", Section::Events),
    ("grizzco-weapon", Section::Weapons),
    ("short-range-weapon", Section::Weapons),
    ("sploosh-o-matic", Section::Weapons),
    ("splattershot", Section::Weapons),
    ("grizzco-roller", Section::Weapons),
    ("killer-wail-51", Section::Weapons),
    ("spawning-grounds", Section::Stages),
    ("marooners-bay", Section::Stages),
    ("gone-fission-hydroplant", Section::Stages),
    ("sockeye-station", Section::Stages),
    ("jammin-salmon-junction", Section::Stages),
    ("salmonid-smokeyard", Section::Stages),
    ("bonerattle-arena", Section::Stages),
];

/// Section of an imported or suggested kind, when the kind tells
fn by_kind(kind: &str) -> Option<Section> {
    Some(match kind {
        "boss" | "enemy" => Section::Bosses,
        "event" | "tide" => Section::Events,
        "stage" => Section::Stages,
        "weapon" | "special" | "sub" => Section::Weapons,
        "role" | "player type" | "callout" | "phase" => Section::Strategy,
        "salmon-run" | "mode" | "title" | "scale" | "rotation" => Section::Other,
        _ => return None,
    })
}

/// Words that make a technique a movement one
const MOVING: &[&str] = &[
    "strafe", "inertia", "momentum", "squid", "swim", "roll", "surge", "hop", "cancel",
];

/// The section of `term` in `g`: a known id, its kind, the section of the
/// broader term it is part or a kind of (a Flyfish's missiles are about
/// bosses), or of the term it relates to when that says enough (a boss, a
/// movement or egg technique, a weapon, an event); else a technique that
/// moves is movement, other techniques and roles are strategy, the rest
/// other
pub fn section(g: &Glossary, term: &Term) -> Section {
    section_depth(g, term, 0)
}

fn section_depth(g: &Glossary, term: &Term, depth: u8) -> Section {
    if let Some((_, s)) = KNOWN.iter().find(|(id, _)| *id == term.id) {
        return *s;
    }
    let kind = term.kind.as_deref().unwrap_or_default();
    if let Some(s) = by_kind(kind) {
        return s;
    }
    if let Some(r) = &term.related
        && depth < 4
        && let Some(target) = g.terms.iter().find(|t| t.id == r.term)
    {
        let theirs = section_depth(g, target, depth + 1);
        // A boss, or bosses as such
        let boss = target.kind.as_deref() == Some("boss")
            || matches!(
                target.id.as_str(),
                "boss" | "boss-salmonid" | "boss-salmonids"
            );
        let inherit = match r.kind {
            RelationKind::PartOf | RelationKind::KindOf => theirs != Section::Other,
            RelationKind::RelatedTo => {
                boss || matches!(
                    theirs,
                    Section::Movement
                        | Section::Eggs
                        | Section::Events
                        | Section::Weapons
                        | Section::Kings
                )
            }
        };
        if inherit {
            return theirs;
        }
    }
    match kind {
        "technique" => {
            let text = alloc::format!("{} {}", english(term), term.definition).to_lowercase();
            let moving = text
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| MOVING.iter().any(|m| w.starts_with(m)));
            if moving {
                Section::Movement
            } else {
                Section::Strategy
            }
        }
        "category" => Section::Bosses,
        _ => Section::Other,
    }
}

/// The English name, else the first name, else the id
pub fn english(term: &Term) -> &str {
    term.name("en")
        .or_else(|| term.forms.values().flatten().next().map(String::as_str))
        .unwrap_or(&term.id)
}

/// The games a term's names belong to (`S3`, `S2`, `S1`), newest first:
/// its own, and those of every name table it came from; empty for the
/// seed's and the user's terms, which are about Salmon Run of any game
pub fn games(term: &Term, tables: &BTreeMap<&str, &str>) -> Vec<String> {
    let mut out: BTreeSet<&str> = term.game.iter().map(String::as_str).collect();
    for from in &term.from {
        let source = from.split('#').next().unwrap_or(from);
        if let Some(game) = tables.get(source) {
            out.insert(game);
        }
    }
    out.into_iter().rev().map(String::from).collect()
}

/// Each name table's game by its source, for [`games`]
pub fn table_games(tables: &[Table]) -> BTreeMap<&str, &str> {
    tables
        .iter()
        .filter_map(|t| Some((t.source.as_str(), t.game.as_deref()?)))
        .collect()
}

/// Where a term's content comes from, any of the three
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Facets {
    /// Names from the game: an imported name table, or the seed's
    /// in-game names in other languages
    pub official: bool,
    /// The community's: slang from the seed or suggested from its texts,
    /// a term suggested from them, or player jargon of the seed
    pub community: bool,
    /// The user's: a term or alias taught in the studio, or a term edited
    /// there (an override of a glossary term)
    pub user: bool,
}

/// The facets of `term`, with the user file that may have added or
/// edited it
pub fn facets(term: &Term, user: &UserGlossary) -> Facets {
    let own = user.term(&term.id);
    let official = !term.from.is_empty() || (own.is_none() && term.forms.len() > 1);
    let by_user = |s: AliasSource| s == AliasSource::User;
    Facets {
        official,
        community: term
            .approved()
            .any(|a| matches!(a.source, AliasSource::Seed | AliasSource::Suggested))
            || own.is_some_and(|t| t.source == AliasSource::Suggested)
            || (own.is_none() && !official),
        user: own.is_some_and(|t| by_user(t.source))
            || term.approved().any(|a| by_user(a.source))
            || user.overrides.iter().any(|o| {
                o.term == term.id || (!o.term_name.is_empty() && term.has_name(&o.term_name))
            }),
    }
}

/// Kinds of names never about Salmon Run (gear, brands, abilities,
/// medals, Splatfests, seasons, ranks, ways to be splatted in battle, the
/// battle modes) or only cosmetic in it (work uniforms, scales)
const OFF_TOPIC: &[&str] = &[
    "gear",
    "brand",
    "ability",
    "medal",
    "splatfest",
    "season",
    "rank",
    "death",
    "mode",
    "uniform",
    "scale",
];

/// Kinds of the Salmon Run tables' names (`salmon-boss3.php`, ...) that are
/// in scope; their other names are interface text (`Avg. Pts.`)
const SALMON_KINDS: &[&str] = &["boss", "event", "tide", "stage", "title"];

/// Whether `term` has a name from a Salmon Run table: stat.ink's of a
/// kind in scope, or Lean's datamine's ([`crate::leanny::name_table`]:
/// every Salmonid, stage, Salmon Run weapon and special)
fn salmon_name(term: &Term) -> bool {
    let kind = term.kind.as_deref().unwrap_or_default();
    term.from.iter().any(|f| {
        f.split('#').next() == Some(crate::leanny::NAMES_SOURCE)
            || (SALMON_KINDS.contains(&kind) && f.contains("/salmon-"))
    })
}

/// Whether a term may be in the Pedia, before knowing what the corpus
/// says: the seed's and the user's terms, terms with a definition, slang or
/// a broader term, the names of the Salmon Run tables ([`salmon_name`]:
/// every Salmon Run weapon and special among them), and the weapons,
/// specials and subs of Splatoon 2 and 3 (which [`in_scope`] keeps when
/// discussed). Placeholders (`?`, `(Normal)`, `Any Weapon`) never are,
/// nor names of kinds about battles or cosmetics ([`OFF_TOPIC`]) or a
/// stage's short name ([`is_short_name`]) unless the seed or the user file
/// has them.
pub fn candidate(
    term: &Term,
    g: &Glossary,
    seed: &BTreeSet<&str>,
    user: &UserGlossary,
    games: &[String],
) -> bool {
    let name = english(term);
    if name == "?" || name.starts_with('(') || name.starts_with("Any ") {
        return false;
    }
    if seed.contains(term.id.as_str()) || user.term(&term.id).is_some() {
        return true;
    }
    if is_short_name(term, g) {
        return false;
    }
    let kind = term.kind.as_deref().unwrap_or_default();
    if OFF_TOPIC.contains(&kind) {
        return false;
    }
    !term.definition.is_empty()
        || term.related.is_some()
        || term.approved().next().is_some()
        || salmon_name(term)
        || (matches!(kind, "weapon" | "special" | "sub" | "stage")
            && games.iter().any(|g| g == "S3" || g == "S2"))
}

/// Whether a candidate stays: a weapon, special, sub or stage only when
/// #vod-review mentions it, a Salmon Run table names it (Lean's datamine
/// lists every Salmon Run weapon and special), the seed or the user has
/// it, or it is a Grizzco weapon (the kits and variants of Turf War, and
/// its stages, never come up; slang alone does not tell); every other
/// candidate does
pub fn in_scope(term: &Term, seed: &BTreeSet<&str>, user: &UserGlossary, mentions: usize) -> bool {
    let kind = term.kind.as_deref().unwrap_or_default();
    let own = seed.contains(term.id.as_str())
        || user.term(&term.id).is_some()
        || !term.definition.is_empty()
        || salmon_name(term);
    !matches!(kind, "weapon" | "special" | "sub" | "stage")
        || own
        || mentions > 0
        || english(term).starts_with("Grizzco")
}

/// Whether `term` is a stage's short name (`Grounds`, `Bay`): one word of
/// another stage's name in `g`, which is the entry
pub fn is_short_name(term: &Term, g: &Glossary) -> bool {
    let name = english(term);
    term.kind.as_deref() == Some("stage")
        && !name.contains(' ')
        && g.terms.iter().any(|o| {
            let other = english(o);
            o.id != term.id
                && o.kind.as_deref() == Some("stage")
                && other.contains(' ')
                && other.split(' ').any(|w| w.eq_ignore_ascii_case(name))
        })
}

/// A key of every name of `terms` (official and approved aliases): the
/// mentions are found again when it changes
pub fn names_key<'a>(terms: impl Iterator<Item = &'a Term>) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for t in terms {
        for part in core::iter::once(t.id.as_str()).chain(t.names()) {
            for b in part.bytes().chain(core::iter::once(0)) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x100000001b3);
            }
        }
    }
    h
}

/// A comment's own words: without the message it replies to, which
/// [`crate::expert::comments`] puts first as context
pub fn own_words(text: &str) -> &str {
    match text.split_once('\n') {
        Some((first, rest)) if first.starts_with("(replying to ") => rest,
        _ => text,
    }
}

/// For each term of `terms` (ids in `g`), the comments that mention it by
/// an official name or an approved alias (indices into `comments`, in
/// order); the reply context of a comment does not count
pub fn mentions(
    g: &Glossary,
    terms: &BTreeSet<&str>,
    comments: &[ExpertComment],
) -> BTreeMap<String, Vec<usize>> {
    let sub = Glossary {
        terms: g
            .terms
            .iter()
            .filter(|t| terms.contains(t.id.as_str()))
            .cloned()
            .collect(),
    };
    let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, c) in comments.iter().enumerate() {
        for t in sub.find_in(own_words(&c.text)) {
            out.entry(t.id.clone()).or_default().push(i);
        }
    }
    out
}

/// Messages among `found` (comments' indices): pieces of one message count
/// once
pub fn messages(found: &[usize], comments: &[ExpertComment]) -> usize {
    let urls: BTreeSet<&str> = found.iter().map(|&i| comments[i].url.as_str()).collect();
    urls.len()
}

/// The comments to quote first among `found`: one piece per message, at
/// most two per VOD, those that can open a review at their moment first
/// (`placed` tells), then Splatoon 3's, then those of a readable length,
/// then the newest
pub fn rank_quotes(
    found: &[usize],
    comments: &[ExpertComment],
    placed: &dyn Fn(&ExpertComment) -> bool,
) -> Vec<usize> {
    let mut scored: Vec<(u8, chrono::NaiveDate, usize)> = found
        .iter()
        .map(|&i| {
            let c = &comments[i];
            let len = own_words(&c.text).chars().count();
            let score = u8::from(placed(c)) * 4
                + u8::from(c.expert.game == crate::game::Game::S3) * 2
                + u8::from((60..=600).contains(&len));
            (score, c.expert.date, i)
        })
        .collect();
    scored.sort_by_key(|&(score, date, _)| core::cmp::Reverse((score, date)));
    let mut out: Vec<usize> = Vec::new();
    for (_, _, i) in scored {
        let c = &comments[i];
        let same_message = out.iter().any(|&o| comments[o].url == c.url);
        let same_vod = out
            .iter()
            .filter(|&&o| comments[o].expert.vod == c.expert.vod)
            .count();
        if !same_message && same_vod < 2 {
            out.push(i);
        }
    }
    out
}

/// Where the first of `names` stands in `text` (ignoring case; a Latin
/// name as a whole word), as a byte range
fn first_mention(text: &str, names: &[&str]) -> Option<(usize, usize)> {
    let hay = text.to_lowercase();
    // Lowercasing keeps byte offsets for the scripts the glossary uses;
    // when it does not, the start of the text is quoted
    if hay.len() != text.len() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for name in names {
        let needle = name.to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let latin = !needle.chars().any(|c| c as u32 >= 0x3040);
        for (start, _) in hay.match_indices(&needle) {
            let end = start + needle.len();
            let bounded = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
            if !latin
                || (bounded(hay[..start].chars().next_back()) && bounded(hay[end..].chars().next()))
            {
                if best.is_none_or(|(s, _)| start < s) {
                    best = Some((start, end));
                }
                break;
            }
        }
    }
    best
}

/// A quote of at most `max` characters of `text` around the first mention
/// of one of `names`, on one line, with `…` where it is cut
pub fn snippet(text: &str, names: &[&str], max: usize) -> String {
    let text = own_words(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text;
    }
    let at = first_mention(&text, names).map_or(0, |(s, _)| text[..s].chars().count());
    // A third of the room before the mention, at a word's start
    let mut start = at.saturating_sub(max / 3);
    if start > 0 {
        while start < at && !chars[start - 1].is_whitespace() {
            start += 1;
        }
    }
    let end = (start + max).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out
}

/// A fact card from game data: a document of source kind `game-data`
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FactCard {
    /// The term it is about
    pub term: String,
    pub title: String,
    pub text: String,
    pub url: Option<String>,
    pub attribution: Option<String>,
    /// What players read of its game data, when the card keeps it
    /// ([`crate::stats`]), with the glossary's names for what it names
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
}

/// The fields of a stored document the cards need; the source kind as
/// written, so kinds this build does not know still read
#[derive(Deserialize)]
struct CardDoc {
    source: String,
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    attribution: Option<String>,
    #[serde(default)]
    facts: Option<Facts>,
}

/// The term of a game key (`SakelienLarge`) in Lean's name table, or of an
/// English name (`Chum`, `Rush`, `The Griller` as `Griller`)
fn term_of<'a>(g: &'a Glossary, name: &str) -> Option<&'a Term> {
    let suffix = alloc::format!("/{name}");
    g.terms
        .iter()
        .find(|t| {
            t.from
                .iter()
                .any(|f| f.starts_with(crate::leanny::NAMES_SOURCE) && f.ends_with(suffix.as_str()))
        })
        .or_else(|| g.lookup(name))
        .or_else(|| g.lookup(name.strip_prefix("The ")?))
}

/// A card's summary with the glossary's names for what it names
fn summary_of(facts: &Facts, g: &Glossary) -> Summary {
    let mut s = stats::summary(facts);
    let named: Vec<(String, Named)> = s
        .named()
        .into_iter()
        .filter_map(|name| {
            let t = term_of(g, name)?;
            let names = t
                .forms
                .iter()
                .filter_map(|(lang, f)| Some((lang.clone(), f.first()?.clone())))
                .collect();
            Some((
                String::from(name),
                Named {
                    id: t.id.clone(),
                    names,
                },
            ))
        })
        .collect();
    s.names.extend(named);
    s
}

/// The fact cards among the stored documents (`<root>/docs/*.json`), each
/// with the term its title names: the name before ` (` (`Steelhead
/// (Salmonid, game data)`), else the first term the title mentions besides
/// Salmon Run itself, which every card is about (`Salmon Run hazard level
/// 40% ...` is Hazard Level's)
pub fn fact_cards(root: &Path, g: &Glossary) -> Vec<FactCard> {
    let Ok(entries) = std::fs::read_dir(root.join("docs")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        // Most documents are not cards: look for the kind's name before
        // parsing (the store writes documents pretty-printed, so not for
        // the compact `"source":"game-data"`)
        if !bytes.windows(9).any(|w| w == b"game-data") {
            continue;
        }
        let Ok(doc) = serde_json::from_slice::<CardDoc>(&bytes) else {
            continue;
        };
        if doc.source != "game-data" {
            continue;
        }
        let name = doc.title.split(" (").next().unwrap_or(&doc.title);
        let term = g.lookup(name).or_else(|| {
            g.find_in(&doc.title)
                .into_iter()
                .find(|t| t.id != "salmon-run")
        });
        if let Some(term) = term {
            out.push(FactCard {
                term: term.id.clone(),
                summary: doc.facts.as_ref().map(|f| summary_of(f, g)),
                title: doc.title,
                text: doc.text,
                url: doc.url,
                attribution: doc.attribution,
            });
        }
    }
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// Whether an expert note is about `term`: its terms or its tags name
/// the term's id (a tag may also be its English name, `Steel Eel`)
pub fn note_is_about(note: &Note, term: &Term) -> bool {
    let slug = crate::tables::slug(english(term));
    note.terms.contains(&term.id)
        || note
            .tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case(&term.id) || crate::tables::slug(t) == slug)
}

/// Whether `text` names `term` (an official name or an approved alias; a
/// Latin one as a whole word), for the bank questions about it
pub fn names_term(text: &str, term: &Term) -> bool {
    let names: Vec<&str> = term.names().collect();
    first_mention(text, &names).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expert::Expert;
    use crate::game::Game;
    use crate::glossary::Relation;

    fn term(id: &str, kind: Option<&str>, related: Option<(RelationKind, &str)>) -> Term {
        Term {
            id: id.into(),
            forms: BTreeMap::from([(String::from("en"), alloc::vec![id.replace('-', " ")])]),
            kind: kind.map(String::from),
            related: related.map(|(kind, t)| Relation {
                kind,
                term: t.into(),
                name: t.into(),
            }),
            ..Term::default()
        }
    }

    #[test]
    fn sections_follow_kinds_ids_and_relations() {
        let mut g = Glossary::seed();
        g.terms.extend([
            term(
                "flyfish-missiles",
                Some("attack"),
                Some((RelationKind::PartOf, "flyfish")),
            ),
            term(
                "main-strafe",
                Some("technique"),
                Some((RelationKind::RelatedTo, "inertia-cancel")),
            ),
            term(
                "egg-toss",
                Some("technique"),
                Some((RelationKind::RelatedTo, "egg-run")),
            ),
            term("squid-roll", Some("technique"), None),
            term("3-1-split", Some("technique"), None),
            term(
                "egg-runner",
                Some("role"),
                Some((RelationKind::RelatedTo, "egg-run")),
            ),
            term("ink-lock", Some("mechanic"), None),
        ]);
        g.terms.iter_mut().find(|t| t.id == "flyfish").unwrap().kind = Some("boss".into());
        let of = |id: &str| section(&g, g.lookup(id).unwrap());
        assert_eq!(of("flyfish-missiles"), Section::Bosses);
        assert_eq!(of("main-strafe"), Section::Movement);
        assert_eq!(of("inertia-cancel"), Section::Movement);
        assert_eq!(of("egg-toss"), Section::Eggs);
        assert_eq!(of("squid-roll"), Section::Movement);
        assert_eq!(of("3-1-split"), Section::Strategy);
        assert_eq!(of("egg-runner"), Section::Strategy);
        assert_eq!(of("cohozuna"), Section::Kings);
        assert_eq!(of("rush"), Section::Events);
        assert_eq!(of("high-tide"), Section::Events);
        assert_eq!(of("ink-lock"), Section::Other);
    }

    fn comment(text: &str, url: &str, vod: &str, date: &str, t_s: Option<f32>) -> ExpertComment {
        ExpertComment {
            url: url.into(),
            text: text.into(),
            expert: Expert {
                reviewer: "Reviewer".into(),
                date: date.parse().unwrap(),
                game: Game::S3,
                vod: vod.into(),
                video: String::new(),
                wave: None,
                timer_s: None,
                t_s,
            },
        }
    }

    #[test]
    fn finds_mentions_and_ranks_quotes() {
        let g = Glossary::seed();
        let comments = [
            comment(
                "(replying to A: \"the steelhead\")\nNice inertia cancel there, keep strafing",
                "u1",
                "v1",
                "2024-01-01",
                None,
            ),
            comment(
                "Kill the Steelhead before it throws the bomb at the basket.",
                "u2",
                "v1",
                "2024-02-01",
                Some(12.0),
            ),
            comment("Steelhead again", "u2", "v1", "2024-02-01", None),
            comment(
                "Steelhead from far, then back to the basket",
                "u3",
                "v2",
                "2023-01-01",
                None,
            ),
        ];
        let ids = BTreeSet::from(["steelhead", "inertia-cancel"]);
        let found = mentions(&g, &ids, &comments);
        // The reply context is not the comment's own words
        assert_eq!(found["steelhead"], [1, 2, 3]);
        assert_eq!(found["inertia-cancel"], [0]);
        assert_eq!(messages(&found["steelhead"], &comments), 2);
        let ranked = rank_quotes(&found["steelhead"], &comments, &|c| c.expert.t_s.is_some());
        assert_eq!(ranked, [1, 3]);
    }

    #[test]
    fn snippets_center_on_the_mention() {
        let text = alloc::format!(
            "{} Stinger on the shore {}",
            "word ".repeat(60),
            "end ".repeat(60)
        );
        let s = snippet(&text, &["Stinger"], 80);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert!(s.contains("Stinger on the shore"), "{s}");
        assert!(s.chars().count() <= 82);
        assert_eq!(snippet("short one", &["x"], 80), "short one");
    }

    #[test]
    fn scope_keeps_salmon_run_and_discussed_weapons() {
        let seed_g = Glossary::seed();
        let seed: BTreeSet<&str> = seed_g.terms.iter().map(|t| t.id.as_str()).collect();
        let user = UserGlossary::default();
        let s3 = [String::from("S3")];
        let mut kit = term("tentatek-splattershot", Some("weapon"), None);
        kit.from = alloc::vec!["inbox/x/weapon3.php#Tentatek Splattershot".into()];
        assert!(candidate(&kit, &seed_g, &seed, &user, &s3));
        assert!(!in_scope(&kit, &seed, &user, 0));
        assert!(in_scope(&kit, &seed, &user, 3));
        // Every Salmon Run weapon of Lean's datamine, discussed or not
        let mut coop = term("undercover-brella", Some("weapon"), None);
        coop.from = alloc::vec![
            "inbox/x/weapon3.php#Undercover Brella".into(),
            "leanny:names#CommonMsg/Weapon/WeaponName_Main/Shelter_Compact_Coop".into(),
        ];
        assert!(candidate(&coop, &seed_g, &seed, &user, &s3));
        assert!(in_scope(&coop, &seed, &user, 0));
        let mut gear = term("headband", Some("gear"), None);
        gear.from = alloc::vec!["inbox/x/gear3.php#Headband".into()];
        assert!(!candidate(&gear, &seed_g, &seed, &user, &s3));
        let mut boss = term("chinook", Some("boss"), None);
        boss.from = alloc::vec!["inbox/x/salmon-boss3.php#Chinook".into()];
        assert!(candidate(&boss, &seed_g, &seed, &user, &s3));
        let mut any = term("any", Some("weapon"), None);
        any.forms
            .insert("en".into(), alloc::vec!["Any Weapon".into()]);
        assert!(!candidate(&any, &seed_g, &seed, &user, &s3));
        let mut grounds = term("grounds", Some("stage"), None);
        grounds
            .forms
            .insert("en".into(), alloc::vec!["Grounds".into()]);
        grounds.from = alloc::vec!["inbox/x/salmon-map2.php#Grounds".into()];
        let mut with_stage = seed_g.clone();
        with_stage
            .terms
            .iter_mut()
            .find(|t| t.id == "spawning-grounds")
            .unwrap()
            .kind = Some("stage".into());
        assert!(!candidate(&grounds, &with_stage, &seed, &user, &s3));
    }

    #[test]
    fn fact_cards_read_the_stored_documents() {
        let root = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-pedia-cards-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let docs = root.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        // The store writes documents pretty-printed
        let card = serde_json::json!({
            "id": "0123456789abcdef",
            "source": "game-data",
            "title": "Steelhead (Salmonid, game data)",
            "text": "# Steelhead\n\nHP 100",
            "url": "https://leanny.github.io/splat3/coop.html",
            "attribution": "Lean",
        });
        std::fs::write(
            docs.join("0123456789abcdef.json"),
            serde_json::to_vec_pretty(&card).unwrap(),
        )
        .unwrap();
        let other = serde_json::json!({
            "id": "fedcba9876543210",
            "source": "web",
            "title": "Steelhead tips",
            "text": "Aim at the bomb.",
        });
        std::fs::write(
            docs.join("fedcba9876543210.json"),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        let level = serde_json::json!({
            "id": "00112233445566ff",
            "source": "game-data",
            "title": "Salmon Run hazard level 40% (difficulty 200): wave and occurrence parameters (game data)",
            "text": "Rush speed",
            "facts": {"kind": "level", "key": "200", "version": "11.3.0",
                "params": {"EventRush": {"ZakoSpeedCoef": 2.0}, "EventDozer": {"DozerSpeedCoef": 1.5}}},
        });
        std::fs::write(
            docs.join("00112233445566ff.json"),
            serde_json::to_vec_pretty(&level).unwrap(),
        )
        .unwrap();
        let cards = fact_cards(&root, &Glossary::seed());
        assert_eq!(cards.len(), 2, "{cards:?}");
        assert_eq!(cards[0].term, "hazard-level");
        // Its summary names the occurrences with the glossary's terms
        let summary = cards[0].summary.as_ref().unwrap();
        assert_eq!(summary.names["Rush"].id, "rush");
        assert_eq!(summary.names["The Griller"].id, "griller");
        assert_eq!(summary.names["Rush"].names["ja"], "ラッシュ");
        let cards: Vec<FactCard> = cards.into_iter().skip(1).collect();
        assert!(cards[0].summary.is_none());
        assert_eq!(cards[0].term, "steelhead");
        assert_eq!(cards[0].title, "Steelhead (Salmonid, game data)");
        assert_eq!(cards[0].attribution.as_deref(), Some("Lean"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn notes_and_questions_about_a_term() {
        let g = Glossary::seed();
        let mut note = Note::new(
            "Which way does the Drizzler jump?",
            "Away from the shooter.",
        );
        note.tags = alloc::vec!["bosses".into(), "Steel Eel".into()];
        note.terms = alloc::vec!["drizzler".into()];
        assert!(note_is_about(&note, g.lookup("drizzler").unwrap()));
        assert!(note_is_about(&note, g.lookup("steel-eel").unwrap()));
        assert!(!note_is_about(&note, g.lookup("maws").unwrap()));
        let eel = g.lookup("steel-eel").unwrap();
        assert!(names_term("Where do you fight a Steel Eel?", eel));
        assert!(!names_term("Steel Eels-ish", g.lookup("maws").unwrap()));
    }
}
