//! Eggstra Work events: the scenario of each event as Lean publishes it
//! (`https://leanny.github.io/eggstra_work/coop_event_NN.html`, whose data
//! is the JavaScript module `eggstrawork/EggstraWorkNN.js`: stage,
//! weapons, specials, the five waves' tide and occurrence, and the boss
//! spawn schedule per hazard level), the dates of the events (Inkipedia's
//! "List of Eggstra Work shifts in Splatoon 3", since Lean's data carries
//! none), the events table in the knowledge folder
//! (`corpus/eggstra_events.json`, [`write_events`]), fact cards per event
//! and per wave ([`cards`]), and which event a #vod-review VOD was probably
//! played in ([`event_at`]: posted during the 48-hour shift or the week
//! after it).
//!
//! Lean's pages decode the data with tables of their own script
//! (`js/salmonrun.js`): the wave codes ([`WAVE_TYPES`]) and the spawn
//! timing in frames of the 100 s wave timer; those tables are repeated
//! here. [`crate::leanny`] fetches the files and names the keys.

use crate::doc::Document;
use crate::leanny::{self, Names};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The events table, in the corpus folder ([`crate::corpus::CORPUS_DIR`])
pub const EVENTS_FILE: &str = "eggstra_events.json";
/// How long a shift runs
pub const SHIFT_HOURS: i64 = 48;
/// A VOD posted this many days after a shift is still probably from it
pub const AFTER_DAYS: i64 = 7;
/// The Inkipedia page with the shifts' dates
pub const INKIPEDIA_PAGE: &str = "List of Eggstra Work shifts in Splatoon 3";
/// Inkipedia's API
pub const INKIPEDIA_API: &str = "https://splatoonwiki.org/w/api.php";
/// Inkipedia's content license
pub const INKIPEDIA_LICENSE: &str = "CC BY-SA 4.0 (Inkipedia contributors)";

/// The page of an event on Lean's site
pub fn page_url(number: u8) -> String {
    alloc::format!("{}/eggstra_work/coop_event_{number:02}.html", leanny::SITE)
}

/// The data module of an event on Lean's site
pub fn data_url(number: u8) -> String {
    alloc::format!(
        "{}/eggstra_work/eggstrawork/EggstraWork{number:02}.js",
        leanny::SITE
    )
}

/// The address of the Inkipedia list's wikitext
pub fn inkipedia_url() -> String {
    alloc::format!(
        "{INKIPEDIA_API}?action=parse&page={}&prop=wikitext&format=json&formatversion=2&maxlag=5",
        crate::crawl::encode(INKIPEDIA_PAGE)
    )
}

// ------------------------------------------------ the JavaScript module

/// Parses a JavaScript object literal as the data modules hold one
/// (`export default { Map: 9, Waves: [2, 16], Spawns: { 1: { 300: [...] }
/// } }`): bare or quoted keys, trailing commas, strings, numbers, booleans
/// and null; nothing else of the language
pub fn js_value(text: &str) -> Result<Value> {
    let start = text.find('{').context("no object in the module")?;
    let mut p = Parser {
        chars: text[start..].chars().collect(),
        at: 0,
    };
    let value = p.value()?;
    p.skip();
    ensure!(p.at >= p.chars.len(), "text after the object at {}", p.at);
    Ok(value)
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Skips whitespace and `//` comments
    fn skip(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.at += 1,
                Some('/') if self.chars.get(self.at + 1) == Some(&'/') => {
                    while !matches!(self.peek(), None | Some('\n')) {
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn expect(&mut self, c: char) -> Result<()> {
        self.skip();
        ensure!(self.peek() == Some(c), "expected '{c}' at {}", self.at);
        self.at += 1;
        Ok(())
    }

    fn value(&mut self) -> Result<Value> {
        self.skip();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"') | Some('\'') => Ok(Value::String(self.string()?)),
            Some(c) if c == '-' || c.is_ascii_digit() => self.number(),
            Some(c) if c.is_alphabetic() => {
                let word = self.word();
                match word.as_str() {
                    "true" => Ok(Value::Bool(true)),
                    "false" => Ok(Value::Bool(false)),
                    "null" | "undefined" => Ok(Value::Null),
                    _ => bail!("unknown word {word} at {}", self.at),
                }
            }
            _ => bail!("unexpected input at {}", self.at),
        }
    }

    fn object(&mut self) -> Result<Value> {
        self.expect('{')?;
        let mut out = Map::new();
        loop {
            self.skip();
            match self.peek() {
                Some('}') => {
                    self.at += 1;
                    return Ok(Value::Object(out));
                }
                Some('"') | Some('\'') => {
                    let key = self.string()?;
                    self.expect(':')?;
                    let v = self.value()?;
                    out.insert(key, v);
                }
                Some(c) if c.is_alphanumeric() || c == '_' || c == '$' => {
                    let key = self.word();
                    self.expect(':')?;
                    let v = self.value()?;
                    out.insert(key, v);
                }
                _ => bail!("bad object key at {}", self.at),
            }
            self.skip();
            if self.peek() == Some(',') {
                self.at += 1;
            }
        }
    }

    fn array(&mut self) -> Result<Value> {
        self.expect('[')?;
        let mut out = Vec::new();
        loop {
            self.skip();
            if self.peek() == Some(']') {
                self.at += 1;
                return Ok(Value::Array(out));
            }
            out.push(self.value()?);
            self.skip();
            if self.peek() == Some(',') {
                self.at += 1;
            }
        }
    }

    fn string(&mut self) -> Result<String> {
        let quote = self.peek().context("no string")?;
        self.at += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None => bail!("unterminated string"),
                Some('\\') => {
                    self.at += 1;
                    let c = self.peek().context("unterminated string")?;
                    out.push(match c {
                        'n' => '\n',
                        't' => '\t',
                        c => c,
                    });
                    self.at += 1;
                }
                Some(c) if c == quote => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(c) => {
                    out.push(c);
                    self.at += 1;
                }
            }
        }
    }

    fn word(&mut self) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' || c == '$' {
                out.push(c);
                self.at += 1;
            } else {
                break;
            }
        }
        out
    }

    fn number(&mut self) -> Result<Value> {
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E') {
                text.push(c);
                self.at += 1;
            } else {
                break;
            }
        }
        let n: serde_json::Number = text
            .parse()
            .with_context(|| alloc::format!("bad number {text}"))?;
        Ok(Value::Number(n))
    }
}

// ------------------------------------------------------- the scenario

/// Tide of a wave
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tide {
    Low,
    Normal,
    High,
}

impl Tide {
    /// The glossary's name
    pub fn name(self) -> &'static str {
        match self {
            Tide::Low => "Low Tide",
            Tide::Normal => "Normal Tide",
            Tide::High => "High Tide",
        }
    }
}

/// Occurrence of a wave, by the game's names
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Occurrence {
    Normal,
    Rush,
    Geyser,
    Dozer,
    Hakobiya,
    Fog,
    Missile,
    Relay,
    Tamaire,
}

impl Occurrence {
    /// The English name of the known occurrence (a standard wave has none)
    pub fn name(self) -> &'static str {
        match self {
            Occurrence::Normal => "Standard",
            Occurrence::Rush => "Rush",
            Occurrence::Geyser => "Goldie Seeking",
            Occurrence::Dozer => "The Griller",
            Occurrence::Hakobiya => "The Mothership",
            Occurrence::Fog => "Fog",
            Occurrence::Missile => "Cohock Charge",
            Occurrence::Relay => "Giant Tornado",
            Occurrence::Tamaire => "Mudmouth Eruptions",
        }
    }
}

/// What each wave code of the data means: occurrence and tide, as Lean's
/// `salmonrun.js` decodes it
pub const WAVE_TYPES: [(Occurrence, Tide); 19] = [
    (Occurrence::Normal, Tide::Normal),
    (Occurrence::Normal, Tide::Low),
    (Occurrence::Normal, Tide::High),
    (Occurrence::Rush, Tide::Normal),
    (Occurrence::Rush, Tide::High),
    (Occurrence::Fog, Tide::Normal),
    (Occurrence::Fog, Tide::Low),
    (Occurrence::Fog, Tide::High),
    (Occurrence::Dozer, Tide::Normal),
    (Occurrence::Dozer, Tide::High),
    (Occurrence::Missile, Tide::Low),
    (Occurrence::Geyser, Tide::Normal),
    (Occurrence::Geyser, Tide::High),
    (Occurrence::Hakobiya, Tide::Normal),
    (Occurrence::Hakobiya, Tide::Low),
    (Occurrence::Hakobiya, Tide::High),
    (Occurrence::Tamaire, Tide::Normal),
    (Occurrence::Tamaire, Tide::High),
    (Occurrence::Relay, Tide::Low),
];

/// The occurrence and tide of a wave code
pub fn wave_type(code: u8) -> Option<(Occurrence, Tide)> {
    WAVE_TYPES.get(usize::from(code)).copied()
}

/// One event's scenario as Lean's data module holds it
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    /// The page's number (`coop_event_07` is 7)
    pub number: u8,
    /// Stage id, as `CoopSceneInfo` numbers them
    pub map: u64,
    /// The five waves' codes ([`wave_type`])
    pub waves: Vec<u8>,
    /// Weapon keys without their variant suffix (`Shooter_Normal`)
    pub weapons: Vec<String>,
    /// Special keys (`SpNiceBall`)
    pub specials: Vec<String>,
    /// The Snatcher's spawn point per wave, 1-based
    pub snatcher: Vec<u8>,
    /// Wave number (as text) to hazard level (as text, `300`) to the
    /// wave's spawn list
    pub spawns: Value,
}

impl Scenario {
    /// Reads a data module
    pub fn parse(number: u8, module: &str) -> Result<Scenario> {
        let v = js_value(module).with_context(|| alloc::format!("EggstraWork{number:02}.js"))?;
        let strings = |key: &str| -> Vec<String> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        };
        let numbers = |key: &str| -> Vec<u8> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_u64().map(|n| n as u8))
                .collect()
        };
        let scenario = Scenario {
            number,
            map: v["Map"].as_u64().context("no Map")?,
            waves: numbers("Waves"),
            weapons: strings("Weapons"),
            specials: strings("Specials"),
            snatcher: numbers("Snatcher"),
            spawns: v["Spawns"].clone(),
        };
        ensure!(scenario.waves.len() == 5, "not five waves");
        Ok(scenario)
    }

    /// The occurrence and tide of wave `wave` (1-based)
    pub fn wave_type(&self, wave: usize) -> Option<(Occurrence, Tide)> {
        self.waves.get(wave - 1).and_then(|c| wave_type(*c))
    }

    /// Hazard levels the data holds for wave `wave` (1-based), rising
    pub fn hazard_levels(&self, wave: usize) -> Vec<u32> {
        let mut out: Vec<u32> = self.spawns[wave.to_string()]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, _)| k.parse().ok())
            .collect();
        out.sort_unstable();
        out
    }
}

/// A hazard level as the game shows it: the difficulty value is five times
/// the percentage (300 is 60%)
pub fn hazard_percent(difficulty: u32) -> u32 {
    difficulty / 5
}

/// A spawn timing in frames of the wave timer, as seconds left of 100
fn timer_left(frames: f64) -> u32 {
    (100.0 - frames / 60.0).floor().max(0.0) as u32
}

/// A spawn point's letter and number (`B2`; `C0` for a point without one)
fn spawn_label(entry: &Value) -> String {
    let point = entry["Spawn"].as_str().unwrap_or("?");
    match entry["SubSpawn"].as_u64() {
        Some(n) => alloc::format!("{point}{n}"),
        None => String::from(point),
    }
}

/// A spawn index as the page letters it (0 is A)
fn letter(n: u64) -> char {
    char::from_u32(u32::from(b'A') + n as u32).unwrap_or('?')
}

/// The lines of one wave's spawn list at one hazard level, as the page
/// shows them for the occurrence
pub fn spawn_lines(occurrence: Occurrence, list: &Value, names: &Names) -> Vec<String> {
    let entries: Vec<&Value> = list.as_array().into_iter().flatten().collect();
    let mut out = Vec::new();
    match occurrence {
        Occurrence::Normal | Occurrence::Fog | Occurrence::Missile => {
            for e in entries {
                let at = timer_left(e["Timing"].as_f64().unwrap_or(0.0));
                if e["Lesser"].as_bool() == Some(true) {
                    out.push(alloc::format!(
                        "- {at} s left: lesser Salmonids from {}",
                        e["Spawn"].as_str().unwrap_or("?")
                    ));
                    continue;
                }
                let key = e["Boss"].as_str().unwrap_or("?");
                let boss = names.en(leanny::ENEMY_NAMES, key).unwrap_or(key);
                let eggs = e["Eggs"]
                    .as_u64()
                    .map(|n| alloc::format!(" carrying {n} golden eggs"))
                    .unwrap_or_default();
                let point = if key == "SakePillar" {
                    alloc::format!("Fs{}0", e["Spawn"].as_str().unwrap_or("?"))
                } else if key == "SakelienCupTwins" {
                    String::from(e["Spawn"].as_str().unwrap_or("?"))
                } else {
                    spawn_label(e)
                };
                out.push(alloc::format!("- {at} s left: {boss}{eggs} from {point}"));
            }
        }
        Occurrence::Rush | Occurrence::Dozer => {
            if let Some(targets) = entries.iter().find_map(|e| e["Targets"].as_array()) {
                let order: Vec<String> = targets
                    .iter()
                    .filter_map(|t| t.as_u64())
                    .map(|i| alloc::format!("weapon {}", i + 1))
                    .collect();
                out.push(alloc::format!(
                    "- Target order (the event's weapons in order): {}",
                    order.join(" > ")
                ));
            }
            for e in entries.iter().filter(|e| e.get("Targets").is_none()) {
                let at = timer_left(e["Timing"].as_f64().unwrap_or(0.0));
                out.push(alloc::format!(
                    "- {at} s left: spawn area {}",
                    e["Spawn"].as_str().unwrap_or("?")
                ));
            }
        }
        Occurrence::Geyser => {
            for (i, e) in entries.iter().enumerate() {
                let pair = e.as_array();
                let start = pair.and_then(|p| p.first()?.as_u64());
                let goal = pair.and_then(|p| p.get(1)?.as_u64());
                if let (Some(s), Some(g)) = (start, goal) {
                    out.push(alloc::format!(
                        "- Goldie {}: gusher {} first, its goal gusher {}",
                        i + 1,
                        letter(s),
                        letter(g)
                    ));
                }
            }
        }
        Occurrence::Hakobiya => {
            if let Some(targets) = entries.iter().find_map(|e| e["Targets"].as_array()) {
                let boxes: Vec<String> = targets.iter().map(|t| t.to_string()).collect();
                out.push(alloc::format!(
                    "- Spawn boxes in order (the page's numbering): {}",
                    boxes.join(", ")
                ));
            }
        }
        Occurrence::Relay => {
            let points: Vec<String> = entries
                .iter()
                .map(|e| {
                    alloc::format!(
                        "G{}{}",
                        e["Spawn"].as_str().unwrap_or("?"),
                        e["SubSpawn"].as_u64().unwrap_or(0)
                    )
                })
                .collect();
            out.push(alloc::format!(
                "- Tornado drop points in order: {}",
                points.join(", ")
            ));
        }
        Occurrence::Tamaire => {
            out.push(String::from(
                "- Mudmouth spawns depend on when the earlier ones are cleared; the data holds the random rolls, which Lean's page simulates (not exact yet, the page says)",
            ));
        }
    }
    out
}

// -------------------------------------------------- Inkipedia's dates

/// One shift as Inkipedia's list gives it (`{{Eggstra Work result
/// |start=2023-04-15 |stage=Sockeye Station ...}}`)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Shift {
    /// The day it started (UTC)
    pub start: NaiveDate,
    pub stage: String,
    pub weapons: Vec<String>,
    pub specials: Vec<String>,
    /// Tide and occurrence per wave, as written (`High Tide`, `Rush`)
    pub tides: Vec<String>,
    pub occurrences: Vec<String>,
    /// The reward for taking part
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prize: Option<String>,
    /// High score thresholds by top percentage (`5%`, `20%`, `50%`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<(String, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Wikitext without links, references and the templates the notes use
fn plain(text: &str) -> String {
    let mut s = String::from(text);
    while let Some(a) = s.find("<ref") {
        match s[a..].find("</ref>") {
            Some(b) => s.replace_range(a..a + b + 6, ""),
            None => break,
        }
    }
    // [[target|label]] and [[label]]
    while let Some(a) = s.find("[[") {
        let Some(b) = s[a..].find("]]") else { break };
        let inner = &s[a + 2..a + b];
        let label = inner.rsplit('|').next().unwrap_or(inner).to_string();
        s.replace_range(a..a + b + 2, &label);
    }
    // {{date|X}} keeps X, other templates go
    while let Some(a) = s.find("{{") {
        let Some(b) = s[a..].find("}}") else { break };
        let inner = &s[a + 2..a + b];
        let kept = inner
            .strip_prefix("date|")
            .map(String::from)
            .unwrap_or_default();
        s.replace_range(a..a + b + 2, &kept);
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The shifts the list page's wikitext holds, in the page's order
pub fn parse_shifts(wikitext: &str) -> Vec<Shift> {
    let mut out = Vec::new();
    let mut rest = wikitext;
    while let Some(start) = rest.find("{{Eggstra Work result") {
        let body = &rest[start + 2..];
        // The template ends at the `}}` matching its own `{{`
        let mut depth = 1;
        let mut end = body.len();
        let bytes = body.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            match &bytes[i..i + 2] {
                b"{{" => {
                    depth += 1;
                    i += 2;
                }
                b"}}" => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                    i += 2;
                }
                _ => i += 1,
            }
        }
        let inner = &body[..end];
        rest = &body[end..];
        let mut fields: BTreeMap<String, String> = BTreeMap::new();
        for part in inner.split("\n|").skip(1) {
            if let Some((k, v)) = part.split_once('=') {
                fields.insert(k.trim().to_string(), plain(v));
            }
        }
        let Some(start) = fields
            .get("start")
            .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        else {
            continue;
        };
        let series = |prefix: &str| -> Vec<String> {
            (1..=5)
                .filter_map(|i| fields.get(&alloc::format!("{prefix}{i}")))
                .filter(|v| !v.is_empty())
                .cloned()
                .collect()
        };
        let thresholds = ["5%", "20%", "50%"]
            .into_iter()
            .filter_map(|k| Some((String::from(k), fields.get(k)?.parse().ok()?)))
            .collect();
        out.push(Shift {
            start,
            stage: fields.get("stage").cloned().unwrap_or_default(),
            weapons: series("weapon"),
            specials: series("special"),
            tides: series("tide"),
            occurrences: series("wave"),
            prize: fields.get("prize").cloned().filter(|p| !p.is_empty()),
            thresholds,
            note: fields.get("note").cloned().filter(|n| !n.is_empty()),
        });
    }
    out
}

// ---------------------------------------------------- the events table

/// One wave of an event, in words
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaveSummary {
    pub tide: String,
    pub occurrence: String,
}

/// An Eggstra Work event: Inkipedia's dates and names, with Lean's
/// scenario when its page exists
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// The event's number, in order of the shifts
    pub number: u8,
    /// When the shift started (the day, 00:00 UTC)
    pub start: DateTime<Utc>,
    /// When it ended ([`SHIFT_HOURS`] later)
    pub end: DateTime<Utc>,
    pub stage: String,
    pub weapons: Vec<String>,
    pub specials: Vec<String>,
    pub waves: Vec<WaveSummary>,
    /// Lean's page number when it has this event's scenario (the same
    /// number, checked by the stage)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<u8>,
    /// Lean's page
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prize: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thresholds: Vec<(String, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Event {
    /// `#7 (2024-11-09, Bonerattle Arena)`
    pub fn label(&self) -> String {
        alloc::format!(
            "#{} ({}, {})",
            self.number,
            self.start.date_naive(),
            self.stage
        )
    }
}

/// The events table as written
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Events {
    /// When it was built
    pub built_at: DateTime<Utc>,
    /// Where the rows come from
    pub sources: Vec<String>,
    pub events: Vec<Event>,
}

/// The rows of the table: Inkipedia's shifts in order, each with Lean's
/// scenario of the same number when its stage (named through
/// `stage_name`, from the stage id) is the shift's. A mismatch is logged
/// and the row keeps no scenario.
pub fn build_events(
    shifts: &[Shift],
    scenarios: &[Scenario],
    stage_name: &dyn Fn(u64) -> Option<String>,
) -> Vec<Event> {
    shifts
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let number = (i + 1) as u8;
            let scenario = scenarios.iter().find(|sc| sc.number == number).filter(|sc| {
                let stage = stage_name(sc.map).unwrap_or_default();
                let same = stage.eq_ignore_ascii_case(&s.stage);
                if !same {
                    log::warn!(
                        "Eggstra Work #{number}: Lean's scenario {number} is on {stage}, Inkipedia's shift on {}; not linked",
                        s.stage
                    );
                }
                same
            });
            let start = s.start.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc();
            let waves = (0..5)
                .map(|w| WaveSummary {
                    tide: s.tides.get(w).cloned().unwrap_or_default(),
                    occurrence: s.occurrences.get(w).cloned().unwrap_or_default(),
                })
                .collect();
            Event {
                number,
                start,
                end: start + Duration::hours(SHIFT_HOURS),
                stage: s.stage.clone(),
                weapons: s.weapons.clone(),
                specials: s.specials.clone(),
                waves,
                scenario: scenario.map(|sc| sc.number),
                url: scenario.map(|sc| page_url(sc.number)),
                prize: s.prize.clone(),
                thresholds: s.thresholds.clone(),
                note: s.note.clone(),
            }
        })
        .collect()
}

/// The events table's file
pub fn events_path(knowledge: &Path) -> PathBuf {
    knowledge.join(crate::corpus::CORPUS_DIR).join(EVENTS_FILE)
}

/// Writes the events table
pub fn write_events(knowledge: &Path, events: &Events) -> Result<PathBuf> {
    let path = events_path(knowledge);
    crate::store::write_atomic(&path, &serde_json::to_vec_pretty(events)?)?;
    Ok(path)
}

/// The events table, if written
pub fn read_events(knowledge: &Path) -> Result<Option<Events>> {
    let path = events_path(knowledge);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        std::fs::read(&path).with_context(|| alloc::format!("reading {}", path.display()))?;
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        alloc::format!("in {}", path.display())
    })?))
}

/// The event something posted at `at` was probably played in: the last
/// shift that started before it, when it was posted during the shift or
/// within [`AFTER_DAYS`] after its end
pub fn event_at(events: &[Event], at: DateTime<Utc>) -> Option<u8> {
    events
        .iter()
        .filter(|e| e.start <= at && at < e.end + Duration::days(AFTER_DAYS))
        .max_by_key(|e| e.start)
        .map(|e| e.number)
}

// -------------------------------------------------------- fact cards

/// The document key of an event's overview card
pub fn overview_key(number: u8) -> String {
    alloc::format!("leanny:eggstra/{number:02}")
}

/// The document key of an event's wave card
pub fn wave_key(number: u8, wave: usize) -> String {
    alloc::format!("leanny:eggstra/{number:02}/wave-{wave}")
}

/// The overview card of an event Lean has no scenario page for (a rerun of
/// an earlier scenario, say): what Inkipedia's list says of it
pub fn shift_card(event: &Event) -> Document {
    let title = alloc::format!("Eggstra Work {}", event.label());
    let waves: Vec<String> = event
        .waves
        .iter()
        .enumerate()
        .map(|(i, w)| alloc::format!("wave {}: {}, {}", i + 1, w.tide, w.occurrence))
        .collect();
    let mut text = alloc::format!(
        "# {title}\n\nFrom Inkipedia's list of Eggstra Work shifts (CC BY-SA 4.0); Lean's site has no scenario page for this event.\n\n- Dates: {} to {} UTC ({SHIFT_HOURS} hours)\n- Stage: {}\n- Weapons: {}\n- Specials: {}\n- Waves: {}\n",
        event.start.format("%Y-%m-%d %H:%M"),
        event.end.format("%Y-%m-%d %H:%M"),
        event.stage,
        event.weapons.join("; "),
        event.specials.join("; "),
        waves.join("; ")
    );
    if let Some(p) = &event.prize {
        text.push_str(&alloc::format!("- Reward for taking part: {p}\n"));
    }
    if !event.thresholds.is_empty() {
        let t: Vec<String> = event
            .thresholds
            .iter()
            .map(|(k, v)| alloc::format!("top {k}: {v} golden eggs"))
            .collect();
        text.push_str(&alloc::format!(
            "- High score thresholds: {}\n",
            t.join(", ")
        ));
    }
    if let Some(note) = &event.note {
        text.push_str(&alloc::format!("- Note: {note}\n"));
    }
    let mut doc = leanny::card(
        &overview_key(event.number),
        &alloc::format!(
            "https://splatoonwiki.org/wiki/{}",
            INKIPEDIA_PAGE.replace(' ', "_")
        ),
        title,
        text,
        "",
    );
    doc.license = Some(String::from(INKIPEDIA_LICENSE));
    doc.attribution = Some(String::from("Inkipedia contributors"));
    doc
}

/// The fact cards of a scenario: an overview (dates, stage, weapons,
/// specials, the waves, Inkipedia's rewards) and one card per wave with
/// the spawn schedule at every hazard level the data holds
pub fn cards(
    scenario: &Scenario,
    event: Option<&Event>,
    names: &Names,
    version: &str,
) -> Vec<Document> {
    let n = scenario.number;
    let stage_key = names.stage_key(scenario.map);
    let stage = stage_key
        .as_deref()
        .and_then(|k| names.en(leanny::STAGE_NAMES, k))
        .map(String::from)
        .or_else(|| event.map(|e| e.stage.clone()))
        .unwrap_or_else(|| alloc::format!("stage {}", scenario.map));
    let title_head = match event {
        Some(e) => alloc::format!("Eggstra Work {}", e.label()),
        None => alloc::format!("Eggstra Work scenario {n} ({stage})"),
    };
    let weapons: Vec<String> = scenario
        .weapons
        .iter()
        .map(|w| {
            let key = alloc::format!("{w}_00");
            alloc::format!("{} ({w})", names.line(leanny::WEAPON_NAMES, &key))
        })
        .collect();
    let specials: Vec<String> = scenario
        .specials
        .iter()
        .map(|s| alloc::format!("{} ({s})", names.line(leanny::SPECIAL_NAMES, s)))
        .collect();
    let waves: Vec<String> = (1..=5)
        .map(|w| match scenario.wave_type(w) {
            Some((occ, tide)) => alloc::format!("wave {w}: {}, {}", tide.name(), occ.name()),
            None => alloc::format!("wave {w}: code {}", scenario.waves[w - 1]),
        })
        .collect();
    let mut text = alloc::format!("# {title_head}\n\n");
    match event {
        Some(e) => text.push_str(&alloc::format!(
            "- Dates: {} to {} UTC ({SHIFT_HOURS} hours)\n",
            e.start.format("%Y-%m-%d %H:%M"),
            e.end.format("%Y-%m-%d %H:%M")
        )),
        None => text.push_str("- Dates: not known (no shift of this number on Inkipedia's list)\n"),
    }
    text.push_str(&alloc::format!(
        "- Stage: {stage}{}\n",
        stage_key
            .as_deref()
            .map(|k| alloc::format!(" ({k}, map id {})", scenario.map))
            .unwrap_or_default()
    ));
    text.push_str(&alloc::format!("- Weapons: {}\n", weapons.join("; ")));
    text.push_str(&alloc::format!("- Specials: {}\n", specials.join("; ")));
    text.push_str(&alloc::format!("- Waves: {}\n", waves.join("; ")));
    let snatcher: Vec<String> = scenario
        .snatcher
        .iter()
        .enumerate()
        .map(|(i, s)| {
            alloc::format!(
                "wave {}: {}",
                i + 1,
                letter(u64::from(*s).saturating_sub(1))
            )
        })
        .collect();
    text.push_str(&alloc::format!(
        "- Snatcher spawn point: {}\n",
        snatcher.join("; ")
    ));
    if let Some(e) = event {
        if let Some(p) = &e.prize {
            text.push_str(&alloc::format!("- Reward for taking part: {p}\n"));
        }
        if !e.thresholds.is_empty() {
            let t: Vec<String> = e
                .thresholds
                .iter()
                .map(|(k, v)| alloc::format!("top {k}: {v} golden eggs"))
                .collect();
            text.push_str(&alloc::format!(
                "- High score thresholds: {}\n",
                t.join(", ")
            ));
        }
        if let Some(note) = &e.note {
            text.push_str(&alloc::format!("- Note: {note}\n"));
        }
    }
    let url = page_url(n);
    text.push_str("Dates from Inkipedia's list of Eggstra Work shifts (CC BY-SA 4.0).\n");
    let mut out = alloc::vec![leanny::card(
        &overview_key(n),
        &url,
        title_head.clone(),
        text,
        version,
    )];
    for w in 1..=5 {
        let Some((occ, tide)) = scenario.wave_type(w) else {
            continue;
        };
        let title = alloc::format!("{title_head}, wave {w}: {}, {}", tide.name(), occ.name());
        let mut text = alloc::format!(
            "# {title}\n\nStage {stage}. Spawn schedule by hazard level, as the scenario holds it: the times are the wave timer (seconds left of 100), the spawn points the letters of Lean's map (A, B, C, with a number for the exact point).\n"
        );
        for hzl in scenario.hazard_levels(w) {
            let list = &scenario.spawns[w.to_string()][hzl.to_string()];
            let lines = spawn_lines(occ, list, names);
            if lines.is_empty() {
                continue;
            }
            text.push_str(&alloc::format!(
                "\n## Hazard level {}% (difficulty {hzl})\n\n{}\n",
                hazard_percent(hzl),
                lines.join("\n")
            ));
        }
        out.push(leanny::card(&wave_key(n, w), &url, title, text, version));
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A small module like Lean's: a standard wave, a Rush wave and a
    /// Goldie Seeking wave (the rest standard)
    pub(crate) const MODULE: &str = r#"export default {
    Snatcher: [4, 1, 3, 2, 1],
    Map: 9,
    Waves: [2, 3, 0, 11, 1],
    Weapons: ["Shooter_Normal", "Brush_Normal"],
    Specials: ["SpNiceBall", "SpChariot"],
    Spawns: {
        1: {
            300: [
                { Spawn: "C", Timing: 0, Lesser: true, },
                { Boss: "SakelienShield", Spawn: "C", Timing: 0, SubSpawn: 0, },
                { Boss: "SakelienBomber", Spawn: "B", Timing: 1080, SubSpawn: 2 },
                { Boss: "SakePillar", Spawn: "A", Timing: 2160, SubSpawn: 1 },
                { Boss: "SakelienGolden", Spawn: "A", Timing: 2523, SubSpawn: 0, Eggs: 5 },
            ],
        },
        2: {
            300: [ { Targets: [1, 0] }, { Spawn: "A", Timing: 600 } ],
            450: [ { Targets: [0, 1] }, { Spawn: "B", Timing: 600 } ],
        },
        3: { 300: [] },
        4: { 300: [[0, 1], [3, 2]] },
        5: { 300: [] }, // trailing comment
    },
}
"#;

    #[test]
    fn parses_javascript_object_literals() {
        let v = js_value(MODULE).unwrap();
        assert_eq!(v["Map"], json!(9));
        assert_eq!(v["Waves"], json!([2, 3, 0, 11, 1]));
        assert_eq!(v["Spawns"]["1"]["300"][2]["Boss"], json!("SakelienBomber"));
        assert_eq!(v["Spawns"]["4"]["300"], json!([[0, 1], [3, 2]]));
        assert_eq!(
            js_value("{a: 'it\\'s', b: -1.5, c: true, d: null}").unwrap(),
            json!({"a": "it's", "b": -1.5, "c": true, "d": null})
        );
        assert!(js_value("{a: 1} extra").is_err());
        assert!(js_value("{a: }").is_err());
    }

    #[test]
    fn reads_a_scenario() {
        let s = Scenario::parse(7, MODULE).unwrap();
        assert_eq!(s.map, 9);
        assert_eq!(s.wave_type(1), Some((Occurrence::Normal, Tide::High)));
        assert_eq!(s.wave_type(2), Some((Occurrence::Rush, Tide::Normal)));
        assert_eq!(s.wave_type(4), Some((Occurrence::Geyser, Tide::Normal)));
        assert_eq!(s.hazard_levels(2), [300, 450]);
        assert_eq!(s.weapons, ["Shooter_Normal", "Brush_Normal"]);
        assert_eq!(hazard_percent(1665), 333);
        assert_eq!(WAVE_TYPES.len(), 19);
        assert!(Scenario::parse(1, "export default { Map: 1, Waves: [1] }").is_err());
    }

    #[test]
    fn renders_spawn_lines() {
        let s = Scenario::parse(7, MODULE).unwrap();
        let names = Names::default();
        let lines = spawn_lines(Occurrence::Normal, &s.spawns["1"]["300"], &names);
        assert_eq!(
            lines,
            [
                "- 100 s left: lesser Salmonids from C",
                "- 100 s left: SakelienShield from C0",
                "- 82 s left: SakelienBomber from B2",
                "- 64 s left: SakePillar from FsA0",
                "- 57 s left: SakelienGolden carrying 5 golden eggs from A0",
            ]
        );
        let rush = spawn_lines(Occurrence::Rush, &s.spawns["2"]["300"], &names);
        assert_eq!(
            rush[0],
            "- Target order (the event's weapons in order): weapon 2 > weapon 1"
        );
        assert_eq!(rush[1], "- 90 s left: spawn area A");
        let goldie = spawn_lines(Occurrence::Geyser, &s.spawns["4"]["300"], &names);
        assert_eq!(goldie[0], "- Goldie 1: gusher A first, its goal gusher B");
        assert_eq!(goldie[1], "- Goldie 2: gusher D first, its goal gusher C");
    }

    /// Two shifts as the Inkipedia page writes them
    pub(crate) const WIKITEXT: &str = r#"{|class="wikitable"
{{Eggstra Work result
|start=2023-04-15
|stage=Sockeye Station
|weapon1=Splattershot
|weapon2=Blaster
|special1=Booyah Bomb
|special2=Crab Tank
|tide1=Low Tide
|tide2=Mid Tide
|wave1=Standard
|wave2=The Griller
|prize=Chum
|5%=203
|20%=169
|50%=123
|below%=122
}}

{{Eggstra Work result
|start=2024-11-09
|stage=Bonerattle Arena
|weapon1=[[Splattershot]]
|tide1=High Tide
|wave1=Standard
|prize=Drizzler
|note=A rerun<ref>{{TWI}} [[Inkipedia:Twitter archive/2026|@SplatoonNA]]</ref> of the {{date|2023-04-15}} event.
}}
|}"#;

    #[test]
    fn reads_inkipedia_shifts() {
        let shifts = parse_shifts(WIKITEXT);
        assert_eq!(shifts.len(), 2);
        assert_eq!(shifts[0].start.to_string(), "2023-04-15");
        assert_eq!(shifts[0].stage, "Sockeye Station");
        assert_eq!(shifts[0].weapons, ["Splattershot", "Blaster"]);
        assert_eq!(shifts[0].tides, ["Low Tide", "Mid Tide"]);
        assert_eq!(shifts[0].occurrences, ["Standard", "The Griller"]);
        assert_eq!(shifts[0].prize.as_deref(), Some("Chum"));
        assert_eq!(shifts[0].thresholds[0], (String::from("5%"), 203));
        assert_eq!(shifts[1].weapons, ["Splattershot"]);
        assert_eq!(
            shifts[1].note.as_deref(),
            Some("A rerun of the 2023-04-15 event.")
        );
        assert!(parse_shifts("nothing here").is_empty());
    }

    #[test]
    fn builds_the_table_and_finds_the_event_of_a_date() {
        let shifts = parse_shifts(WIKITEXT);
        let mut two = Scenario::parse(2, MODULE).unwrap();
        two.map = 9;
        let one = Scenario::parse(1, MODULE).unwrap();
        // Scenario 1 is on map 9 too, not Sockeye Station: not linked
        let stage_name = |id: u64| (id == 9).then(|| String::from("Bonerattle Arena"));
        let events = build_events(&shifts, &[one, two], &stage_name);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].scenario, None);
        assert_eq!(events[0].start.to_rfc3339(), "2023-04-15T00:00:00+00:00");
        assert_eq!(events[0].end.to_rfc3339(), "2023-04-17T00:00:00+00:00");
        assert_eq!(events[1].scenario, Some(2));
        assert_eq!(
            events[1].url.as_deref(),
            Some("https://leanny.github.io/eggstra_work/coop_event_02.html")
        );
        assert_eq!(events[1].label(), "#2 (2024-11-09, Bonerattle Arena)");
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(event_at(&events, at("2023-04-14T23:00:00Z")), None);
        assert_eq!(event_at(&events, at("2023-04-15T00:00:00Z")), Some(1));
        assert_eq!(event_at(&events, at("2023-04-23T12:00:00Z")), Some(1));
        assert_eq!(event_at(&events, at("2023-04-24T00:00:00Z")), None);
        assert_eq!(event_at(&events, at("2024-11-12T00:00:00Z")), Some(2));

        let root =
            std::env::temp_dir().join(alloc::format!("cuttlefish-eggstra-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(read_events(&root).unwrap().is_none());
        let table = Events {
            built_at: at("2026-09-26T00:00:00Z"),
            sources: alloc::vec![String::from("test")],
            events,
        };
        write_events(&root, &table).unwrap();
        assert_eq!(read_events(&root).unwrap(), Some(table));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn makes_an_overview_and_wave_cards() {
        let shifts = parse_shifts(WIKITEXT);
        let scenario = Scenario::parse(2, MODULE).unwrap();
        let names = Names::default();
        let events = build_events(&shifts, core::slice::from_ref(&scenario), &|id| {
            (id == 9).then(|| String::from("Bonerattle Arena"))
        });
        let docs = cards(&scenario, Some(&events[1]), &names, "11.3.0");
        assert_eq!(docs.len(), 6);
        let overview = &docs[0];
        assert_eq!(
            overview.title,
            "Eggstra Work #2 (2024-11-09, Bonerattle Arena)"
        );
        assert_eq!(overview.id, crate::doc::doc_id("leanny:eggstra/02"));
        assert!(
            overview
                .text
                .contains("- Dates: 2024-11-09 00:00 to 2024-11-11 00:00 UTC (48 hours)")
        );
        assert!(overview.text.contains(
            "- Weapons: Shooter_Normal_00 (Shooter_Normal); Brush_Normal_00 (Brush_Normal)"
        ));
        assert!(overview.text.contains("wave 2: Normal Tide, Rush"));
        assert!(
            overview
                .text
                .contains("- Snatcher spawn point: wave 1: D; wave 2: A")
        );
        assert!(overview.text.contains("- Reward for taking part: Drizzler"));
        assert!(
            overview
                .text
                .contains("Note: A rerun of the 2023-04-15 event.")
        );
        assert_eq!(overview.source, crate::doc::SourceKind::GameData);
        assert_eq!(overview.game, Some(crate::game::Game::S3));
        let wave2 = &docs[2];
        assert_eq!(
            wave2.title,
            "Eggstra Work #2 (2024-11-09, Bonerattle Arena), wave 2: Normal Tide, Rush"
        );
        assert!(wave2.text.contains("## Hazard level 60% (difficulty 300)"));
        assert!(wave2.text.contains("## Hazard level 90% (difficulty 450)"));
        assert!(wave2.text.contains("- 90 s left: spawn area B"));
        // Without a shift: no dates
        let alone = cards(&scenario, None, &names, "11.3.0");
        assert_eq!(alone[0].title, "Eggstra Work scenario 2 (stage 9)");
        assert!(alone[0].text.contains("- Dates: not known"));
        // A shift without a scenario: Inkipedia's facts alone
        let shift = shift_card(&events[0]);
        assert_eq!(shift.title, "Eggstra Work #1 (2023-04-15, Sockeye Station)");
        assert_eq!(shift.id, crate::doc::doc_id("leanny:eggstra/01"));
        assert!(shift.text.contains("- Weapons: Splattershot; Blaster"));
        assert!(shift.text.contains("wave 2: Mid Tide, The Griller"));
        assert_eq!(shift.license.as_deref(), Some(INKIPEDIA_LICENSE));
    }
}
