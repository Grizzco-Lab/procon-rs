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
//! aliases = [{ lang = "en", text = "bomb guy", note = "from the bomb it grows" }]
//! ```
//!
//! A term has official names per language ([`Term::forms`]) and aliases:
//! the slang and abbreviations players use ([`Alias`]), each with its
//! language, a note on its origin or use, where it came from and whether it
//! is approved. Approved aliases count as names: [`Glossary::lookup`] and
//! [`Glossary::find_in`] find a term by them. What the user teaches or
//! approves is kept apart from the generated glossary ([`crate::slang`]).
//!
//! Retrieval finds the terms used in a query and adds their other-language
//! official names ([`Glossary::expand`]), so a Japanese question also
//! matches English notes; prompts list the matched terms
//! ([`Glossary::prompt_lines`], slang as `alias → official`) so the model
//! uses the community's names and resolves its slang.
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

/// Where an alias came from
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasSource {
    /// Written in a glossary file (the seed or the data folder's own)
    #[default]
    Seed,
    /// Taught by the user
    User,
    /// Proposed by the model from the knowledge base ([`crate::slang`])
    Suggested,
    /// Brought by an import
    Imported,
}

/// Whether an alias is in use
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasStatus {
    /// A name of its term: looked up, found in text, listed in prompts
    #[default]
    Approved,
    /// A suggestion waiting for the user
    Pending,
    /// A suggestion the user turned down; kept so it is not proposed again
    Rejected,
}

/// A name players use for a term besides its official ones: slang, an
/// abbreviation, a nickname
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Alias {
    /// The alias as written (`G Roller`)
    pub text: String,
    /// Language code (`zh`, `en`, `ja`, ...)
    pub lang: String,
    /// Its origin or how it is used
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default)]
    pub source: AliasSource,
    #[serde(default)]
    pub status: AliasStatus,
}

/// One term
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Term {
    /// Stable id (`steelhead`)
    pub id: String,
    /// Short English definition; empty for imported names
    #[serde(default)]
    pub definition: String,
    /// Official names per language code, the main one first; a regional
    /// variant (`en-GB`, `es-MX`) holds only the names that differ from its
    /// language's. A term of player jargon, with no name in the game, has
    /// descriptive ones here.
    pub forms: BTreeMap<String, Vec<String>>,
    /// Slang and abbreviations; only approved ones are names of the term
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<Alias>,
    /// Where imported names came from: `<file>#<key>`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
    /// What it is (`boss`, `stage`, `weapon`, ...), when an import says
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The game its names belong to (`S3`, `S2`, `S1`), when an import
    /// says; the newest game's import comes first, so an older game's name
    /// never leads
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
}

impl Term {
    /// The main name in `lang`, if the glossary has one
    pub fn name(&self, lang: &str) -> Option<&str> {
        self.forms.get(lang)?.first().map(String::as_str)
    }

    /// Approved aliases
    pub fn approved(&self) -> impl Iterator<Item = &Alias> {
        self.aliases
            .iter()
            .filter(|a| a.status == AliasStatus::Approved)
    }

    /// Every name: official ones, then approved aliases
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.forms
            .values()
            .flatten()
            .map(String::as_str)
            .chain(self.approved().map(|a| a.text.as_str()))
    }

    /// Whether `text` is one of its names (official or an approved alias),
    /// ignoring case
    pub fn has_name(&self, text: &str) -> bool {
        let text = key(text);
        self.names().any(|n| key(n) == text)
    }

    /// Adds an alias unless it is an official name of the term; an alias
    /// of the same text and language is replaced. Returns whether it was
    /// added.
    pub fn add_alias(&mut self, alias: Alias) -> bool {
        let k = key(&alias.text);
        if k.is_empty() || self.forms.values().flatten().any(|f| key(f) == k) {
            return false;
        }
        self.aliases
            .retain(|a| !(key(&a.text) == k && a.lang == alias.lang));
        self.aliases.push(alias);
        true
    }
}

/// A set of terms
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Glossary {
    /// The terms, in file order
    #[serde(rename = "term", default)]
    pub terms: Vec<Term>,
}

/// A name or id as compared: trimmed, lowercase
fn key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Where a glossary's terms are, by id, English name and any name, for
/// [`Glossary::merge`] and [`Glossary::conflicts`]
struct Index {
    ids: BTreeMap<String, usize>,
    english: BTreeMap<String, usize>,
    any: BTreeMap<String, usize>,
}

impl Index {
    fn new(terms: &[Term]) -> Self {
        let mut index = Index {
            ids: BTreeMap::new(),
            english: BTreeMap::new(),
            any: BTreeMap::new(),
        };
        for (i, t) in terms.iter().enumerate() {
            index.ids.entry(key(&t.id)).or_insert(i);
            for (lang, forms) in &t.forms {
                for f in forms {
                    index.add(lang, f, i);
                }
            }
        }
        index
    }

    /// Notes a name of term `i`, the first term with it keeping it
    fn add(&mut self, lang: &str, form: &str, i: usize) {
        if lang == "en" {
            self.english.entry(key(form)).or_insert(i);
        }
        self.any.entry(key(form)).or_insert(i);
    }

    /// The term `t` belongs to: the one with an English name of it; without
    /// English names, the one with its id, else one sharing any name
    fn find(&self, t: &Term) -> Option<usize> {
        let english: Vec<&String> = t.forms.get("en").into_iter().flatten().collect();
        if !english.is_empty() {
            return english
                .iter()
                .find_map(|f| self.english.get(&key(f)).copied());
        }
        if let Some(&i) = self.ids.get(&key(&t.id)) {
            return Some(i);
        }
        t.forms
            .values()
            .flatten()
            .find_map(|f| self.any.get(&key(f)).copied())
    }
}

/// An alias's line under its term in a prompt: `  - <lang> slang: <alias>
/// → <official> (en: <English name>): <note>`; the official name is the
/// one in the alias's language, else the English one, else the id
fn alias_line(t: &Term, a: &Alias) -> String {
    let english = t.name("en");
    let official = t.name(&a.lang).or(english).unwrap_or(&t.id);
    let mut line = alloc::format!("  - {} slang: {} → {official}", a.lang, a.text);
    if let Some(en) = english.filter(|en| *en != official) {
        line.push_str(&alloc::format!(" (en: {en})"));
    }
    if !a.note.is_empty() {
        line.push_str(": ");
        line.push_str(&a.note);
    }
    line.push('\n');
    line
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

    /// The term with this id or name (ignoring case): an official name
    /// first, else an approved alias
    pub fn lookup(&self, name: &str) -> Option<&Term> {
        let name = key(name);
        self.terms
            .iter()
            .find(|t| t.id == name || t.forms.values().flatten().any(|f| key(f) == name))
            .or_else(|| {
                self.terms
                    .iter()
                    .find(|t| t.approved().any(|a| key(&a.text) == name))
            })
    }

    /// Up to `limit` terms with an id or name (official or an approved
    /// alias) containing `query`, ignoring case: whole names first, then
    /// names starting with it, then the rest, each in glossary order
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Term> {
        let q = key(query);
        if q.is_empty() {
            return Vec::new();
        }
        let rank = |t: &Term| -> Option<u8> {
            core::iter::once(t.id.as_str())
                .chain(t.names())
                .filter_map(|n| {
                    let n = key(n);
                    if n == q {
                        Some(0)
                    } else if n.starts_with(&q) {
                        Some(1)
                    } else {
                        n.contains(&q).then_some(2)
                    }
                })
                .min()
        };
        let mut found: Vec<(u8, usize)> = self
            .terms
            .iter()
            .enumerate()
            .filter_map(|(i, t)| rank(t).map(|r| (r, i)))
            .collect();
        found.sort();
        found
            .into_iter()
            .take(limit)
            .map(|(_, i)| &self.terms[i])
            .collect()
    }

    /// Terms mentioned in `text` by an official name or an approved alias,
    /// in order of first mention. Latin names must stand as whole words;
    /// longer matches win over names inside them (`キンシャケ` is Goldie,
    /// not Chum), and an official name over an alias as long.
    pub fn find_in(&self, text: &str) -> Vec<&Term> {
        let hay = text.to_lowercase();
        // (start, end, is an alias, term index) of every occurrence
        let mut hits: Vec<(usize, usize, bool, usize)> = Vec::new();
        for (ti, term) in self.terms.iter().enumerate() {
            let official = term.forms.values().flatten().map(|f| (f.as_str(), false));
            let aliases = term.approved().map(|a| (a.text.as_str(), true));
            for (name, alias) in official.chain(aliases) {
                let needle = name.to_lowercase();
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
                        hits.push((start, end, alias, ti));
                    }
                }
            }
        }
        // Longest first, official before alias, dropping matches that
        // overlap one taken before
        hits.sort_by_key(|&(s, e, alias, _)| (core::cmp::Reverse(e - s), s, alias));
        let mut taken: Vec<(usize, usize, bool, usize)> = Vec::new();
        for h in hits {
            if taken.iter().all(|t| h.1 <= t.0 || h.0 >= t.1) {
                taken.push(h);
            }
        }
        taken.sort_by_key(|&(s, _, _, _)| s);
        let mut out: Vec<&Term> = Vec::new();
        for (_, _, _, ti) in taken {
            let term = &self.terms[ti];
            if !out.iter().any(|t| t.id == term.id) {
                out.push(term);
            }
        }
        out
    }

    /// `text` followed by every official name of the terms it mentions, so
    /// the query embedding also points at documents in other languages
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

    /// Adds imported terms: a term with an English name (ignoring case) of
    /// one already here adds its names and origin to it, as does a term
    /// without English names that has the id of one or shares a name in any
    /// language; others are appended, with their id made unique. English
    /// decides because localized names are shared more often (Splattershot
    /// and the Shooter class are both "Lanzatintas" in Spanish).
    pub fn merge(&mut self, terms: &[Term]) {
        let mut index = Index::new(&self.terms);
        for t in terms {
            let i = match index.find(t) {
                Some(i) => i,
                None => {
                    let mut new = Term {
                        forms: BTreeMap::new(),
                        aliases: Vec::new(),
                        from: Vec::new(),
                        ..t.clone()
                    };
                    let mut n = 1;
                    while index.ids.contains_key(&key(&new.id)) {
                        n += 1;
                        new.id = alloc::format!("{}-{n}", t.id);
                    }
                    index.ids.insert(key(&new.id), self.terms.len());
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
                    index.add(lang, f, i);
                }
            }
            for a in &t.aliases {
                term.add_alias(a.clone());
            }
            for f in &t.from {
                if !term.from.contains(f) {
                    term.from.push(f.clone());
                }
            }
            if term.kind.is_none() {
                term.kind = t.kind.clone();
            }
            if term.game.is_none() {
                term.game = t.game.clone();
            }
        }
    }

    /// Names of `terms` that differ from this glossary's: for each imported
    /// term that [`Glossary::merge`] would fold into a term here, the
    /// languages where its main name is none of the term's official names
    /// here, as lines `<id>: <lang> "<here>" here, "<there>" in <source>`
    /// (the main name here; the source is the term's first origin). An
    /// imported name that is only an alias here is a difference: the
    /// import's names are official.
    pub fn conflicts(&self, terms: &[Term]) -> Vec<String> {
        let index = Index::new(&self.terms);
        let mut out = Vec::new();
        for t in terms {
            let Some(ours) = index.find(t).map(|i| &self.terms[i]) else {
                continue;
            };
            for (lang, forms) in &t.forms {
                let (Some(here), Some(there)) = (ours.name(lang), forms.first()) else {
                    continue;
                };
                let known = ours.forms[lang].iter().any(|f| key(f) == key(there));
                if !known {
                    let source = t
                        .from
                        .first()
                        .map_or("the import", |f| f.split('#').next().unwrap_or(f));
                    out.push(alloc::format!(
                        "{}: {lang} \"{here}\" here, \"{there}\" in {source}",
                        ours.id
                    ));
                }
            }
        }
        out
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

    /// One line per term for a prompt: its official names and definition,
    /// with the `lang` name first when given; then a line per approved
    /// alias, `<lang> slang: <alias> → <official> (en: <English>): <note>`
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
            // What an import says it is; the game only when not the current one
            let tags: Vec<String> = t
                .kind
                .iter()
                .cloned()
                .chain(
                    t.game
                        .iter()
                        .filter(|g| *g != "S3")
                        .map(|g| match g.as_str() {
                            "S2" => String::from("Splatoon 2"),
                            "S1" => String::from("Splatoon 1"),
                            g => String::from(g),
                        }),
                )
                .collect();
            if !tags.is_empty() {
                out.push_str(&alloc::format!(" [{}]", tags.join(", ")));
            }
            out.push('\n');
            for a in t.approved() {
                out.push_str(&alias_line(t, a));
            }
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
    fn searches_names_as_typed() {
        let g = Glossary::seed();
        let ids =
            |q: &str| -> Vec<String> { g.search(q, 3).iter().map(|t| t.id.clone()).collect() };
        // A whole name first, then names starting with it, then the rest
        assert_eq!(ids("Maws"), ["maws"]);
        assert_eq!(ids("gri"), ["grizzco", "griller", "grizzco-roller"]);
        assert_eq!(ids("熊刷"), ["grizzco-roller"]);
        assert!(ids(" ").is_empty());
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
    fn finds_player_jargon() {
        let g = Glossary::seed();
        let ids =
            |text: &str| -> Vec<String> { g.find_in(text).iter().map(|t| t.id.clone()).collect() };
        assert_eq!(ids("惯性取消搬蛋快"), ["inertia-cancel", "egg-run"]);
        assert_eq!(
            ids("我刚拿的熊刷，不应该上柱子拍的"),
            ["grizzco-roller", "fish-stick"]
        );
        assert_eq!(
            ids("小枪可以优先出差回收一些外围蛋，但不要待太久卡新一波怪"),
            ["short-range-weapon", "fetch-eggs", "outer-eggs"]
        );
        assert_eq!(ids("我还剩一个镭射"), ["killer-wail-51"]);
        assert_eq!(
            g.lookup("Killer Wail 5.1").unwrap().name("zh"),
            Some("喇叭镭射5.1ch")
        );
        // The longest Chinese name wins: a Goldie is not a Chum
        assert_eq!(ids("金鲑鱼掉金鲑鱼卵"), ["goldie", "golden-egg"]);
        // The slang the player gave
        assert_eq!(ids("鬼坝和破船"), ["spawning-grounds", "marooners-bay"]);
        assert_eq!(ids("喇叭和雷神"), ["sploosh-o-matic"]);
        assert_eq!(g.lookup("雷神").unwrap().id, "sploosh-o-matic");
        assert_eq!(g.lookup("小绿").unwrap().name("zh"), Some("斯普拉射击枪"));
        assert_eq!(ids("回筐边，家里没人"), ["egg-basket"]);
        assert_eq!(g.lookup("蛋筐").unwrap().name("en"), Some("Egg Basket"));
        let roller = g.lookup("熊刷").unwrap();
        assert_eq!(roller.name("zh"), Some("熊先生印章滚筒"));
        let lines = Glossary::prompt_lines(&[roller], Some("en"));
        assert!(
            lines.contains(
                "\n  - zh slang: 熊刷 → 熊先生印章滚筒 (en: Grizzco Roller): 'bear brush'\n"
            ),
            "{lines}"
        );
        assert!(
            lines.contains("\n  - en slang: G Roller → Grizzco Roller\n"),
            "{lines}"
        );
        // Aliases stay out of the expansion
        assert!(!g.expand("Grizzco Roller").contains("G Roller"));
        assert!(g.expand("熊刷").contains("Grizzco Roller"));
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
        // Without English names the id decides; with an English name that
        // no term has, a new term with an id of its own
        assert_eq!(g.terms.len(), size + 1);
        assert_eq!(g.lookup("Maws DE").unwrap().id, "maws");
        assert_eq!(g.lookup("Something else").unwrap().id, "maws-2");
        assert!(g.languages()["fr"] >= 1);
        let line = Glossary::prompt_lines(&[g.lookup("maws-2").unwrap()], None);
        assert_eq!(line, "- maws-2 (en: Something else)\n");
    }

    #[test]
    fn tags_conflicts_and_older_games() {
        let mut g = Glossary::seed();
        let imported: Glossary = Glossary::parse(
            r#"
            [[term]]
            id = "steelhead"
            forms = { en = ["Steelhead"], zh = ["炸弹鱼"], ja = ["バクダン"] }
            from = ["inbox/s.zip/messages/*/salmon-boss3.php#Steelhead"]
            kind = "boss"
            game = "S3"
            [[term]]
            id = "spawning-grounds"
            forms = { en = ["Spawning Grounds"], zh = ["鲑鱼坝"] }
            from = ["inbox/s.zip/messages/*/map3.php#Spawning Grounds"]
            kind = "stage"
            game = "S3"
            [[term]]
            id = "lost-outpost"
            forms = { en = ["Lost Outpost"], ja = ["海上集落シャケト場"] }
            kind = "stage"
            game = "S2"
            "#,
        )
        .unwrap();
        // Only the stage's Chinese name differs from the seed's
        assert_eq!(
            g.conflicts(&imported.terms),
            ["spawning-grounds: zh \"鲑坝\" here, \"鲑鱼坝\" in inbox/s.zip/messages/*/map3.php"]
        );
        g.merge(&imported.terms);
        let grounds = g.lookup("Spawning Grounds").unwrap();
        // Both names stay, the seed's first
        assert_eq!(grounds.forms["zh"], ["鲑坝", "鲑鱼坝"]);
        assert_eq!(grounds.kind.as_deref(), Some("stage"));
        let s2 = g.lookup("Lost Outpost").unwrap();
        assert_eq!(s2.game.as_deref(), Some("S2"));
        assert_eq!(
            Glossary::prompt_lines(&[s2], None),
            "- lost-outpost (en: Lost Outpost; ja: 海上集落シャケト場) [stage, Splatoon 2]\n"
        );
        let s3 = Glossary::prompt_lines(&[grounds], Some("zh"));
        assert!(
            s3.ends_with(
                "Salmon Run stage. [stage]\n  - zh slang: 鬼坝 → 鲑坝 (en: Spawning Grounds): \
                 a play on 鲑 (guī) as 鬼 (guǐ, 'ghost')\n"
            ),
            "{s3}"
        );
        // The seed's own terms carry no tags
        assert!(!Glossary::prompt_lines(&[g.lookup("Maws").unwrap()], None).contains('['));
    }

    #[test]
    fn merges_by_english_name_only() {
        let mut g = Glossary::default();
        let imported: Glossary = Glossary::parse(
            r#"
            [[term]]
            id = "shooters"
            forms = { en = ["Shooters"], es = ["Lanzatintas"], ja = ["シューター"] }
            [[term]]
            id = "splattershot"
            forms = { en = ["Splattershot"], es = ["Lanzatintas"], ja = ["スプラシューター"] }
            [[term]]
            id = "grounds"
            forms = { en = ["Grounds"], zh = ["鲑坝"] }
            [[term]]
            id = "spawning-grounds"
            forms = { en = ["Spawning Grounds"], zh = ["鲑坝"] }
            [[term]]
            id = "splattershot-2"
            forms = { en = ["SPLATTERSHOT"], fr = ["Liquidateur"] }
            [[term]]
            id = "no-english"
            forms = { ja = ["スプラシューター"], ko = ["스플랫 슈터"] }
            "#,
        )
        .unwrap();
        g.merge(&imported.terms);
        let ids: Vec<&str> = g.terms.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            ["shooters", "splattershot", "grounds", "spawning-grounds"]
        );
        let shot = g.lookup("splattershot").unwrap();
        assert_eq!(shot.forms["fr"], ["Liquidateur"]);
        assert_eq!(shot.forms["ko"], ["스플랫 슈터"]);
        assert_eq!(shot.forms["en"], ["Splattershot"]);
        assert!(g.conflicts(&imported.terms).is_empty());
    }

    #[test]
    fn prompt_lines_put_target_first() {
        let g = Glossary::seed();
        let t = g.lookup("Maws").unwrap();
        let s = Glossary::prompt_lines(&[t], Some("ja"));
        assert!(
            s.starts_with("- maws (ja: モグラ; en: Maws; zh: 鼹鼠鱼)"),
            "{s}"
        );
    }
}
