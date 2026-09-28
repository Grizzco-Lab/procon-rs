//! Game numbers as players read them. A fact card of Lean's datamine
//! ([`crate::leanny`]) keeps the data it was made from as [`Facts`]: its
//! kind, the game's key and the parameters as the game has them.
//! [`summary`] turns them into what matters in Salmon Run, in the units
//! players and Inkipedia use: a weapon's or special's damage (with its
//! falloff by distance), ink, times and sizes, how many hits the
//! Salmonids take ([`SALMONID_HP`]); a Salmonid's eggs and HP; a hazard
//! level's parameters by known occurrence. Every raw parameter stays in
//! the summary ([`Summary::raw`]) for experts, cosmetic ones (textures,
//! effects) included, and only there.
//!
//! Conversions, each checked against pages in the knowledge store (the
//! Inkipedia wiki documents), and no other: damage is stored ×10
//! ([`DAMAGE_SCALE`]), ink as a fraction of the tank (shown in percent,
//! [`PERCENT`]), times in frames at 60 per second ([`FPS`]). Distances and
//! radii stay in the game's own units ([`Unit::Units`]), as Inkipedia
//! quotes them.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The data stores damage ×10: Inkipedia's "Salmon Run Next Wave data"
/// (<https://splatoonwiki.org/wiki/Salmon_Run_Next_Wave_data>, revision
/// 711534, in the knowledge store), section "Weapon statistics", gives the
/// Splattershot 36 / 30 damage in Salmon Run where its Salmon Run table has
/// `DamageParam.ValueMax` 360 and `ValueMin` 300, the Splat Charger 300
/// (`ValueFullCharge` 3000), the Splat Roller 125 (`BodyParam.Damage`
/// 1250), the Wave Breaker 150 (`WaveParam.Damage` 1500) and the Triple
/// Splashdown 300 (`DistanceDamage` 3000); its Salmonid HP are in the same
/// units. Inkipedia's "Grizzco Dualies" page: its dodge-roll blast deals
/// 125 within 4.0 units, down to 40 at 6.0 (`SideStepBlastParam.
/// DistanceDamage` 1250 at 4.0, 400 at 6.0).
pub const DAMAGE_SCALE: f64 = 10.0;

/// Ink is stored as a fraction of the tank; players read percent: the same
/// Inkipedia page's "Ink usage: the percent of the ink tank's capacity
/// consumed by each shot" gives the Splattershot 0.92 (`InkConsume`
/// 0.0092) and the Splat Charger 18 / 2.25 (`InkConsumeFullCharge` 0.18,
/// `InkConsumeMinCharge` 0.0225).
pub const PERCENT: f64 = 100.0;

/// Frames per second of the game's timings: Inkipedia's "Grizzco Brella"
/// ("a 20 frame (0.33 second) cooldown before the ink tank starts
/// refilling", `InkRecoverStopCharge` 20), "Grizzco Stringer" ("the shots
/// explode after 45 frames (0.75 seconds)", `DetonationFrame` 45) and
/// "Grizzco Dualies" ("a bullet every 5 frames (12 shots per second)",
/// `RepeatFrame` 5).
pub const FPS: f64 = 60.0;

/// Where the Salmonids' HP are from
pub const HP_SOURCE: &str = "Inkipedia, Salmon Run Next Wave data";
pub const HP_URL: &str = "https://splatoonwiki.org/wiki/Salmon_Run_Next_Wave_data";

/// A Salmonid's HP (or one part's), in the units of [`DAMAGE_SCALE`]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hp {
    /// The game's key (`SakelienStandard`)
    pub key: &'static str,
    /// Its English name
    pub name: &'static str,
    /// The part hit, when not the Salmonid itself (`bomb`)
    pub part: Option<&'static str>,
    pub hp: f64,
}

/// The HP of the Salmonids with one plain number in the table "Salmonids"
/// of Inkipedia's "Salmon Run Next Wave data" (revision 711534, in the
/// knowledge store): those with parts or ranges (Flyfish, Scrapper,
/// Stinger, Fish Stick, the King Salmonids) are left out, but for the
/// Steelhead's bomb, which is what players shoot
pub const SALMONID_HP: &[Hp] = &[
    Hp {
        key: "SakelienSmall",
        name: "Smallfry",
        part: None,
        hp: 40.0,
    },
    Hp {
        key: "SakeCopter",
        name: "Chinook",
        part: None,
        hp: 50.0,
    },
    Hp {
        key: "SakelienStandard",
        name: "Chum",
        part: None,
        hp: 100.0,
    },
    Hp {
        key: "SakeFlyBagman",
        name: "Snatcher",
        part: None,
        hp: 100.0,
    },
    Hp {
        key: "SakelienBomber",
        name: "Steelhead",
        part: Some("bomb"),
        hp: 300.0,
    },
    Hp {
        key: "SakelienLarge",
        name: "Cohock",
        part: None,
        hp: 400.0,
    },
    Hp {
        key: "SakelienGolden",
        name: "Goldie",
        part: None,
        hp: 500.0,
    },
    Hp {
        key: "SakelienSnake",
        name: "Steel Eel",
        part: None,
        hp: 500.0,
    },
    Hp {
        key: "SakeSaucer",
        name: "Slammin' Lid",
        part: None,
        hp: 500.0,
    },
    Hp {
        key: "SakeBigMouth",
        name: "Mudmouth",
        part: None,
        hp: 540.0,
    },
    Hp {
        key: "Sakerocket",
        name: "Drizzler",
        part: None,
        hp: 900.0,
    },
    Hp {
        key: "Sakediver",
        name: "Maws",
        part: None,
        hp: 1200.0,
    },
    Hp {
        key: "SakeArtillery",
        name: "Big Shot",
        part: None,
        hp: 1200.0,
    },
    Hp {
        key: "SakeDolphin",
        name: "Flipper-Flopper",
        part: None,
        hp: 1200.0,
    },
    Hp {
        key: "Sakedozer",
        name: "Griller",
        part: None,
        hp: 2200.0,
    },
];

/// What a fact card is about
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FactKind {
    Weapon,
    Special,
    Salmonid,
    Stage,
    Level,
}

/// The structured data behind a fact card, kept in its document
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    pub kind: FactKind,
    /// The game's key (`SpSuperLanding_Coop`, `SakelienBomber`)
    pub key: String,
    /// The battle form's key, for a Salmon Run weapon or special
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub versus: Option<String>,
    /// The game version the data is from (`11.3.0`)
    pub version: String,
    /// Whether it is a Grizzco weapon
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grizzco: Option<bool>,
    /// The Eggstra Work events with it (a weapon, special or stage):
    /// `#1 (2023-04-15)`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    /// A King Salmonid's HP coefficient by hazard level (percent)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hp_coef: Vec<(u32, f64)>,
    /// The parameters as the game has them: a weapon's or special's
    /// `GameParameters` (Salmon Run form, its parent's merged in), a
    /// Salmonid's `CoopEnemyInfo` row, a stage's `CoopSceneInfo` row, a
    /// hazard level of `CoopLevelsConfig`
    #[serde(default)]
    pub params: Value,
}

/// The unit a summary's numbers are in, after conversion
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Damage as players see it
    Damage,
    /// Percent of the ink tank
    Percent,
    /// Percent of the ink tank per frame
    PercentPerFrame,
    /// Frames at [`FPS`]
    Frames,
    /// The game's distance units, unconverted
    Units,
    /// Power Eggs
    Eggs,
    /// Hit points, in damage units
    Hp,
    /// A number as the game has it
    Plain,
}

/// One line of a summary: a statistic (`damage`), what it is of (`group`:
/// `explosion`, `vertical_flick`; empty for the main attack), its values
/// (the largest first, then the smallest when they differ) or a text, and
/// the raw parameters it is from
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub stat: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub unit: Unit,
    pub from: Vec<String>,
}

/// A small two-column table: damage by distance (`falloff`: up to each
/// distance, in game units) or a King Salmonid's HP coefficient by hazard
/// level (`hp_coef`)
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Table {
    pub stat: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    pub rows: Vec<(f64, f64)>,
    pub from: String,
}

/// How many hits at the most damage per hit a Salmonid takes
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    pub key: String,
    /// Its English name ([`Summary::names`] has the others)
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    pub hp: f64,
    pub hits: u32,
}

/// What a fact card shows players
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Summary {
    pub kind: FactKind,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versus: Option<String>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grizzco: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
    pub rows: Vec<Row>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tables: Vec<Table>,
    /// Hits to defeat each Salmonid of [`SALMONID_HP`], fewest first
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hits: Vec<Hit>,
    /// The page the HP are from, when the summary has some ([`HP_URL`])
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hp_url: Option<&'static str>,
    /// Every parameter as the game has it: path and value
    pub raw: Vec<(String, String)>,
    /// The glossary's terms for the game keys and English names the
    /// summary uses, filled in by whoever has the glossary
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub names: BTreeMap<String, Named>,
}

/// A glossary term a summary names: its id and main name per language
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Named {
    pub id: String,
    pub names: BTreeMap<String, String>,
}

impl Summary {
    /// The keys and English names [`Summary::names`] is for: the Salmonids
    /// of the hits, and a hazard level's occurrences and game keys
    pub fn named(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.hits.iter().map(|h| h.name.as_str()).collect();
        if self.kind == FactKind::Level {
            for r in &self.rows {
                out.push(&r.group);
                out.push(&r.stat);
                // A King's HP coefficient (`SakelienGiantHPCoef`)
                if let Some(key) = r.stat.strip_suffix("HPCoef") {
                    out.push(key);
                }
            }
        }
        out.retain(|s| !s.is_empty());
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Longest raw value shown whole; a longer one (a curve of hundreds of
/// points) is its item count
pub const MAX_RAW_CHARS: usize = 400;

/// Every leaf of `value` (arrays are leaves) with its path, `$type` and
/// `$parent` left out
fn leaves<'a>(value: &'a Value, path: &mut Vec<&'a str>, out: &mut Vec<(Vec<&'a str>, &'a Value)>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if k.starts_with('$') {
                    continue;
                }
                path.push(k);
                leaves(v, path, out);
                path.pop();
            }
        }
        v => out.push((path.clone(), v)),
    }
}

/// The raw parameters as `(path, value)`
fn raw(params: &Value) -> Vec<(String, String)> {
    let mut all = Vec::new();
    leaves(params, &mut Vec::new(), &mut all);
    all.into_iter()
        .map(|(path, v)| {
            let text = match v {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            };
            let text = if text.chars().count() > MAX_RAW_CHARS {
                alloc::format!("({} values)", v.as_array().map_or(0, Vec::len))
            } else {
                text
            };
            (path.join("."), text)
        })
        .collect()
}

/// Leaves holding damage whatever their parent is called
const DAMAGE_LEAVES: &[&str] = &[
    "Damage",
    "DamageValue",
    "DamageValueStart",
    "DamageValueEnd",
    "HitDamage",
    "DirectDamage",
    "CanopyDamage",
    "LaserDamage",
    "DamageDashValue",
    "DamageJumpValue",
    "DirectHitDamageMax",
    "DirectHitDamageMid",
    "DirectHitDamageMin",
    "DamageMaxValue",
    "DamageHighValue",
    "DamageLowValue",
    "DamageMinValue",
    "FinalDamageMinValue",
    "DamageEffectiveTotalMax",
];

/// What a weapon's or special's number is: the statistic, its unit and
/// the factor that converts it; `None` for what the summary leaves to the
/// raw parameters
fn stat_of(path: &[&str]) -> Option<(&'static str, Unit, f64)> {
    let (leaf, parents) = path.split_last()?;
    let in_damage = parents.iter().any(|p| p.contains("Damage"));
    Some(match *leaf {
        l if DAMAGE_LEAVES.contains(&l) || (in_damage && l.starts_with("Value")) => {
            ("damage", Unit::Damage, 1.0 / DAMAGE_SCALE)
        }
        l if l.starts_with("InkConsume") && l.ends_with("PerFrame") => {
            ("ink_per_frame", Unit::PercentPerFrame, PERCENT)
        }
        l if l.starts_with("InkConsume") && !l.contains("Rate") => ("ink", Unit::Percent, PERCENT),
        "RepeatFrame" | "LapOver_RepeatFrame" => ("fire_interval", Unit::Frames, 1.0),
        "ChargeFrameFullCharge" => ("full_charge", Unit::Frames, 1.0),
        l if l.starts_with("InkRecoverStop") && !l.contains("ChargeKeep") => {
            ("ink_recovery", Unit::Frames, 1.0)
        }
        "DetonationFrame" => ("detonation", Unit::Frames, 1.0),
        "SpecialTotalFrame" => ("duration", Unit::Frames, 1.0),
        "PaintRadius" if parents.len() == 1 => ("paint_radius", Unit::Units, 1.0),
        "DamageRadiusEnd" => ("damage_radius", Unit::Units, 1.0),
        "DistanceFullCharge" => ("range", Unit::Units, 1.0),
        _ => return None,
    })
}

/// What part of the weapon a parameter is of, from words in its path; the
/// first that matches (empty: the main attack)
const GROUPS: &[(&str, &str)] = &[
    ("lapover", "after_roll"),
    ("sidestep", "dodge_roll"),
    ("slashvertical", "vertical_slash"),
    ("slashhorizontal", "horizontal_slash"),
    ("sabervertical", "vertical_projectile"),
    ("saberhorizontal", "horizontal_projectile"),
    ("chargeswing", "charged_swing"),
    ("verticalswing", "vertical_flick"),
    ("wideswing", "horizontal_flick"),
    ("swingunitgroup", "flick"),
    ("umbrella", "canopy"),
    ("canopy", "canopy"),
    ("shotgun", "pellets"),
    ("cannon", "cannon"),
    ("detonation", "explosion"),
    ("blast", "explosion"),
    ("laser", "laser"),
    ("jet", "jet"),
    ("waveparam", "shockwave"),
    ("shooterdamage", "turret"),
    ("body", "contact"),
];

fn group_of(path: &[&str]) -> &'static str {
    let joined = path.join(".").to_lowercase();
    GROUPS
        .iter()
        .find(|(word, _)| joined.contains(word))
        .map_or("", |(_, group)| group)
}

/// The order statistics are listed in
const STAT_ORDER: &[&str] = &[
    "damage",
    "damage_radius",
    "paint_radius",
    "range",
    "ink",
    "ink_per_frame",
    "fire_interval",
    "full_charge",
    "detonation",
    "duration",
    "ink_recovery",
];

/// Rounds away the float noise of a conversion (0.0092 × 100)
fn tidy(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

/// A weapon's or special's rows and falloff tables
fn attack(params: &Value) -> (Vec<Row>, Vec<Table>) {
    let mut all = Vec::new();
    leaves(params, &mut Vec::new(), &mut all);
    let mut rows: Vec<Row> = Vec::new();
    let mut tables = Vec::new();
    let mut add = |stat: &str, group: &str, unit: Unit, x: f64, from: String| match rows
        .iter_mut()
        .find(|r| r.stat == stat && r.group == group)
    {
        Some(r) => {
            r.values.push(x);
            r.from.push(from);
        }
        None => rows.push(Row {
            stat: String::from(stat),
            group: String::from(group),
            values: alloc::vec![x],
            text: None,
            unit,
            from: alloc::vec![from],
        }),
    };
    for (path, v) in &all {
        let from = path.join(".");
        if path.last() == Some(&"DistanceDamage") {
            let mut points: Vec<(f64, f64)> = v
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    Some((
                        p["Distance"].as_f64()?,
                        tidy(p["Damage"].as_f64()? / DAMAGE_SCALE),
                    ))
                })
                .filter(|(_, d)| *d > 0.0)
                .collect();
            points.sort_by(|a, b| a.0.total_cmp(&b.0));
            let group = group_of(path);
            let Some(&(reach, first)) = points.last() else {
                continue;
            };
            // One damage all the way: a damage and its radius
            if points.iter().all(|(_, d)| *d == first) {
                add("damage", group, Unit::Damage, first, from.clone());
                add("damage_radius", group, Unit::Units, reach, from);
            } else {
                tables.push(Table {
                    stat: String::from("falloff"),
                    group: String::from(group),
                    rows: points,
                    from,
                });
            }
            continue;
        }
        let Some(x) = v.as_f64().filter(|x| *x > 0.0) else {
            continue;
        };
        if let Some((stat, unit, factor)) = stat_of(path) {
            add(stat, group_of(path), unit, tidy(x * factor), from);
        }
    }
    for r in &mut rows {
        r.values.sort_by(|a, b| b.total_cmp(a));
        r.values.dedup();
        if r.values.len() > 2 {
            let min = r.values[r.values.len() - 1];
            r.values.truncate(1);
            r.values.push(min);
        }
    }
    // Beside an explosion, the damage of a `DamageParam` is the direct
    // hit's (Inkipedia's "Salmon Run Next Wave data": "for blasters, this
    // is the amount of damage dealt with a direct hit"; a Stringer's is
    // `DirectHitDamage`)
    let blast = rows
        .iter()
        .any(|r| r.stat == "damage" && r.group == "explosion")
        || tables.iter().any(|t| t.group == "explosion");
    for r in rows.iter_mut().filter(|r| {
        blast
            && r.stat == "damage"
            && r.group.is_empty()
            && r.from.iter().all(|f| f.contains("DamageParam"))
    }) {
        r.group = String::from("direct");
    }
    // By statistic, the main attack first
    rows.sort_by_key(|r| {
        (
            STAT_ORDER.iter().position(|s| *s == r.stat),
            !matches!(r.group.as_str(), "" | "direct"),
        )
    });
    (rows, tables)
}

/// The most damage one hit of the rows and tables does
fn most_damage(rows: &[Row], tables: &[Table]) -> Option<f64> {
    rows.iter()
        .filter(|r| r.unit == Unit::Damage)
        .flat_map(|r| r.values.first().copied())
        .chain(
            tables
                .iter()
                .filter(|t| t.stat == "falloff")
                .flat_map(|t| t.rows.iter().map(|(_, d)| *d)),
        )
        .max_by(f64::total_cmp)
}

/// Hits to defeat each Salmonid of [`SALMONID_HP`] at `damage` per hit
pub fn hits(damage: f64) -> Vec<Hit> {
    if damage <= 0.0 {
        return Vec::new();
    }
    let mut out: Vec<Hit> = SALMONID_HP
        .iter()
        .map(|h| Hit {
            key: String::from(h.key),
            name: String::from(h.name),
            part: h.part.map(String::from),
            hp: h.hp,
            hits: (h.hp / damage).ceil() as u32,
        })
        .collect();
    out.sort_by(|a, b| a.hits.cmp(&b.hits).then(a.hp.total_cmp(&b.hp)));
    out
}

fn row(stat: &str, group: &str, values: Vec<f64>, unit: Unit, from: &str) -> Row {
    Row {
        stat: String::from(stat),
        group: String::from(group),
        values,
        text: None,
        unit,
        from: alloc::vec![String::from(from)],
    }
}

/// A Salmonid's rows: its category, how many at once, its eggs and, from
/// [`SALMONID_HP`], its HP
fn salmonid(facts: &Facts) -> (Vec<Row>, Vec<Table>) {
    let p = &facts.params;
    let mut rows = Vec::new();
    if let Some(c) = p["Category"].as_str() {
        rows.push(Row {
            text: Some(String::from(c)),
            ..row("category", "", Vec::new(), Unit::Plain, "Category")
        });
    }
    for h in SALMONID_HP.iter().filter(|h| h.key == facts.key) {
        rows.push(row(
            "hp",
            h.part.unwrap_or_default(),
            alloc::vec![h.hp],
            Unit::Hp,
            HP_SOURCE,
        ));
    }
    for (key, stat, unit) in [
        ("ActorMax", "at_once", Unit::Plain),
        ("HitIkuraNum", "eggs_per_hit", Unit::Eggs),
        ("KillIkuraNum", "eggs_on_kill", Unit::Eggs),
    ] {
        if let Some(x) = p[key].as_f64() {
            rows.push(row(stat, "", alloc::vec![x], unit, key));
        }
    }
    let tables = if facts.hp_coef.is_empty() {
        Vec::new()
    } else {
        alloc::vec![Table {
            stat: String::from("hp_coef"),
            group: String::new(),
            rows: facts
                .hp_coef
                .iter()
                .map(|(h, c)| (f64::from(*h), *c))
                .collect(),
            from: alloc::format!("CoopLevelsConfig {}HPCoef", facts.key),
        }]
    };
    (rows, tables)
}

/// A stage's rows: whether it is a Big Run stage
fn stage(facts: &Facts) -> Vec<Row> {
    facts.params["IsBigRun"]
        .as_bool()
        .map(|b| Row {
            text: Some(String::from(if b { "yes" } else { "no" })),
            ..row("big_run", "", Vec::new(), Unit::Plain, "IsBigRun")
        })
        .into_iter()
        .collect()
}

/// A hazard level's numbers, each under the known occurrence it sets
/// (`EventRush` under Rush; the English name as the group), as the game
/// has them: what they measure is not documented, so nothing is converted
fn level(params: &Value) -> Vec<Row> {
    let mut all = Vec::new();
    leaves(params, &mut Vec::new(), &mut all);
    all.into_iter()
        .filter_map(|(path, v)| {
            let x = v.as_f64()?;
            let (leaf, parents) = path.split_last()?;
            let group = parents
                .first()
                .and_then(|p| p.strip_prefix("Event"))
                .and_then(crate::eggstra::Occurrence::from_key)
                .map_or_else(|| parents.join("."), |o| String::from(o.name()));
            Some(row(
                leaf,
                &group,
                alloc::vec![x],
                Unit::Plain,
                &path.join("."),
            ))
        })
        .collect()
}

/// What players read of a card's facts
pub fn summary(facts: &Facts) -> Summary {
    let (rows, tables, hits) = match facts.kind {
        FactKind::Weapon | FactKind::Special => {
            let (rows, tables) = attack(&facts.params);
            let hits = most_damage(&rows, &tables).map(hits).unwrap_or_default();
            (rows, tables, hits)
        }
        FactKind::Salmonid => {
            let (rows, tables) = salmonid(facts);
            (rows, tables, Vec::new())
        }
        FactKind::Stage => (stage(facts), Vec::new(), Vec::new()),
        FactKind::Level => (level(&facts.params), Vec::new(), Vec::new()),
    };
    let hp_url = (!hits.is_empty() || rows.iter().any(|r| r.stat == "hp")).then_some(HP_URL);
    Summary {
        hp_url,
        kind: facts.kind,
        key: facts.key.clone(),
        versus: facts.versus.clone(),
        version: facts.version.clone(),
        grizzco: facts.grizzco,
        events: facts.events.clone(),
        rows,
        tables,
        hits,
        raw: raw(&facts.params),
        names: BTreeMap::new(),
    }
}

/// A number as written in the lines: no trailing zeros
fn num(x: f64) -> String {
    let s = alloc::format!("{:.3}", tidy(x));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    String::from(s)
}

/// A value with its unit, in English
fn with_unit(x: f64, unit: Unit) -> String {
    match unit {
        Unit::Percent => alloc::format!("{}% of the tank", num(x)),
        Unit::PercentPerFrame => alloc::format!("{}% of the tank per frame", num(x)),
        Unit::Frames => alloc::format!("{} frames ({} s)", num(x), num(x / FPS)),
        Unit::Units => alloc::format!("{} game units", num(x)),
        Unit::Eggs => alloc::format!("{} Power Eggs", num(x)),
        Unit::Damage | Unit::Hp | Unit::Plain => num(x),
    }
}

/// The weapon and special summaries in English lines for the card's text
/// (what the model reads): the statistics with their units, falloff and
/// hits; nothing for the other kinds, whose text says it already
pub fn lines(s: &Summary) -> Vec<String> {
    if !matches!(s.kind, FactKind::Weapon | FactKind::Special) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let of = |group: &str| {
        if group.is_empty() {
            String::new()
        } else {
            alloc::format!(" ({})", group.replace('_', " "))
        }
    };
    for r in &s.rows {
        let values: Vec<String> = r.values.iter().map(|x| with_unit(*x, r.unit)).collect();
        out.push(alloc::format!(
            "- {}{}: {}",
            r.stat.replace('_', " "),
            of(&r.group),
            values.join(", down to ")
        ));
    }
    for t in &s.tables {
        let points: Vec<String> = t
            .rows
            .iter()
            .map(|(d, x)| alloc::format!("{} up to {}", num(*x), num(*d)))
            .collect();
        out.push(alloc::format!(
            "- damage by distance{} (game units): {}",
            of(&t.group),
            points.join(", ")
        ));
    }
    if !s.hits.is_empty() {
        let hits: Vec<String> = s
            .hits
            .iter()
            .map(|h| match &h.part {
                Some(part) => alloc::format!("{}'s {part} {}", h.name, h.hits),
                None => alloc::format!("{} {}", h.name, h.hits),
            })
            .collect();
        out.push(alloc::format!(
            "- hits to defeat at the most damage per hit (HP from {HP_SOURCE}): {}",
            hits.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts(kind: FactKind, key: &str, params: Value) -> Facts {
        Facts {
            kind,
            key: key.into(),
            versus: None,
            version: "11.3.0".into(),
            grizzco: None,
            events: Vec::new(),
            hp_coef: Vec::new(),
            params,
        }
    }

    #[test]
    fn splashdown_reads_as_damage_radius_and_hits() {
        let s = summary(&facts(
            FactKind::Special,
            "SpSuperLanding_Coop",
            json!({"spl__BulletBlastParam": {
                "$type": "spl__BulletBlastParam",
                "CrossPaintTexture": "SprLanding00",
                "DistanceDamage": [
                    {"Damage": 7000, "Distance": 7.0},
                    {"Damage": 7000, "Distance": 10.0},
                    {"Damage": 7000, "Distance": 15.0}
                ],
                "KnockBackParam": {"Accel": 420.0, "Distance": 15.0},
                "PaintRadius": 14.0
            }}),
        ));
        let stats: Vec<(&str, &str, &[f64])> = s
            .rows
            .iter()
            .map(|r| (r.stat.as_str(), r.group.as_str(), r.values.as_slice()))
            .collect();
        assert_eq!(
            stats,
            [
                ("damage", "explosion", &[700.0][..]),
                ("damage_radius", "explosion", &[15.0][..]),
                ("paint_radius", "explosion", &[14.0][..]),
            ]
        );
        assert!(s.tables.is_empty());
        // Cosmetic and knockback numbers only in the raw parameters
        assert!(s.raw.contains(&(
            "spl__BulletBlastParam.CrossPaintTexture".into(),
            "SprLanding00".into()
        )));
        assert!(!s.raw.iter().any(|(k, _)| k.contains("$type")));
        let one: Vec<&str> = s
            .hits
            .iter()
            .filter(|h| h.hits == 1)
            .map(|h| h.name.as_str())
            .collect();
        assert!(
            one.contains(&"Chum") && one.contains(&"Mudmouth"),
            "{one:?}"
        );
        assert_eq!(s.hits.iter().find(|h| h.name == "Griller").unwrap().hits, 4);
    }

    #[test]
    fn shooters_convert_damage_ink_and_frames() {
        let s = summary(&facts(
            FactKind::Weapon,
            "Shooter_Normal_Coop",
            json!({
                "DamageParam": {"ReduceEndFrame": 40, "ReduceStartFrame": 8, "ValueMax": 360, "ValueMin": 300},
                "WeaponParam": {"InkConsume": 0.0092, "RepeatFrame": 6, "MoveSpeed": 0.072}
            }),
        ));
        let damage = &s.rows[0];
        assert_eq!(
            (damage.stat.as_str(), damage.values.as_slice()),
            ("damage", &[36.0, 30.0][..])
        );
        let ink = s.rows.iter().find(|r| r.stat == "ink").unwrap();
        assert_eq!(ink.values, [0.92]);
        assert_eq!(ink.unit, Unit::Percent);
        let fire = s.rows.iter().find(|r| r.stat == "fire_interval").unwrap();
        assert_eq!(
            (fire.values.as_slice(), fire.unit),
            (&[6.0][..], Unit::Frames)
        );
        assert_eq!(s.hits.iter().find(|h| h.name == "Chum").unwrap().hits, 3);
        let text = lines(&s).join("\n");
        assert!(text.contains("- damage: 36, down to 30"), "{text}");
        assert!(text.contains("- fire interval: 6 frames (0.1 s)"), "{text}");
    }

    #[test]
    fn a_blasters_direct_hit_comes_before_its_explosion() {
        let s = summary(&facts(
            FactKind::Weapon,
            "Blaster_Bear_Coop",
            json!({
                "BlastParam": {"DistanceDamage": [{"Damage": 350, "Distance": 3.5}]},
                "DamageParam": {"ValueMax": 500, "ValueMin": 500}
            }),
        ));
        let damage: Vec<(&str, &[f64])> = s
            .rows
            .iter()
            .filter(|r| r.stat == "damage")
            .map(|r| (r.group.as_str(), r.values.as_slice()))
            .collect();
        assert_eq!(
            damage,
            [("direct", &[50.0][..]), ("explosion", &[35.0][..])]
        );
    }

    #[test]
    fn falloff_stays_a_table_and_rollers_group_their_flicks() {
        let s = summary(&facts(
            FactKind::Weapon,
            "Maneuver_Bear_Coop",
            json!({
                "SideStepBlastParam": {"DistanceDamage": [{"Damage": 400, "Distance": 6.0}, {"Damage": 1250, "Distance": 4.0}]},
                "VerticalSwingUnitGroupParam": {"DamageParam": {
                    "Inside": {"DamageMaxValue": 2000, "DamageMinValue": 700, "DamageMaxDistance": 5.2},
                    "Outside": {"DamageMaxValue": 2000, "DamageHighValue": 1500, "DamageMinValue": 700}
                }}
            }),
        ));
        assert_eq!(s.tables.len(), 1);
        assert_eq!(s.tables[0].group, "dodge_roll");
        assert_eq!(s.tables[0].rows, [(4.0, 125.0), (6.0, 40.0)]);
        let flick = s.rows.iter().find(|r| r.group == "vertical_flick").unwrap();
        assert_eq!(flick.values, [200.0, 70.0]);
        assert_eq!(flick.from.len(), 5);
    }

    #[test]
    fn salmonids_levels_and_stages() {
        let mut f = facts(
            FactKind::Salmonid,
            "SakelienBomber",
            json!({"Type": "SakelienBomber", "Category": "Rare", "ActorMax": 4, "HitIkuraNum": 8, "KillIkuraNum": 7}),
        );
        f.hp_coef = alloc::vec![(40, 1.0)];
        let s = summary(&f);
        let hp = s.rows.iter().find(|r| r.stat == "hp").unwrap();
        assert_eq!(
            (hp.group.as_str(), hp.values.as_slice()),
            ("bomb", &[300.0][..])
        );
        assert_eq!(s.rows[0].text.as_deref(), Some("Rare"));
        assert_eq!(s.tables[0].stat, "hp_coef");

        let s = summary(&facts(
            FactKind::Level,
            "1000",
            json!({"Difficulty": 1000, "EventRush": {"ZakoSpeedCoef": 3.5}, "ZakoAppearWeight": {"SakelienLarge": 4}}),
        ));
        let groups: Vec<(&str, &str)> = s
            .rows
            .iter()
            .map(|r| (r.stat.as_str(), r.group.as_str()))
            .collect();
        assert_eq!(
            groups,
            [
                ("Difficulty", ""),
                ("ZakoSpeedCoef", "Rush"),
                ("SakelienLarge", "ZakoAppearWeight")
            ]
        );

        let s = summary(&facts(
            FactKind::Stage,
            "Shakeup",
            json!({"IsBigRun": false}),
        ));
        assert_eq!(s.rows[0].text.as_deref(), Some("no"));
    }
}
