//! Message folders: one folder per language, one file per category, as
//! Yii applications keep their translations (`messages/<lang>/<category>.php`,
//! stat.ink among them) and as `.json` or `.yaml` locale trees do.
//!
//! [`layout`] recognizes such a path: the file's parent names a language
//! (`zh-CN`, `en-GB`, `ja`); the file's stem is the category. The category
//! says what the names are ([`category`]: `salmon-boss3` holds boss
//! Salmonids, `map3` stages, `weapon3` weapons) and which game they belong
//! to ([`game`]: a trailing `3` or `2`; a category without one next to a
//! suffixed sibling is Splatoon 1). Categories of interface and site text
//! (`app`, `email`, `privacy`, time zones, ...) are not names and are
//! skipped, as are machine-translated folders (`_deepl`).

use crate::tables::{language, variant};
use alloc::string::String;

/// What a message category holds
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// Interface or site text: no names
    Site,
    /// Names, of this kind when known (`boss`, `stage`, `weapon`, ...)
    Names(Option<&'static str>),
}

/// A file of a message folder
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageFile {
    /// The language the folder names
    pub language: &'static str,
    /// Its regional variant, when the folder names one (`en-GB`, `es-MX`)
    pub variant: Option<&'static str>,
    /// The file's stem (`salmon-boss3`)
    pub category: String,
    /// What the category holds
    pub kind: Category,
    /// Game of the category from its suffix (`S3`, `S2`); `None` without one
    pub game: Option<&'static str>,
    /// In a machine-translated folder (`_deepl`)
    pub machine: bool,
}

/// Folders of machine translations
const MACHINE: [&str; 3] = ["_deepl", "deepl", "machine"];

/// The message file a path names, if its parent folder is a language and
/// it is a `.php`, `.json`, `.yaml` or `.yml` file
pub fn layout(rel: &str) -> Option<MessageFile> {
    let mut parts = rel.rsplit('/');
    let file = parts.next()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || !matches!(ext.to_lowercase().as_str(), "php" | "json" | "yaml" | "yml") {
        return None;
    }
    let folder = parts.next()?;
    let language = language(folder)?;
    let category = stem.to_lowercase();
    Some(MessageFile {
        language,
        variant: variant(folder),
        kind: category_kind(&category),
        game: game(&category),
        category,
        machine: rel
            .split('/')
            .any(|p| MACHINE.contains(&p.to_lowercase().as_str())),
    })
}

/// The game a category's suffix names: `weapon3` → `S3`, `map2` → `S2`
pub fn game(category: &str) -> Option<&'static str> {
    match category.chars().next_back()? {
        '3' => Some("S3"),
        '2' => Some("S2"),
        _ => None,
    }
}

/// A category without a game suffix (`map`) is the first game's when a
/// suffixed sibling exists (`map2` or `map3`)
pub fn first_game(category: &str, exists: impl Fn(&str) -> bool) -> Option<&'static str> {
    if game(category).is_some() {
        return None;
    }
    ["2", "3"]
        .iter()
        .any(|s| exists(&alloc::format!("{category}{s}")))
        .then_some("S1")
}

/// Categories of interface and site text
const SITE: [&str; 24] = [
    "app",
    "alert",
    "apidoc",
    "cookie",
    "counter",
    "email",
    "festpower",
    "freshness",
    "gearstat",
    "link",
    "privacy",
    "region",
    "reltime",
    "salmon-history",
    "slack",
    "start",
    "tz",
    "ua_vars",
    "ua_vars_v",
    "version",
    "weapon-short",
    "xmatch",
    "site",
    "common",
];

/// What a category holds, from its stem without the game suffix
pub fn category(category: &str) -> Category {
    category_kind(&category.to_lowercase())
}

fn category_kind(category: &str) -> Category {
    let base = category.trim_end_matches(|c: char| c.is_ascii_digit());
    if SITE.contains(&base) {
        return Category::Site;
    }
    let kind = match base {
        "salmon-boss" | "boss" | "bosses" => "boss",
        "salmon-event" | "event" | "events" => "event",
        "salmon-title" | "title" | "titles" => "title",
        "salmon-uniform" | "uniform" => "uniform",
        "salmon-scale" | "scale" => "scale",
        "salmon-map" | "map" | "maps" | "stage" | "stages" => "stage",
        "salmon-tide" | "tide" => "tide",
        "salmon" | "salmon-overfishing" => "salmon-run",
        "subweapon" | "sub" | "subs" => "sub",
        "special" | "specials" => "special",
        "ability" | "abilities" => "ability",
        "brand" | "brands" => "brand",
        "medal" | "medals" => "medal",
        "rank" | "ranks" => "rank",
        "season" | "seasons" => "season",
        "rule" | "rules" | "lobby" | "mode" | "modes" => "mode",
        "fest" | "conch-clash" | "splatfest" => "splatfest",
        "death" => "death",
        w if w == "weapon" || w == "weapons" || w.starts_with("weapon-") => "weapon",
        g if g == "gear" || g.starts_with("gear-") => "gear",
        _ => return Category::Names(None),
    };
    Category::Names(Some(kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_message_folders() {
        let boss = layout("stat.ink.zip/stat.ink-3.128.4/messages/zh-CN/salmon-boss3.php").unwrap();
        assert_eq!(boss.language, "zh");
        assert_eq!(boss.variant, None);
        assert_eq!(boss.category, "salmon-boss3");
        assert_eq!(boss.kind, Category::Names(Some("boss")));
        assert_eq!(boss.game, Some("S3"));
        assert!(!boss.machine);
        let gb = layout("messages/en-GB/weapon3.php").unwrap();
        assert_eq!((gb.language, gb.variant), ("en", Some("en-GB")));
        assert_eq!(gb.kind, Category::Names(Some("weapon")));
        let mx = layout("messages/es-MX/map2.php").unwrap();
        assert_eq!(
            (mx.language, mx.variant, mx.game),
            ("es", Some("es-MX"), Some("S2"))
        );
        assert_eq!(mx.kind, Category::Names(Some("stage")));
        assert_eq!(layout("messages/ja/app.php").unwrap().kind, Category::Site);
        assert_eq!(layout("messages/ja/tz.php").unwrap().kind, Category::Site);
        assert_eq!(
            layout("messages/ja/xmatch3.php").unwrap().kind,
            Category::Site
        );
        assert!(
            layout("messages/_deepl/zh/salmon-boss3.php")
                .unwrap()
                .machine
        );
        assert_eq!(
            layout("locales/ko/strings.json").unwrap().kind,
            Category::Names(None)
        );
        assert_eq!(layout("messages/ja/gear-shoes.php").unwrap().game, None);
        // Not message folders
        assert!(layout("src/models/Weapon.php").is_none());
        assert!(layout("messages/ja/weapon3.txt").is_none());
        assert!(layout("weapon3.php").is_none());
        assert!(layout("ja/.php").is_none());
    }

    #[test]
    fn categories_and_games() {
        assert_eq!(category("weapon-shooter"), Category::Names(Some("weapon")));
        assert_eq!(category("Gear2"), Category::Names(Some("gear")));
        assert_eq!(category("special"), Category::Names(Some("special")));
        assert_eq!(category("subweapon3"), Category::Names(Some("sub")));
        assert_eq!(category("weapon-short"), Category::Site);
        assert_eq!(category("something"), Category::Names(None));
        assert_eq!(game("salmon3"), Some("S3"));
        assert_eq!(game("salmon2"), Some("S2"));
        assert_eq!(game("salmon"), None);
        let all = ["map", "map2", "map3", "brand", "brand2", "death"];
        let exists = |c: &str| all.contains(&c);
        assert_eq!(first_game("map", exists), Some("S1"));
        assert_eq!(first_game("brand", exists), Some("S1"));
        assert_eq!(first_game("death", exists), None);
        assert_eq!(first_game("map3", exists), None);
    }
}
