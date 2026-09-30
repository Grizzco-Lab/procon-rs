//! Lean's Splatoon 3 datamine (<https://leanny.github.io>, by Lean,
//! @LeanYoshi): game data extracted from the game's files, published as
//! JSON on the site (the `Leanny/splat3` repository on GitHub) and as the
//! Eggstra Work scenario pages ([`crate::eggstra`]). `ingest` fetches
//! what matters for Salmon Run and turns it into fact cards: one document
//! per Salmonid, stage, Salmon Run weapon (with its parameters), special
//! and hazard-level configuration, plus one per Eggstra Work event and
//! wave; and a name table for the glossary (English, Japanese and
//! Simplified Chinese names by internal key), which also lets the assets
//! link icons named by those keys. [`items`] lists the Salmon Run weapons
//! and specials of the raw copies for the lab's pickers (the Studio's
//! Techniques panel), each with its picture on Lean's site
//! ([`icon_url`]), which the lab fetches into its local cache when shown.
//!
//! The data is Nintendo's, extracted and published by Lean without a
//! licence: it is fetched into the knowledge folder at run time
//! (`raw/leanny/`), never copied into the repository, and every card
//! names its origin ([`LICENSE`], [`ATTRIBUTION`]). Fetching is polite
//! (one request at a time through [`Fetcher`], with its delay) and
//! incremental: each file's ETag is kept in `raw/leanny/state.json`, and a
//! re-run asks with `If-None-Match`, so unchanged files answer 304 and the
//! cards are rebuilt only when something changed (or with `--refresh`).

use crate::crawl::Fetcher;
use crate::doc::{Document, SourceKind, doc_id};
use crate::eggstra::{self, Event, Events, Scenario, Shift};
use crate::game::Game;
use crate::glossary::{Glossary, Term};
use crate::ingest::{self, Meta, Sink};
use crate::stats::{self, FactKind, Facts};
use crate::tables::{Table, slug};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, Utc};
use core::fmt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Lean's site
pub const SITE: &str = "https://leanny.github.io";
/// Folder of the raw copies below `raw/`
pub const RAW: &str = "leanny";
/// The state file in it: each file's ETag and fetch time
pub const STATE_FILE: &str = "state.json";
/// What every card records as its terms of use
pub const LICENSE: &str = "No licence stated: Splatoon 3 game data © Nintendo, extracted and published by Lean (leanny.github.io); private study only";
/// Whom every card credits
pub const ATTRIBUTION: &str =
    "Lean (leanny.github.io, @LeanYoshi), Splatoon 3 datamine; game data © Nintendo";
/// The Salmon Run database page, cited by the Salmonid, stage, weapon,
/// special and level cards
pub const COOP_PAGE: &str = "https://leanny.github.io/splat3/coop.html";
/// Event pages are probed up to this number
pub const MAX_EVENTS: u8 = 40;
/// The language files read, by our language code
pub const LANGUAGES: [(&str, &str); 3] = [("en", "EUen"), ("ja", "JPja"), ("zh", "CNzh")];
/// Name categories in the language files
pub const ENEMY_NAMES: &str = "CommonMsg/Coop/CoopEnemy";
pub const STAGE_NAMES: &str = "CommonMsg/Coop/CoopStageName";
pub const WEAPON_NAMES: &str = "CommonMsg/Weapon/WeaponName_Main";
pub const SPECIAL_NAMES: &str = "CommonMsg/Weapon/WeaponName_Special";
/// The name table's source, its file in `terms/`
pub const NAMES_SOURCE: &str = "leanny:names";
/// Seconds between requests, when the caller gives none
pub const DEFAULT_DELAY_S: f32 = 1.5;

/// The last line of every card: what the data is and where it is from
pub fn provenance(version: &str) -> String {
    alloc::format!(
        "Splatoon 3 v{version} game data from Lean's datamine (leanny.github.io, thanks to Lean); the numbers are the game's own."
    )
}

/// A fact card: a game-data document with its origin, license and era;
/// the provenance line closes the text when a `version` is given, so the
/// facts lead
pub fn card(key: &str, url: &str, title: String, text: String, version: &str) -> Document {
    let text = if version.is_empty() {
        text
    } else {
        alloc::format!("{}\n\n{}\n", text.trim_end(), provenance(version))
    };
    let mut d = Document::new(SourceKind::GameData, key, title, text);
    d.url = Some(String::from(url));
    d.license = Some(String::from(LICENSE));
    d.attribution = Some(String::from(ATTRIBUTION));
    d.language = Some(String::from("en"));
    d.game = Some(Game::S3);
    d
}

/// A version folder's name as the game shows it (`1130` is 11.3.0, `099`
/// is 0.9.9)
pub fn version_name(folder: &str) -> String {
    let digits: Vec<char> = folder.chars().collect();
    match digits.len() {
        3 => alloc::format!("{}.{}.{}", digits[0], digits[1], digits[2]),
        4 => alloc::format!("{}{}.{}.{}", digits[0], digits[1], digits[2], digits[3]),
        _ => String::from(folder),
    }
}

// ------------------------------------------------------------ names

/// The names of the game's keys in our languages, from the language files
#[derive(Clone, Debug, Default)]
pub struct Names {
    /// Language code to the file's object (category to key to name)
    pub tables: BTreeMap<String, Value>,
    /// Stage id to key (`9` to `Shakerail`), from `CoopSceneInfo`
    pub stages: BTreeMap<u64, String>,
}

impl Names {
    /// The name of `key` in `category` in `lang`
    pub fn get(&self, lang: &str, category: &str, key: &str) -> Option<&str> {
        self.tables.get(lang)?[category][key].as_str()
    }

    /// The English name
    pub fn en(&self, category: &str, key: &str) -> Option<&str> {
        self.get("en", category, key)
    }

    /// The names as a term's forms: every language that has one
    pub fn forms(&self, category: &str, key: &str) -> BTreeMap<String, Vec<String>> {
        LANGUAGES
            .iter()
            .filter_map(|(lang, _)| {
                let name = self.get(lang, category, key)?;
                Some((String::from(*lang), alloc::vec![String::from(name)]))
            })
            .collect()
    }

    /// `Steelhead (ja バクダン, zh 炸弹鱼)`, or the key when no name is known
    pub fn line(&self, category: &str, key: &str) -> String {
        let Some(en) = self.en(category, key) else {
            return String::from(key);
        };
        let others: Vec<String> = LANGUAGES
            .iter()
            .filter(|(lang, _)| *lang != "en")
            .filter_map(|(lang, _)| {
                Some(alloc::format!("{lang} {}", self.get(lang, category, key)?))
            })
            .collect();
        if others.is_empty() {
            String::from(en)
        } else {
            alloc::format!("{en} ({})", others.join(", "))
        }
    }

    /// The stage key of a stage id (`Shakerail`)
    pub fn stage_key(&self, id: u64) -> Option<String> {
        self.stages.get(&id).cloned()
    }
}

// ------------------------------------------------------------ fetching

/// What is known of a fetched file
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileState {
    /// Every ETag the servers gave for this content: GitHub Pages answers
    /// from several backends whose ETags differ (they are made of the
    /// file's time on that backend), so all of them go into
    /// `If-None-Match`; the newest last, at most [`ETAGS_KEPT`]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub etags: Vec<String>,
    /// Hash of the copy ([`content_hash`]): a 200 with the same content is
    /// no change either
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    pub fetched_at: DateTime<Utc>,
    /// The last status: 200 for a copy, 404 for a file that is not there
    pub status: u16,
}

/// ETags remembered per file
pub const ETAGS_KEPT: usize = 6;

/// A hash of a file's content (64-bit FNV-1a, hex), to tell a changed file
/// from one served with another ETag
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    alloc::format!("{h:016x}")
}

/// The state of every file fetched, by its path below `raw/leanny/`
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub files: BTreeMap<String, FileState>,
    /// The [`CARD_FORMAT`] of the cards stored last (0 before it existed)
    #[serde(default)]
    pub format: u32,
}

/// What the cards hold, raised when they change so that the next run
/// rebuilds them though no file changed: 1 text only, 2 with their
/// [`Facts`] and the numbers in players' units
pub const CARD_FORMAT: u32 = 2;

impl State {
    fn path(dir: &Path) -> PathBuf {
        dir.join(STATE_FILE)
    }

    /// The state in `dir`, empty without one
    pub fn load(dir: &Path) -> State {
        std::fs::read(Self::path(dir))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, dir: &Path) -> Result<()> {
        crate::store::write_atomic(&Self::path(dir), &serde_json::to_vec_pretty(self)?)
    }
}

/// What to do
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    /// List the files and what the raw copies would give; fetch nothing,
    /// store nothing
    pub dry_run: bool,
    /// Build the cards again even when no file changed
    pub refresh: bool,
    /// Also the parameter files of the Salmon Run weapons and specials
    /// (one per weapon or special and its parent table, about 180 more
    /// requests the first time)
    pub weapons: bool,
}

/// What a run did
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Files fetched anew (200)
    pub fetched: usize,
    /// Files the server still had as we do (304)
    pub unchanged: usize,
    /// Files answered 404 (the event page after the last)
    pub missing: usize,
    /// Cards stored
    pub cards: usize,
    /// Cards left as they were
    pub kept: usize,
    /// Scenarios read
    pub scenarios: usize,
    /// Events in the table
    pub events: usize,
    /// Terms in the name table
    pub terms: usize,
    /// The game version
    pub version: String,
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.version.is_empty() {
            return write!(
                f,
                "no data fetched yet ({} files fetched this run)",
                self.fetched
            );
        }
        write!(
            f,
            "Splatoon 3 v{}: {} files fetched, {} unchanged; {} scenarios, {} events; {} cards stored, {} kept; {} names",
            self.version,
            self.fetched,
            self.unchanged,
            self.scenarios,
            self.events,
            self.cards,
            self.kept,
            self.terms
        )
    }
}

/// The files of the site, fetched conditionally and kept as raw copies
struct Site<'a> {
    fetcher: &'a mut Fetcher,
    dir: PathBuf,
    state: State,
    options: &'a Options,
    summary: Summary,
    /// For a dry run: each file and whether a copy is there
    plan: Vec<String>,
}

impl Site<'_> {
    /// The file at `url`, kept as `rel` below `raw/leanny/`: the raw copy
    /// when the server says it is current (or in a dry run), else the
    /// server's; `None` when the server has no such file (404)
    fn file(&mut self, url: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        let path = self.dir.join(rel);
        let copy = std::fs::read(&path).ok();
        let known = self.state.files.get(rel).cloned();
        if self.options.dry_run {
            let what = match (&copy, &known) {
                (Some(_), Some(k)) => alloc::format!("copy of {}", k.fetched_at.format("%Y-%m-%d")),
                (None, Some(k)) if k.status == 404 => String::from("not on the site"),
                _ => String::from("not fetched yet"),
            };
            self.plan.push(alloc::format!("{url}: {what}"));
            return Ok(copy);
        }
        // Every ETag seen for the copy, as one If-None-Match list
        let etags: Vec<String> = known
            .as_ref()
            .filter(|_| copy.is_some())
            .map(|k| k.etags.clone())
            .unwrap_or_default();
        let header = (!etags.is_empty()).then(|| etags.join(", "));
        let got = self.fetcher.fetch_if_changed(url, header.as_deref())?;
        if got.unchanged() {
            self.summary.unchanged += 1;
            return Ok(copy);
        }
        if got.status == 404 {
            self.summary.missing += 1;
            self.state.files.insert(
                String::from(rel),
                FileState {
                    etags: Vec::new(),
                    hash: String::new(),
                    fetched_at: Utc::now(),
                    status: 404,
                },
            );
            return Ok(None);
        }
        ensure!(got.ok(), "GET {url}: HTTP {}", got.status);
        let hash = content_hash(&got.body);
        let mut etags = etags;
        if let Some(tag) = &got.etag
            && !etags.contains(tag)
        {
            etags.push(tag.clone());
            if etags.len() > ETAGS_KEPT {
                etags.remove(0);
            }
        }
        // Another backend's ETag for the same content: nothing changed
        let same = copy.is_some() && known.as_ref().is_some_and(|k| k.hash == hash);
        if !same {
            crate::store::write_atomic(&path, &got.body)?;
            self.summary.fetched += 1;
        } else {
            self.summary.unchanged += 1;
        }
        self.state.files.insert(
            String::from(rel),
            FileState {
                etags,
                hash,
                fetched_at: Utc::now(),
                status: got.status,
            },
        );
        Ok(Some(got.body))
    }

    /// A file as JSON
    fn json(&mut self, url: &str, rel: &str) -> Result<Option<Value>> {
        match self.file(url, rel)? {
            Some(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).with_context(|| alloc::format!("in {rel}"))?,
            )),
            None => Ok(None),
        }
    }

    /// A file of the site by its path (`splat3/versions.json`)
    fn site_json(&mut self, rel: &str) -> Result<Option<Value>> {
        self.json(&alloc::format!("{SITE}/{rel}"), rel)
    }

    fn save(&self) -> Result<()> {
        self.state.save(&self.dir)
    }
}

/// The site's path of one of a version's tables (`WeaponInfoMain`)
fn mush_path(version: &str, name: &str) -> String {
    alloc::format!("splat3/data/mush/{version}/{name}.json")
}

/// The site's paths of the data files of a version
fn data_paths(version: &str) -> Vec<String> {
    let mush = |name: &str| mush_path(version, name);
    alloc::vec![
        mush("CoopEnemyInfo"),
        mush("CoopSceneInfo"),
        mush("WeaponInfoMain"),
        mush("WeaponInfoSpecial"),
        alloc::format!(
            "splat3/data/parameter/{version}/misc/spl__CoopLevelsConfig.spl__CoopLevelsConfig.json"
        ),
    ]
}

/// The game data of one version, as read
#[derive(Clone, Debug, Default)]
pub struct Data {
    /// The version folder (`1130`)
    pub folder: String,
    /// The version as shown (`11.3.0`)
    pub version: String,
    pub enemies: Vec<Value>,
    pub scenes: Vec<Value>,
    pub weapons: Vec<Value>,
    pub specials: Vec<Value>,
    /// `spl__CoopLevelsConfig`
    pub levels: Value,
    pub names: Names,
    /// Salmon Run weapon and special parameters by the row's `__RowId`,
    /// with its parent table merged in
    pub parameters: BTreeMap<String, Value>,
}

/// Deep-merges `over` into `base`: objects key by key, anything else
/// replaced
fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, o) => *b = o.clone(),
    }
}

/// Longest value written out; a longer one (a curve of hundreds of points)
/// is summarized, since a line without spaces that long cannot be chunked
pub const MAX_VALUE_CHARS: usize = 160;

/// `Section.Key: value` lines of a parameter table, `$type` and `$parent`
/// left out, arrays as JSON (long ones as their item count), a blank line
/// between the top-level sections so the chunker sees paragraphs
pub fn parameter_lines(value: &Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k.starts_with('$') {
                    continue;
                }
                let path = if prefix.is_empty() {
                    if !out.is_empty() {
                        out.push(String::new());
                    }
                    k.clone()
                } else {
                    alloc::format!("{prefix}.{k}")
                };
                parameter_lines(v, &path, out);
            }
        }
        v => {
            let text = v.to_string();
            if text.chars().count() > MAX_VALUE_CHARS {
                let n = v.as_array().map_or(0, Vec::len);
                out.push(alloc::format!("- {prefix}: ({n} values, left out)"));
            } else {
                out.push(alloc::format!("- {prefix}: {text}"));
            }
        }
    }
}

// ------------------------------------------------------------- cards

/// The rows of a table that name their row
fn rows(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

fn row_id(row: &Value) -> &str {
    row["__RowId"].as_str().unwrap_or_default()
}

/// The stage key of a `CoopSceneInfo` row (`Cop_Shakerail` is `Shakerail`)
fn stage_key(row: &Value) -> &str {
    row_id(row).strip_prefix("Cop_").unwrap_or(row_id(row))
}

/// The number of the field, if it is one
fn num(row: &Value, key: &str) -> Option<String> {
    let v = &row[key];
    (v.is_number() || v.is_boolean()).then(|| v.to_string())
}

/// A stem's file name in the parameter folder (`WeaponShooterNormal_Coop`
/// from a `Work/Actor/WeaponShooterNormal_Coop.engine__actor__ActorParam.gyml`
/// path or a `Work/Component/GameParameterTable/WeaponShooterNormal.game__GameParameterTable.gyml` one)
fn stem(path: &str) -> Option<&str> {
    path.rsplit('/').next()?.split('.').next()
}

/// The events (numbers) whose scenario uses the weapon key
fn events_with(scenarios: &[Scenario], events: &[Event], key: &str, specials: bool) -> Vec<String> {
    scenarios
        .iter()
        .filter(|s| {
            let list = if specials { &s.specials } else { &s.weapons };
            list.iter().any(|k| k == key)
        })
        .map(
            |s| match events.iter().find(|e| e.scenario == Some(s.number)) {
                Some(e) => alloc::format!("#{} ({})", e.number, e.start.date_naive()),
                None => alloc::format!("scenario {}", s.number),
            },
        )
        .collect()
}

/// The Salmonid cards, one per `CoopEnemyInfo` row
pub fn enemy_cards(data: &Data) -> Vec<Document> {
    let king_coefs = |key: &str| -> Vec<(u32, f64)> {
        data.levels["Levels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| {
                let coef = l[key].as_f64()?;
                let hazard = eggstra::hazard_percent(l["Difficulty"].as_u64().unwrap_or(0) as u32);
                Some((hazard, coef))
            })
            .collect()
    };
    let mut out = Vec::new();
    for row in rows(&Value::Array(data.enemies.clone())) {
        let key = row["Type"].as_str().unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let name = data.names.en(ENEMY_NAMES, key).unwrap_or(key);
        let category = row["Category"].as_str().unwrap_or("?");
        let category = match category {
            "Rare" => String::from("Rare (a Boss Salmonid)"),
            "Boss" => String::from("Boss (a King Salmonid)"),
            "Zako" => String::from("Zako (a lesser Salmonid)"),
            "EventRare" => String::from("EventRare (appears in known occurrences)"),
            other => String::from(other),
        };
        let title = alloc::format!("{name} (Salmonid, game data)");
        let mut text = alloc::format!(
            "# {name} (Salmonid)\n\nInternal key: {key} (CoopEnemyInfo). Names: {}.\n\n",
            data.names.line(ENEMY_NAMES, key)
        );
        text.push_str(&alloc::format!("- Category in the data: {category}\n"));
        if let Some(n) = num(&row, "ActorMax") {
            text.push_str(&alloc::format!(
                "- At most on the field at once (ActorMax): {n}\n"
            ));
        }
        if let Some(n) = num(&row, "HitIkuraNum") {
            text.push_str(&alloc::format!("- Power eggs per hit (HitIkuraNum): {n}\n"));
        }
        if let Some(n) = num(&row, "KillIkuraNum") {
            text.push_str(&alloc::format!(
                "- Power eggs on a kill (KillIkuraNum): {n}\n"
            ));
        }
        if let Some(n) = num(&row, "CalcAppearPriority") {
            text.push_str(&alloc::format!(
                "- Appear priority (CalcAppearPriority): {n}\n"
            ));
        }
        let coefs = king_coefs(&alloc::format!("{key}HPCoef"));
        if !coefs.is_empty() {
            let coefs: Vec<String> = coefs
                .iter()
                .map(|(hazard, coef)| alloc::format!("{hazard}%: {coef}"))
                .collect();
            text.push_str(&alloc::format!(
                "- King Salmonid HP coefficient by hazard level (CoopLevelsConfig): {}\n",
                coefs.join(", ")
            ));
        }
        let hp: Vec<String> = stats::SALMONID_HP
            .iter()
            .filter(|h| h.key == key)
            .map(|h| match h.part {
                Some(part) => alloc::format!("{} (its {part})", h.hp),
                None => alloc::format!("{}", h.hp),
            })
            .collect();
        if hp.is_empty() {
            text.push_str("\nHit points are not in this table; do not guess them.\n");
        } else {
            text.push_str(&alloc::format!(
                "- HP (not in Lean's data; from {}, in the damage units players see): {}\n",
                stats::HP_SOURCE,
                hp.join(", ")
            ));
        }
        let mut doc = card(
            &alloc::format!("leanny:enemy/{key}"),
            COOP_PAGE,
            title,
            text,
            &data.version,
        );
        doc.facts = Some(Box::new(Facts {
            kind: FactKind::Salmonid,
            key: String::from(key),
            versus: None,
            version: data.version.clone(),
            grizzco: None,
            events: Vec::new(),
            hp_coef: coefs,
            params: row.clone(),
        }));
        out.push(doc);
    }
    out
}

/// The stage cards, one per `CoopSceneInfo` row
pub fn stage_cards(data: &Data, scenarios: &[Scenario], events: &[Event]) -> Vec<Document> {
    let mut out = Vec::new();
    for row in &data.scenes {
        let key = stage_key(row);
        if key.is_empty() {
            continue;
        }
        let id = row["Id"].as_u64().unwrap_or(0);
        let name = data.names.en(STAGE_NAMES, key).unwrap_or(key);
        let big_run = row["IsBigRun"].as_bool().unwrap_or(false);
        let title = alloc::format!("{name} (Salmon Run stage, game data)");
        let mut text = alloc::format!(
            "# {name} (Salmon Run stage)\n\nInternal key: {} (CoopSceneInfo, stage id {id}). Names: {}.\n\n",
            row_id(row),
            data.names.line(STAGE_NAMES, key)
        );
        text.push_str(&alloc::format!(
            "- Big Run stage (a battle stage the Salmonids invade): {}\n",
            if big_run { "yes" } else { "no" }
        ));
        if let Some(n) = num(row, "Season") {
            text.push_str(&alloc::format!("- Season index in the data: {n}\n"));
        }
        let here: Vec<String> = scenarios
            .iter()
            .filter(|s| s.map == id)
            .map(
                |s| match events.iter().find(|e| e.scenario == Some(s.number)) {
                    Some(e) => alloc::format!("#{} ({})", e.number, e.start.date_naive()),
                    None => alloc::format!("scenario {}", s.number),
                },
            )
            .collect();
        if !here.is_empty() {
            text.push_str(&alloc::format!(
                "- Eggstra Work events here: {}\n",
                here.join(", ")
            ));
        }
        let mut doc = card(
            &alloc::format!("leanny:stage/{key}"),
            COOP_PAGE,
            title,
            text,
            &data.version,
        );
        doc.facts = Some(Box::new(Facts {
            kind: FactKind::Stage,
            key: String::from(row_id(row)),
            versus: None,
            version: data.version.clone(),
            grizzco: None,
            events: here,
            hp_coef: Vec::new(),
            params: row.clone(),
        }));
        out.push(doc);
    }
    out
}

/// The Parameters section of a Salmon Run weapon's or special's card, when
/// its parameter table was fetched, after what they mean in players' units
/// ([`stats::lines`]): a special's hold its damage to Salmonids
/// (`spl__BulletBlastParam.DistanceDamage`, ×10 in the data)
fn parameters_section(data: &Data, facts: &Facts, text: &mut String) {
    let Some(params) = data.parameters.get(&facts.key) else {
        return;
    };
    let readable = stats::lines(&stats::summary(facts));
    if !readable.is_empty() {
        text.push_str(&alloc::format!(
            "\n## In players' units (damage as the game shows it, a tenth of the data's; ink in percent of the tank; frames at 60 per second; distances in game units)\n\n{}\n",
            readable.join("\n")
        ));
    }
    let mut lines = Vec::new();
    parameter_lines(&params["GameParameters"], "", &mut lines);
    if !lines.is_empty() {
        text.push_str(&alloc::format!(
            "\n## Parameters of the Salmon Run form (GameParameterTable, the Salmon Run overrides merged into the weapon's table)\n\n{}\n",
            lines.join("\n")
        ));
    }
}

/// A Salmon Run weapon's or special's `GameParameters`, `null` when its
/// parameter table was not fetched
fn parameters_of(data: &Data, key: &str) -> Value {
    data.parameters
        .get(key)
        .map(|p| p["GameParameters"].clone())
        .unwrap_or_default()
}

/// The special cards, one per Salmon Run special (`WeaponInfoSpecial` rows
/// of type `Coop`), with the parameters when fetched
pub fn special_cards(data: &Data, scenarios: &[Scenario], events: &[Event]) -> Vec<Document> {
    let mut out = Vec::new();
    for row in &data.specials {
        if row["Type"].as_str() != Some("Coop") {
            continue;
        }
        let key = row_id(row);
        let base = key.strip_suffix("_Coop").unwrap_or(key);
        let name = data.names.en(SPECIAL_NAMES, key).unwrap_or(key);
        let title = alloc::format!("{name} (Salmon Run special, game data)");
        let mut text = alloc::format!(
            "# {name} (Salmon Run special)\n\nInternal key: {key} (WeaponInfoSpecial; the battle special is {base}). Names: {}.\n\n",
            data.names.line(SPECIAL_NAMES, key)
        );
        let used = events_with(scenarios, events, base, true);
        if !used.is_empty() {
            text.push_str(&alloc::format!(
                "- Eggstra Work events with it: {}\n",
                used.join(", ")
            ));
        }
        let facts = Facts {
            kind: FactKind::Special,
            key: String::from(key),
            versus: (base != key).then(|| String::from(base)),
            version: data.version.clone(),
            grizzco: None,
            events: used,
            hp_coef: Vec::new(),
            params: parameters_of(data, key),
        };
        parameters_section(data, &facts, &mut text);
        let mut doc = card(
            &alloc::format!("leanny:special/{key}"),
            COOP_PAGE,
            title,
            text,
            &data.version,
        );
        doc.facts = Some(Box::new(facts));
        out.push(doc);
    }
    out
}

/// The weapon cards, one per Salmon Run weapon (`WeaponInfoMain` rows of
/// type `Coop`), with the parameters when fetched
pub fn weapon_cards(data: &Data, scenarios: &[Scenario], events: &[Event]) -> Vec<Document> {
    let mut out = Vec::new();
    for row in &data.weapons {
        if row["Type"].as_str() != Some("Coop") {
            continue;
        }
        let key = row_id(row);
        let base = key.strip_suffix("_Coop").unwrap_or(key);
        let name = data.names.en(WEAPON_NAMES, key).unwrap_or(key);
        let grizzco = row["IsCoopRare"].as_bool().unwrap_or(false);
        // The battle weapon whose Salmon Run form this is
        let versus = data
            .weapons
            .iter()
            .find(|v| {
                v["WeaponInfoForCoop"]
                    .as_str()
                    .and_then(stem)
                    .is_some_and(|s| s == key)
            })
            .map(row_id);
        let title = alloc::format!("{name} (Salmon Run weapon, game data)");
        let mut text = alloc::format!(
            "# {name} (Salmon Run weapon)\n\nInternal key: {key} (WeaponInfoMain{}). Names: {}.\n\n",
            versus
                .map(|v| alloc::format!("; the battle weapon is {v}"))
                .unwrap_or_default(),
            data.names.line(WEAPON_NAMES, key)
        );
        text.push_str(&alloc::format!(
            "- Grizzco weapon (IsCoopRare): {}\n",
            if grizzco { "yes" } else { "no" }
        ));
        if let Some(n) = num(row, "Season") {
            text.push_str(&alloc::format!("- Season index in the data: {n}\n"));
        }
        let used = events_with(scenarios, events, base, false);
        if !used.is_empty() {
            text.push_str(&alloc::format!(
                "- Eggstra Work events with it: {}\n",
                used.join(", ")
            ));
        }
        let facts = Facts {
            kind: FactKind::Weapon,
            key: String::from(key),
            versus: versus.map(String::from),
            version: data.version.clone(),
            grizzco: Some(grizzco),
            events: used,
            hp_coef: Vec::new(),
            params: parameters_of(data, key),
        };
        parameters_section(data, &facts, &mut text);
        let mut doc = card(
            &alloc::format!("leanny:weapon/{key}"),
            COOP_PAGE,
            title,
            text,
            &data.version,
        );
        doc.facts = Some(Box::new(facts));
        out.push(doc);
    }
    out
}

/// The hazard-level cards, one per level of `CoopLevelsConfig`: the wave
/// parameters and the event parameters at that difficulty
pub fn level_cards(data: &Data) -> Vec<Document> {
    let mut out = Vec::new();
    for (i, level) in data.levels["Levels"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let difficulty = level["Difficulty"].as_u64().unwrap_or(0) as u32;
        let hazard = eggstra::hazard_percent(difficulty);
        let title = alloc::format!(
            "Salmon Run hazard level {hazard}% (difficulty {difficulty}): wave and occurrence parameters (game data)"
        );
        let mut text = alloc::format!(
            "# Salmon Run hazard level {hazard}% (difficulty {difficulty})\n\nLevel {i} of CoopLevelsConfig: how the waves and known occurrences are set at this difficulty (the game interpolates between levels).\n\n"
        );
        let mut lines = Vec::new();
        parameter_lines(level, "", &mut lines);
        text.push_str(&lines.join("\n"));
        text.push('\n');
        let mut doc = card(
            &alloc::format!("leanny:level/{difficulty}"),
            COOP_PAGE,
            title,
            text,
            &data.version,
        );
        doc.facts = Some(Box::new(Facts {
            kind: FactKind::Level,
            key: alloc::format!("{difficulty}"),
            versus: None,
            version: data.version.clone(),
            grizzco: None,
            events: Vec::new(),
            hp_coef: Vec::new(),
            params: level.clone(),
        }));
        out.push(doc);
    }
    out
}

/// The name table for the glossary: Salmonids, stages, Salmon Run weapons
/// (with their battle weapon's key) and specials, each with its names in
/// every language read and its keys as origins
pub fn name_table(data: &Data) -> Table {
    let mut terms = Vec::new();
    let mut strings = 0;
    let mut add = |category: &str, key: &str, kind: &str, more: &[&str]| {
        let forms = data.names.forms(category, key);
        let Some(en) = forms.get("en").and_then(|f| f.first()) else {
            return;
        };
        strings += forms.len();
        let mut from: Vec<String> = alloc::vec![alloc::format!("{NAMES_SOURCE}#{category}/{key}")];
        from.extend(
            more.iter()
                .map(|k| alloc::format!("{NAMES_SOURCE}#{category}/{k}")),
        );
        terms.push(Term {
            id: slug(en),
            definition: String::new(),
            forms,
            aliases: Vec::new(),
            from,
            kind: Some(String::from(kind)),
            game: Some(String::from("S3")),
            related: None,
        });
    };
    for row in &data.enemies {
        if let Some(key) = row["Type"].as_str() {
            add(ENEMY_NAMES, key, "boss", &[]);
        }
    }
    for row in &data.scenes {
        add(STAGE_NAMES, stage_key(row), "stage", &[]);
    }
    for row in &data.weapons {
        if row["Type"].as_str() != Some("Coop") {
            continue;
        }
        let key = row_id(row);
        let versus: Vec<&str> = data
            .weapons
            .iter()
            .filter(|v| {
                v["WeaponInfoForCoop"]
                    .as_str()
                    .and_then(stem)
                    .is_some_and(|s| s == key)
            })
            .map(row_id)
            .collect();
        add(WEAPON_NAMES, key, "weapon", &versus);
    }
    for row in &data.specials {
        if row["Type"].as_str() == Some("Coop") {
            add(SPECIAL_NAMES, row_id(row), "special", &[]);
        }
    }
    Table {
        id: doc_id(NAMES_SOURCE),
        source: String::from(NAMES_SOURCE),
        files: LANGUAGES
            .iter()
            .map(|(_, file)| alloc::format!("splat3/data/language/{file}.json"))
            .collect(),
        languages: LANGUAGES.iter().map(|(l, _)| String::from(*l)).collect(),
        strings,
        note: String::from(
            "Salmonids, stages, Salmon Run weapons and specials of Lean's Splatoon 3 datamine, by internal key",
        ),
        kind: None,
        game: Some(String::from("S3")),
        license: Some(String::from(LICENSE)),
        attribution: Some(String::from(ATTRIBUTION)),
        terms,
    }
}

// ------------------------------------------------------------ pickers

/// Where Lean's site keeps the game's pictures; an [`Item`]'s `icon` is a
/// path below it
pub const IMAGES: &str = "https://leanny.github.io/splat3/images";

/// A Salmon Run weapon or special as a picker lists it (the lab's
/// Techniques panel), from the raw copies of the newest version
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    /// `weapon` or `special`
    pub kind: &'static str,
    /// Lean's internal key, the row's `__RowId` (`Shooter_Normal_Coop`)
    pub key: String,
    /// The game's number for it (`Id`); the game lists them in its order
    pub id: u64,
    /// A Grizzco weapon (`IsCoopRare`)
    pub grizzco: bool,
    /// Its picture below [`IMAGES`] ([`icon_url`]): a weapon's flat icon,
    /// its battle form's (`weapon_flat/Path_Wst_Shooter_Normal_00.png`) or,
    /// for a Grizzco weapon, its own (`weapon_flat/Path_Wst_Roller_Bear.png`);
    /// a special's icon (`subspe/Wsp_SpJetpack00.png`)
    pub icon: String,
    /// Its term in the glossary, the one whose names came from Lean's table
    /// under this key ([`NAMES_SOURCE`]): its Pedia entry
    pub term: Option<String>,
    /// The term's main name in each of [`LANGUAGES`] it has
    pub names: BTreeMap<String, String>,
    /// The term's other names in those languages (official ones after the
    /// first) and its approved slang, for search
    pub search: Vec<String>,
}

/// The Salmon Run weapons (Grizzco's included) and specials of the newest
/// version in the raw copies (`<knowledge>/raw/leanny/`, fetched by
/// [`ingest`]): the weapons, then the specials, each in the game's order,
/// with their names and Pedia term from `glossary` (the store's, which has
/// Lean's name table merged in). Empty when nothing was fetched yet.
pub fn items(knowledge: &Path, glossary: &Glossary) -> Result<Vec<Item>> {
    let dir = knowledge.join("raw").join(RAW);
    let read = |rel: &str| -> Result<Value> {
        let path = dir.join(rel);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
            Err(e) => {
                return Err(e).with_context(|| alloc::format!("cannot read {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .with_context(|| alloc::format!("cannot parse {}", path.display()))
    };
    let versions = read("splat3/versions.json")?;
    let Some(folder) = versions
        .as_array()
        .and_then(|v| v.last())
        .and_then(Value::as_str)
    else {
        return Ok(Vec::new());
    };
    let coop = |row: &&Value| row["Type"].as_str() == Some("Coop");
    let weapons = rows(&read(&mush_path(folder, "WeaponInfoMain"))?);
    let specials = rows(&read(&mush_path(folder, "WeaponInfoSpecial"))?);
    let item = |kind, row: &Value, category, icon| {
        let key = row_id(row);
        let origin = alloc::format!("{NAMES_SOURCE}#{category}/{key}");
        let term = glossary.terms.iter().find(|t| t.from.contains(&origin));
        let names: BTreeMap<String, String> = LANGUAGES
            .iter()
            .filter_map(|(lang, _)| Some((String::from(*lang), String::from(term?.name(lang)?))))
            .collect();
        let official = term
            .into_iter()
            .flat_map(|t| LANGUAGES.iter().filter_map(|(lang, _)| t.forms.get(*lang)))
            .flatten();
        let slang = term.into_iter().flat_map(|t| t.approved().map(|a| &a.text));
        let mut search: Vec<String> = Vec::new();
        for name in official.chain(slang) {
            if !names.values().any(|n| n == name) && !search.contains(name) {
                search.push(name.clone());
            }
        }
        Item {
            kind,
            key: String::from(key),
            id: row["Id"].as_u64().unwrap_or(u64::MAX),
            grizzco: row["IsCoopRare"].as_bool().unwrap_or(false),
            icon,
            term: term.map(|t| t.id.clone()),
            names,
            search,
        }
    };
    let mut out = Vec::new();
    for row in weapons.iter().filter(coop) {
        let key = row_id(row);
        // The battle form's picture; a Grizzco weapon has only its own
        let form = weapons
            .iter()
            .find(|v| {
                v["WeaponInfoForCoop"]
                    .as_str()
                    .and_then(stem)
                    .is_some_and(|s| s == key)
            })
            .map(row_id)
            .unwrap_or(key.strip_suffix("_Coop").unwrap_or(key));
        let icon = alloc::format!("weapon_flat/Path_Wst_{form}.png");
        out.push(item("weapon", row, WEAPON_NAMES, icon));
    }
    for row in specials.iter().filter(coop) {
        let key = row_id(row);
        let icon = alloc::format!(
            "subspe/Wsp_{}00.png",
            key.strip_suffix("_Coop").unwrap_or(key)
        );
        out.push(item("special", row, SPECIAL_NAMES, icon));
    }
    out.sort_by_key(|i| (i.kind != "weapon", i.id));
    Ok(out)
}

/// The address on Lean's site of a picture a picker shows: an [`Item`]'s
/// `icon`, or a sub weapon's (`subspe/Wsb_Bomb_Splash00.png`); `None` for
/// any other path, so nothing else is ever fetched
pub fn icon_url(path: &str) -> Option<String> {
    let name = ["weapon_flat/Path_Wst_", "subspe/Wsp_", "subspe/Wsb_"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix))?
        .strip_suffix(".png")?;
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    plain.then(|| alloc::format!("{IMAGES}/{path}"))
}

// ------------------------------------------------------------- ingest

/// Everything a run builds
pub struct Built {
    pub data: Data,
    pub scenarios: Vec<Scenario>,
    pub events: Events,
    pub cards: Vec<Document>,
    pub names: Table,
}

/// Fetches the data files (conditionally), the event modules and
/// Inkipedia's list, and builds the cards; `None` when a dry run finds a
/// needed file not fetched yet
fn fetch_and_build(site: &mut Site) -> Result<Option<Built>> {
    let Some(versions) = site.site_json("splat3/versions.json")? else {
        return Ok(None);
    };
    let folder = versions
        .as_array()
        .and_then(|v| v.last())
        .and_then(|v| v.as_str())
        .context("versions.json names no version")?
        .to_string();
    let mut data = Data {
        version: version_name(&folder),
        folder: folder.clone(),
        ..Data::default()
    };
    let paths = data_paths(&folder);
    let mut tables = Vec::new();
    for rel in &paths {
        let Some(v) = site.site_json(rel)? else {
            return Ok(None);
        };
        tables.push(v);
    }
    let mut tables = tables.into_iter();
    data.enemies = rows(&tables.next().unwrap_or_default());
    data.scenes = rows(&tables.next().unwrap_or_default());
    data.weapons = rows(&tables.next().unwrap_or_default());
    data.specials = rows(&tables.next().unwrap_or_default());
    data.levels = tables.next().unwrap_or_default();
    for (lang, file) in LANGUAGES {
        let rel = alloc::format!("splat3/data/language/{file}.json");
        let Some(v) = site.site_json(&rel)? else {
            return Ok(None);
        };
        data.names.tables.insert(String::from(lang), v);
    }
    for row in &data.scenes {
        if let Some(id) = row["Id"].as_u64() {
            data.names.stages.insert(id, String::from(stage_key(row)));
        }
    }
    if site.options.weapons {
        let folder = folder.clone();
        // The row id and parameter file (from its actor) of every Salmon
        // Run weapon and special
        let coop: Vec<(String, String)> = data
            .weapons
            .iter()
            .chain(&data.specials)
            .filter(|r| r["Type"].as_str() == Some("Coop"))
            .filter_map(|r| {
                let actor = stem(r["SpecActor"].as_str()?)?;
                let file = alloc::format!(
                    "splat3/data/parameter/{folder}/weapon/{actor}.game__GameParameterTable.json"
                );
                Some((row_id(r).to_string(), file))
            })
            .collect();
        for (id, rel) in &coop {
            let Some(mut table) = site.site_json(rel)? else {
                continue;
            };
            // The Salmon Run table overrides its parent's; follow the
            // chain a few steps
            let mut chain = 0;
            while let Some(parent) = table["$parent"].as_str().and_then(stem).map(String::from) {
                chain += 1;
                let rel = alloc::format!(
                    "splat3/data/parameter/{folder}/weapon/{parent}.game__GameParameterTable.json"
                );
                let Some(mut base) = site.site_json(&rel)? else {
                    break;
                };
                let grand = base["$parent"].clone();
                merge(&mut base["GameParameters"], &table["GameParameters"]);
                table = base;
                table["$parent"] = grand;
                if chain >= 3 {
                    break;
                }
            }
            data.parameters.insert(String::from(id), table);
        }
    }
    let mut scenarios = Vec::new();
    for n in 1..=MAX_EVENTS {
        let rel = alloc::format!("eggstra_work/eggstrawork/EggstraWork{n:02}.js");
        let Some(bytes) = site.file(&eggstra::data_url(n), &rel)? else {
            break;
        };
        match Scenario::parse(n, &String::from_utf8_lossy(&bytes)) {
            Ok(s) => scenarios.push(s),
            Err(e) => log::warn!("Eggstra Work {n}: {e:#}"),
        }
    }
    let shifts: Vec<Shift> = match site.json(
        &eggstra::inkipedia_url(),
        "inkipedia/eggstra_work_shifts.json",
    ) {
        Ok(Some(v)) => eggstra::parse_shifts(v["parse"]["wikitext"].as_str().unwrap_or_default()),
        Ok(None) => Vec::new(),
        Err(e) => {
            log::warn!("Inkipedia's list of shifts: {e:#}; the events keep no dates this run");
            Vec::new()
        }
    };
    let names = data.names.clone();
    let stage_name = move |id: u64| {
        names
            .stage_key(id)
            .and_then(|k| names.en(STAGE_NAMES, &k).map(String::from))
    };
    let events = Events {
        built_at: Utc::now(),
        sources: alloc::vec![
            String::from(SITE),
            alloc::format!(
                "https://splatoonwiki.org/wiki/{}",
                eggstra::INKIPEDIA_PAGE.replace(' ', "_")
            ),
        ],
        events: eggstra::build_events(&shifts, &scenarios, &stage_name),
    };
    let mut cards = Vec::new();
    cards.extend(enemy_cards(&data));
    cards.extend(stage_cards(&data, &scenarios, &events.events));
    cards.extend(special_cards(&data, &scenarios, &events.events));
    cards.extend(weapon_cards(&data, &scenarios, &events.events));
    cards.extend(level_cards(&data));
    for s in &scenarios {
        let event = events.events.iter().find(|e| e.scenario == Some(s.number));
        cards.extend(eggstra::cards(s, event, &data.names, &data.version));
    }
    for e in events.events.iter().filter(|e| e.scenario.is_none()) {
        if !scenarios.iter().any(|s| s.number == e.number) {
            cards.push(eggstra::shift_card(e));
        }
    }
    let names = name_table(&data);
    Ok(Some(Built {
        data,
        scenarios,
        events,
        cards,
        names,
    }))
}

/// Fetches Lean's Salmon Run data and the Eggstra Work scenarios into
/// `<knowledge>/raw/leanny/`, writes the events table
/// (`corpus/eggstra_events.json`) and stores the fact cards and the name
/// table through `sink`. Unchanged files are not fetched again (ETags), and
/// when nothing changed and every card is stored, nothing is stored again
/// unless `meta.refresh`. A dry run fetches and stores nothing: it lists
/// the files with their state and what the raw copies would give.
pub fn ingest(
    sink: &mut dyn Sink,
    knowledge: &Path,
    fetcher: &mut Fetcher,
    options: &Options,
    meta: &Meta,
) -> Result<Summary> {
    let dir = knowledge.join("raw").join(RAW);
    let options = Options {
        refresh: options.refresh || meta.refresh,
        ..options.clone()
    };
    let mut site = Site {
        fetcher,
        state: State::load(&dir),
        dir,
        options: &options,
        summary: Summary::default(),
        plan: Vec::new(),
    };
    sink.note(&alloc::format!(
        "Lean's Splatoon 3 datamine ({SITE}): Salmon Run data and Eggstra Work scenarios; {}",
        LICENSE
    ));
    let built = fetch_and_build(&mut site);
    if !options.dry_run {
        site.save()?;
    }
    let built = built?;
    let mut summary = site.summary.clone();
    if options.dry_run {
        for line in &site.plan {
            sink.note(line);
        }
        match &built {
            Some(b) => {
                summary.version = b.data.version.clone();
                summary.scenarios = b.scenarios.len();
                summary.events = b.events.events.len();
                summary.terms = b.names.terms.len();
                sink.note(&alloc::format!(
                    "dry run: the raw copies would give {} cards ({} scenarios, {} events, {} names) for Splatoon 3 v{}; nothing stored",
                    b.cards.len(),
                    b.scenarios.len(),
                    b.events.events.len(),
                    b.names.terms.len(),
                    b.data.version
                ));
            }
            None => sink.note("dry run: files are not fetched yet; an import fetches them"),
        }
        return Ok(summary);
    }
    let built = built.context("a needed file is missing")?;
    summary.version = built.data.version.clone();
    summary.scenarios = built.scenarios.len();
    summary.events = built.events.events.len();
    summary.terms = built.names.terms.len();
    let path = eggstra::write_events(knowledge, &built.events)?;
    sink.note(&alloc::format!(
        "{} Eggstra Work events ({} with Lean's scenario) in {}",
        built.events.events.len(),
        built
            .events
            .events
            .iter()
            .filter(|e| e.scenario.is_some())
            .count(),
        path.display()
    ));
    let changed = summary.fetched > 0 || site.state.format < CARD_FORMAT;
    let all_stored = built.cards.iter().all(|c| sink.has_id(&c.id));
    if !changed && all_stored && !options.refresh && sink.has_table(NAMES_SOURCE) {
        summary.kept = built.cards.len();
        sink.note(&alloc::format!(
            "nothing changed on the site; {} cards kept (--refresh rebuilds them)",
            built.cards.len()
        ));
        return Ok(summary);
    }
    let total = built.cards.len();
    for (i, doc) in built.cards.into_iter().enumerate() {
        ingest::check(sink)?;
        ingest::add(sink, doc, meta)?;
        sink.progress(i + 1, total);
        summary.cards += 1;
    }
    sink.add_table(&built.names)?;
    sink.note(&alloc::format!(
        "{} names in the glossary's table {NAMES_SOURCE}",
        built.names.terms.len()
    ));
    site.state.format = CARD_FORMAT;
    site.save()?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawl::{Fetched, Http};
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use core::time::Duration;
    use serde_json::json;

    /// A fake of the site and Inkipedia: files by their url's end, each
    /// with an ETag, answering 304 to a matching If-None-Match
    struct Fake {
        files: BTreeMap<&'static str, String>,
        seen: Rc<RefCell<Vec<String>>>,
        /// The backend answering: its ETags end with this, as GitHub
        /// Pages' backends give different ETags for one file
        backend: &'static str,
    }

    impl Fake {
        fn answer(&mut self, url: &str, etag: Option<&str>) -> Fetched {
            self.seen.borrow_mut().push(String::from(url));
            let file = self
                .files
                .iter()
                .find(|(k, _)| url.ends_with(*k))
                .map(|(k, v)| (*k, v.clone()));
            let (status, body, tag) = match file {
                Some((k, body)) => {
                    let tag = alloc::format!("\"{}{}\"", doc_id(k), self.backend);
                    let listed = etag.is_some_and(|list| list.split(", ").any(|t| t == tag));
                    if listed {
                        (304, String::new(), Some(tag))
                    } else {
                        (200, body, Some(tag))
                    }
                }
                None if url.ends_with("robots.txt") => (404, String::new(), None),
                None => (404, String::from("not here"), None),
            };
            Fetched {
                status,
                content_type: String::from("application/json"),
                file_name: None,
                etag: tag,
                final_url: String::from(url),
                body: body.into_bytes(),
            }
        }
    }

    impl Http for Fake {
        fn get(&mut self, url: &str) -> Result<Fetched> {
            Ok(self.answer(url, None))
        }
        fn get_if_none_match(&mut self, url: &str, etag: &str) -> Result<Fetched> {
            Ok(self.answer(url, Some(etag)))
        }
        fn sleep(&mut self, _: Duration) {}
    }

    /// Keeps what is stored in memory
    #[derive(Default)]
    struct Memory {
        docs: BTreeMap<String, Document>,
        tables: Vec<Table>,
        notes: Vec<String>,
        root: PathBuf,
    }

    impl Sink for Memory {
        fn has(&self, key: &str) -> bool {
            self.docs.contains_key(&doc_id(key))
        }
        fn has_id(&self, id: &str) -> bool {
            self.docs.contains_key(id)
        }
        fn has_table(&self, key: &str) -> bool {
            self.tables.iter().any(|t| t.source == key)
        }
        fn add(&mut self, doc: &Document) -> Result<usize> {
            self.docs.insert(doc.id.clone(), doc.clone());
            Ok(1)
        }
        fn raw_dir(&self, kind: &str) -> PathBuf {
            self.root.join("raw").join(kind)
        }
        fn note(&mut self, line: &str) {
            self.notes.push(String::from(line));
        }
        fn add_table(&mut self, table: &Table) -> Result<()> {
            self.tables.retain(|t| t.source != table.source);
            self.tables.push(table.clone());
            Ok(())
        }
    }

    fn files() -> BTreeMap<&'static str, String> {
        let mut f = BTreeMap::new();
        f.insert("splat3/versions.json", json!(["099", "1130"]).to_string());
        f.insert(
            "mush/1130/CoopEnemyInfo.json",
            json!([
                {"Type": "SakelienBomber", "Category": "Boss", "ActorMax": 4, "HitIkuraNum": 25, "KillIkuraNum": 3, "CalcAppearPriority": 1},
                {"Type": "SakelienGiant", "Category": "Other", "ActorMax": 1, "HitIkuraNum": 0, "KillIkuraNum": 0}
            ])
            .to_string(),
        );
        f.insert(
            "mush/1130/CoopSceneInfo.json",
            json!([
                {"Id": 9, "IsBigRun": false, "Season": 7, "__RowId": "Cop_Shakerail"},
                {"Id": 1, "IsBigRun": false, "Season": 1, "__RowId": "Cop_Shakeup"}
            ])
            .to_string(),
        );
        f.insert(
            "mush/1130/WeaponInfoMain.json",
            json!([
                {"__RowId": "Shooter_Normal_00", "Type": "Versus", "WeaponInfoForCoop": "Work/Gyml/Shooter_Normal_Coop.spl__WeaponInfoMain.gyml", "IsCoopRare": false},
                {"__RowId": "Shooter_Normal_Coop", "Type": "Coop", "IsCoopRare": false, "Season": 0, "SpecActor": "Work/Actor/WeaponShooterNormal_Coop.engine__actor__ActorParam.gyml"},
                {"__RowId": "Roller_Bear_Coop", "Type": "Coop", "IsCoopRare": true, "Season": 1, "SpecActor": "Work/Actor/WeaponRollerBear_Coop.engine__actor__ActorParam.gyml"}
            ])
            .to_string(),
        );
        f.insert(
            "mush/1130/WeaponInfoSpecial.json",
            json!([
                {"__RowId": "SpNiceBall", "Type": "Versus"},
                {"__RowId": "SpNiceBall_Coop", "Type": "Coop", "SpecActor": "Work/Actor/WeaponSpNiceBall_Coop.engine__actor__ActorParam.gyml"}
            ])
            .to_string(),
        );
        f.insert(
            "spl__CoopLevelsConfig.spl__CoopLevelsConfig.json",
            json!({"Levels": [
                {"SakelienGiantHPCoef": 0.7, "Round": [{"NormaGoldenIkuraNum": 3}], "EventRush": {"ZakoSpeedCoef": 2.0}},
                {"Difficulty": 1665, "Round": [{"NormaGoldenIkuraNum": 5}], "EventRush": {"ZakoSpeedCoef": 4.0}}
            ]})
            .to_string(),
        );
        f.insert(
            "language/EUen.json",
            json!({
                ENEMY_NAMES: {"SakelienBomber": "Steelhead", "SakelienGiant": "Cohozuna"},
                STAGE_NAMES: {"Shakerail": "Bonerattle Arena", "Shakeup": "Sockeye Station"},
                WEAPON_NAMES: {"Shooter_Normal_00": "Splattershot", "Shooter_Normal_Coop": "Splattershot", "Roller_Bear_Coop": "Grizzco Roller"},
                SPECIAL_NAMES: {"SpNiceBall_Coop": "Booyah Bomb", "SpNiceBall": "Booyah Bomb"}
            })
            .to_string(),
        );
        f.insert(
            "language/JPja.json",
            json!({ENEMY_NAMES: {"SakelienBomber": "バクダン"}, WEAPON_NAMES: {"Shooter_Normal_Coop": "スプラシューター"}})
                .to_string(),
        );
        f.insert(
            "language/CNzh.json",
            json!({ENEMY_NAMES: {"SakelienBomber": "炸弹鱼"}}).to_string(),
        );
        f.insert(
            "weapon/WeaponShooterNormal_Coop.game__GameParameterTable.json",
            json!({"$parent": "Work/Component/GameParameterTable/WeaponShooterNormal.game__GameParameterTable.gyml",
                   "GameParameters": {"DamageParam": {"$type": "x", "ValueMax": 360}}})
            .to_string(),
        );
        f.insert(
            "weapon/WeaponShooterNormal.game__GameParameterTable.json",
            json!({"GameParameters": {"DamageParam": {"$type": "x", "ValueMax": 300, "ValueMin": 250}, "WeaponParam": {"InkConsume": 0.0092}}})
                .to_string(),
        );
        f.insert(
            "weapon/WeaponSpNiceBall_Coop.game__GameParameterTable.json",
            json!({"$parent": "Work/Component/GameParameterTable/WeaponSpNiceBall.game__GameParameterTable.gyml",
                   "GameParameters": {"spl__BulletBlastParam": {"$type": "x", "DistanceDamage": [{"Damage": 7000, "Distance": 7.0}]}}})
            .to_string(),
        );
        f.insert(
            "weapon/WeaponSpNiceBall.game__GameParameterTable.json",
            json!({"GameParameters": {"spl__BulletBlastParam": {"$type": "x", "DistanceDamage": [{"Damage": 1800, "Distance": 7.0}], "PaintRadius": 14.0}}})
                .to_string(),
        );
        // Scenario 1 on Sockeye Station (map 1), scenario 2 on Bonerattle
        // Arena (map 9), as the shifts of the wikitext
        f.insert(
            "EggstraWork01.js",
            crate::eggstra::tests::MODULE.replace("Map: 9", "Map: 1"),
        );
        f.insert(
            "EggstraWork02.js",
            String::from(crate::eggstra::tests::MODULE),
        );
        f.insert(
            "page=List%20of%20Eggstra%20Work%20shifts%20in%20Splatoon%203&prop=wikitext&format=json&formatversion=2&maxlag=5",
            json!({"parse": {"wikitext": crate::eggstra::tests::WIKITEXT}}).to_string(),
        );
        f
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-leanny-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fetcher(seen: &Rc<RefCell<Vec<String>>>) -> Fetcher {
        fetcher_of(seen, "-a")
    }

    fn fetcher_of(seen: &Rc<RefCell<Vec<String>>>, backend: &'static str) -> Fetcher {
        Fetcher::with_http(
            Box::new(Fake {
                files: files(),
                seen: Rc::clone(seen),
                backend,
            }),
            Duration::from_millis(1),
        )
    }

    #[test]
    fn imports_cards_then_keeps_them_when_nothing_changed() {
        let root = temp("ingest");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut sink = Memory {
            root: root.clone(),
            ..Memory::default()
        };
        let options = Options {
            weapons: true,
            ..Options::default()
        };
        let meta = Meta::default();
        let summary = ingest(&mut sink, &root, &mut fetcher(&seen), &options, &meta).unwrap();
        assert_eq!(summary.version, "11.3.0");
        // Everything fetched once; the event after the last and the Grizzco
        // Roller's parameter file answered 404
        assert_eq!(summary.unchanged, 0);
        assert_eq!(summary.missing, 2);
        assert_eq!(summary.scenarios, 2);
        assert_eq!(summary.events, 2);
        // 2 Salmonids, 2 stages, 1 special, 2 weapons, 2 levels, 2 x 6 event cards
        assert_eq!(summary.cards, 2 + 2 + 1 + 2 + 2 + 12);
        assert_eq!(sink.docs.len(), summary.cards);
        assert_eq!(summary.terms, 2 + 2 + 2 + 1);
        assert!(summary.to_string().starts_with("Splatoon 3 v11.3.0: "));

        let steelhead = &sink.docs[&doc_id("leanny:enemy/SakelienBomber")];
        assert_eq!(steelhead.title, "Steelhead (Salmonid, game data)");
        assert!(
            steelhead
                .text
                .contains("Names: Steelhead (ja バクダン, zh 炸弹鱼).")
        );
        assert!(
            steelhead
                .text
                .contains("- At most on the field at once (ActorMax): 4")
        );
        assert!(
            steelhead
                .text
                .contains("- Power eggs per hit (HitIkuraNum): 25")
        );
        // HP from Inkipedia where it has one plain number, else a warning
        assert!(
            steelhead
                .text
                .contains("- HP (not in Lean's data; from Inkipedia"),
            "{}",
            steelhead.text
        );
        let facts = steelhead.facts.as_ref().unwrap();
        assert_eq!(facts.kind, FactKind::Salmonid);
        assert_eq!(facts.params["HitIkuraNum"], json!(25));
        assert_eq!(steelhead.license.as_deref(), Some(LICENSE));
        assert_eq!(steelhead.url.as_deref(), Some(COOP_PAGE));
        let king = &sink.docs[&doc_id("leanny:enemy/SakelienGiant")];
        assert!(
            king.text
                .contains("HP coefficient by hazard level (CoopLevelsConfig): 0%: 0.7")
        );
        assert!(king.text.contains("Hit points are not in this table"));
        assert_eq!(king.facts.as_ref().unwrap().hp_coef[0], (0, 0.7));
        let stage = &sink.docs[&doc_id("leanny:stage/Shakerail")];
        assert!(
            stage
                .text
                .contains("- Eggstra Work events here: #2 (2024-11-09)"),
            "{}",
            stage.text
        );
        let weapon = &sink.docs[&doc_id("leanny:weapon/Shooter_Normal_Coop")];
        assert!(
            weapon
                .text
                .contains("the battle weapon is Shooter_Normal_00")
        );
        assert!(weapon.text.contains("- Grizzco weapon (IsCoopRare): no"));
        assert!(
            weapon
                .text
                .contains("- Eggstra Work events with it: #1 (2023-04-15), #2 (2024-11-09)"),
            "{}",
            weapon.text
        );
        // The Salmon Run override wins over the parent table's value
        assert!(weapon.text.contains("- DamageParam.ValueMax: 360"));
        assert!(weapon.text.contains("- DamageParam.ValueMin: 250"));
        assert!(weapon.text.contains("- WeaponParam.InkConsume: 0.0092"));
        assert!(!weapon.text.contains("$type"));
        // And in players' units, before the raw parameters
        assert!(
            weapon.text.contains("- damage: 36, down to 25"),
            "{}",
            weapon.text
        );
        assert!(weapon.text.find("## In players' units") < weapon.text.find("## Parameters"));
        let facts = weapon.facts.as_ref().unwrap();
        assert_eq!(facts.versus.as_deref(), Some("Shooter_Normal_00"));
        assert_eq!(facts.grizzco, Some(false));
        assert_eq!(facts.events.len(), 2);
        assert_eq!(facts.params["DamageParam"]["ValueMax"], json!(360));
        // A special's parameters: its Salmon Run damage over the battle one
        let special = &sink.docs[&doc_id("leanny:special/SpNiceBall_Coop")];
        assert!(
            special.text.contains(
                "- spl__BulletBlastParam.DistanceDamage: [{\"Damage\":7000,\"Distance\":7.0}]"
            ),
            "{}",
            special.text
        );
        assert!(
            special
                .text
                .contains("- spl__BulletBlastParam.PaintRadius: 14.0")
        );
        let grizzco = &sink.docs[&doc_id("leanny:weapon/Roller_Bear_Coop")];
        assert!(grizzco.text.contains("- Grizzco weapon (IsCoopRare): yes"));
        let level = &sink.docs[&doc_id("leanny:level/1665")];
        assert!(
            level
                .title
                .starts_with("Salmon Run hazard level 333% (difficulty 1665)")
        );
        assert!(level.text.contains("- EventRush.ZakoSpeedCoef: 4.0"));
        let event = &sink.docs[&doc_id("leanny:eggstra/02")];
        assert_eq!(
            event.title,
            "Eggstra Work #2 (2024-11-09, Bonerattle Arena)"
        );
        assert!(
            event
                .text
                .contains("- Stage: Bonerattle Arena (Shakerail, map id 9)")
        );
        assert!(
            event.text.contains(
                "- Weapons: Splattershot (Shooter_Normal); Brush_Normal_00 (Brush_Normal)"
            )
        );
        let wave = &sink.docs[&doc_id("leanny:eggstra/02/wave-1")];
        assert!(wave.text.contains("- 82 s left: Steelhead from B2"));
        let one = &sink.docs[&doc_id("leanny:eggstra/01")];
        assert_eq!(one.title, "Eggstra Work #1 (2023-04-15, Sockeye Station)");
        assert!(one.text.contains("- High score thresholds: top 5%: 203 golden eggs, top 20%: 169 golden eggs, top 50%: 123 golden eggs"));

        // The events table
        let events = crate::eggstra::read_events(&root).unwrap().unwrap();
        assert_eq!(events.events.len(), 2);
        assert_eq!(events.events[0].scenario, Some(1));
        assert_eq!(events.events[1].scenario, Some(2));
        assert_eq!(events.sources[0], SITE);

        // The name table: terms with their keys as origins
        let table = &sink.tables[0];
        assert_eq!(table.source, NAMES_SOURCE);
        let steelhead = table.terms.iter().find(|t| t.id == "steelhead").unwrap();
        assert_eq!(steelhead.forms["ja"], ["バクダン"]);
        assert_eq!(steelhead.kind.as_deref(), Some("boss"));
        assert_eq!(
            steelhead.from,
            ["leanny:names#CommonMsg/Coop/CoopEnemy/SakelienBomber"]
        );
        let shot = table.terms.iter().find(|t| t.id == "splattershot").unwrap();
        assert_eq!(
            shot.from,
            [
                "leanny:names#CommonMsg/Weapon/WeaponName_Main/Shooter_Normal_Coop",
                "leanny:names#CommonMsg/Weapon/WeaponName_Main/Shooter_Normal_00"
            ]
        );
        // The glossary links an icon named by the key
        let mut glossary = crate::glossary::Glossary::seed();
        glossary.merge(&table.terms);
        let mut assets = alloc::vec![crate::assets::Asset::new(
            "icons/Wst_Shooter_Normal_00.png",
            None,
            b"",
            1,
            "h"
        )];
        assert_eq!(crate::assets::link(&mut assets, &glossary), 1);
        assert_eq!(assets[0].term.as_deref(), Some("splattershot"));

        // The state remembers every file with its ETag
        let state = State::load(&root.join("raw").join(RAW));
        assert_eq!(state.files["splat3/versions.json"].status, 200);
        assert_eq!(state.files["splat3/versions.json"].etags.len(), 1);
        assert!(!state.files["splat3/versions.json"].hash.is_empty());
        assert_eq!(
            state.files["eggstra_work/eggstrawork/EggstraWork03.js"].status,
            404
        );
        assert!(root.join("raw/leanny/splat3/versions.json").is_file());

        // A second run: every file answers 304, nothing is stored again
        let requests = seen.borrow().len();
        let again = ingest(&mut sink, &root, &mut fetcher(&seen), &options, &meta).unwrap();
        assert_eq!(again.fetched, 0);
        assert!(again.unchanged >= 10, "{again:?}");
        assert_eq!(again.cards, 0);
        assert_eq!(again.kept, summary.cards);
        assert!(seen.borrow().len() > requests);
        assert!(
            sink.notes
                .iter()
                .any(|n| n.starts_with("nothing changed on the site"))
        );
        // Another backend answers 200 with other ETags and the same
        // content: still nothing changed, and both ETags are remembered
        let other = ingest(
            &mut sink,
            &root,
            &mut fetcher_of(&seen, "-b"),
            &options,
            &meta,
        )
        .unwrap();
        assert_eq!(other.fetched, 0, "{other:?}");
        assert_eq!(other.cards, 0);
        assert_eq!(other.kept, summary.cards);
        let state = State::load(&root.join("raw").join(RAW));
        assert_eq!(state.files["splat3/versions.json"].etags.len(), 2);
        // Asked to refresh, it stores them again
        let refreshed = ingest(
            &mut sink,
            &root,
            &mut fetcher(&seen),
            &Options {
                refresh: true,
                ..options.clone()
            },
            &meta,
        )
        .unwrap();
        assert_eq!(refreshed.cards, summary.cards);
        // Cards of an older format are rebuilt though nothing changed
        let dir = root.join("raw").join(RAW);
        let mut state = State::load(&dir);
        assert_eq!(state.format, CARD_FORMAT);
        state.format = 1;
        state.save(&dir).unwrap();
        let rebuilt = ingest(&mut sink, &root, &mut fetcher(&seen), &options, &meta).unwrap();
        assert_eq!(rebuilt.fetched, 0);
        assert_eq!(rebuilt.cards, summary.cards);
        assert_eq!(State::load(&dir).format, CARD_FORMAT);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_dry_run_fetches_nothing() {
        let root = temp("dry");
        let seen = Rc::new(RefCell::new(Vec::new()));
        let mut sink = Memory {
            root: root.clone(),
            ..Memory::default()
        };
        let options = Options {
            dry_run: true,
            ..Options::default()
        };
        let summary = ingest(
            &mut sink,
            &root,
            &mut fetcher(&seen),
            &options,
            &Meta::default(),
        )
        .unwrap();
        assert!(seen.borrow().is_empty());
        assert_eq!(summary.cards, 0);
        assert!(sink.docs.is_empty());
        assert!(!root.join("raw/leanny/state.json").exists());
        assert!(
            sink.notes
                .iter()
                .any(|n| n.ends_with("splat3/versions.json: not fetched yet"))
        );
        assert!(
            sink.notes
                .last()
                .unwrap()
                .starts_with("dry run: files are not fetched yet")
        );
        // After an import, a dry run reads the copies and counts the cards
        ingest(
            &mut sink,
            &root,
            &mut fetcher(&seen),
            &Options::default(),
            &Meta::default(),
        )
        .unwrap();
        let requests = seen.borrow().len();
        let summary = ingest(
            &mut sink,
            &root,
            &mut fetcher(&seen),
            &options,
            &Meta::default(),
        )
        .unwrap();
        assert_eq!(seen.borrow().len(), requests);
        assert_eq!(summary.scenarios, 2);
        assert!(
            sink.notes.last().unwrap().contains("would give 21 cards"),
            "{:?}",
            sink.notes.last()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn versions_and_parameters() {
        assert_eq!(version_name("1130"), "11.3.0");
        assert_eq!(version_name("099"), "0.9.9");
        assert_eq!(version_name("latest"), "latest");
        let mut base = json!({"a": {"x": 1, "y": 2}, "b": 3});
        merge(&mut base, &json!({"a": {"x": 9}, "c": [1, 2]}));
        assert_eq!(base, json!({"a": {"x": 9, "y": 2}, "b": 3, "c": [1, 2]}));
        let mut lines = Vec::new();
        parameter_lines(&base, "", &mut lines);
        assert_eq!(
            lines,
            ["- a.x: 9", "- a.y: 2", "", "- b: 3", "", "- c: [1,2]"]
        );
        // A curve of hundreds of points is not written out
        let curve: Vec<u32> = (0..200).collect();
        let mut lines = Vec::new();
        parameter_lines(&json!({"Curve": curve}), "", &mut lines);
        assert_eq!(lines, ["- Curve: (200 values, left out)"]);
        assert_eq!(content_hash(b"a"), content_hash(b"a"));
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
        assert_eq!(
            stem("Work/Actor/WeaponShooterNormal_Coop.engine__actor__ActorParam.gyml"),
            Some("WeaponShooterNormal_Coop")
        );
        let names = Names::default();
        assert_eq!(names.line(ENEMY_NAMES, "SakelienBomber"), "SakelienBomber");
        assert!(
            provenance("11.3.0").starts_with("Splatoon 3 v11.3.0 game data from Lean's datamine")
        );
    }

    #[test]
    fn items_for_pickers_in_the_games_order() {
        let root = temp("items");
        // Nothing fetched yet
        assert!(items(&root, &Glossary::default()).unwrap().is_empty());
        let raw = root.join("raw").join(RAW).join("splat3");
        let mush = raw.join("data/mush/1130");
        std::fs::create_dir_all(&mush).unwrap();
        std::fs::write(
            raw.join("versions.json"),
            json!(["099", "1130"]).to_string(),
        )
        .unwrap();
        std::fs::write(
            mush.join("WeaponInfoMain.json"),
            json!([
                {"__RowId": "Roller_Bear_Coop", "Type": "Coop", "Id": 21900, "IsCoopRare": true},
                {"__RowId": "Shooter_Normal_00", "Type": "Versus", "Id": 40,
                 "WeaponInfoForCoop": "Work/Gyml/Shooter_Normal_Coop.spl__WeaponInfoMain.gyml"},
                {"__RowId": "Shooter_Normal_Coop", "Type": "Coop", "Id": 20040, "IsCoopRare": false}
            ])
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            mush.join("WeaponInfoSpecial.json"),
            json!([
                {"__RowId": "SpJetpack_Coop", "Type": "Coop", "Id": 20010},
                {"__RowId": "SpNiceBall", "Type": "Versus", "Id": 6},
                {"__RowId": "SpNiceBall_Coop", "Type": "Coop", "Id": 20006}
            ])
            .to_string(),
        )
        .unwrap();
        let forms = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(lang, name)| (String::from(*lang), alloc::vec![String::from(*name)]))
                .collect()
        };
        let mut glossary = Glossary::default();
        glossary.merge(&[
            Term {
                id: String::from("splattershot"),
                forms: forms(&[
                    ("en", "Splattershot"),
                    ("ja", "Splattershot ja"),
                    ("fr", "Liquidateur"),
                ]),
                from: alloc::vec![alloc::format!(
                    "{NAMES_SOURCE}#{WEAPON_NAMES}/Shooter_Normal_Coop"
                )],
                aliases: alloc::vec![crate::glossary::Alias {
                    text: String::from("shot"),
                    lang: String::from("en"),
                    ..Default::default()
                }],
                ..Default::default()
            },
            Term {
                id: String::from("booyah-bomb"),
                forms: forms(&[("en", "Booyah Bomb")]),
                from: alloc::vec![alloc::format!(
                    "{NAMES_SOURCE}#{SPECIAL_NAMES}/SpNiceBall_Coop"
                )],
                ..Default::default()
            },
        ]);
        // A second English name, as an older game's table gives one
        glossary.terms[0]
            .forms
            .get_mut("en")
            .unwrap()
            .push(String::from("Hero Shot"));
        let found = items(&root, &glossary).unwrap();
        let keys: Vec<&str> = found.iter().map(|i| i.key.as_str()).collect();
        // Weapons, then specials, each by the game's number
        assert_eq!(
            keys,
            [
                "Shooter_Normal_Coop",
                "Roller_Bear_Coop",
                "SpNiceBall_Coop",
                "SpJetpack_Coop"
            ]
        );
        let shot = &found[0];
        assert_eq!((shot.kind, shot.id, shot.grizzco), ("weapon", 20040, false));
        assert_eq!(shot.icon, "weapon_flat/Path_Wst_Shooter_Normal_00.png");
        assert_eq!(shot.term.as_deref(), Some("splattershot"));
        assert_eq!(shot.names["en"], "Splattershot");
        assert_eq!(shot.names["ja"], "Splattershot ja");
        // Further names and approved slang are searched, not shown; other
        // languages are neither
        assert!(!shot.names.contains_key("fr"));
        assert_eq!(shot.search, ["Hero Shot", "shot"]);
        let roller = &found[1];
        assert!(roller.grizzco);
        assert_eq!(roller.icon, "weapon_flat/Path_Wst_Roller_Bear.png");
        // Not in the glossary: no term, no names
        assert_eq!(roller.term, None);
        assert!(roller.names.is_empty());
        assert_eq!(found[2].kind, "special");
        assert_eq!(found[2].icon, "subspe/Wsp_SpNiceBall00.png");
        assert_eq!(found[2].term.as_deref(), Some("booyah-bomb"));

        // Only the pictures pickers show are ever asked of Lean's site
        assert_eq!(
            icon_url(&shot.icon).as_deref(),
            Some(
                "https://leanny.github.io/splat3/images/weapon_flat/Path_Wst_Shooter_Normal_00.png"
            )
        );
        assert!(icon_url("subspe/Wsb_Bomb_Splash00.png").is_some());
        for bad in [
            "",
            "weapon_flat/Path_Wst_.png",
            "weapon_flat/Path_Wst_x.jpg",
            "weapon_flat/../Path_Wst_x.png",
            "subspe/Wsp_a/../b.png",
            "coopEnemy/SakelienBomber.png",
            "https://example.com/x.png",
        ] {
            assert_eq!(icon_url(bad), None, "{bad}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }
}
