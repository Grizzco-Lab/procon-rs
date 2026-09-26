//! Multilingual name tables in structured files: the same key with a value
//! in several languages (weapon, stage and boss names), as found in the
//! locale folders of source trees, datamined text dumps and spreadsheets
//! saved as CSV.
//!
//! A file is read into leaves, each a key path and a string ([`read`]):
//! JSON, YAML and TOML are flattened (array items are named by their `id`,
//! `key` or `name` when they have one); CSV and TSV rows become `[row key,
//! column]`; gettext `.po` and `.properties` files give `[key]`. A leaf's
//! language comes from the file's path when it names one
//! (`locales/ja/weapons.json`, `JPja.json`, `strings_ko.po`: files that
//! differ only by it form one table, a family written
//! `locales/*/weapons.json`, see [`path_language`]), else from a key segment
//! (`en`, `ja`, `name_zh`, `USen`, `Japanese`, ...). Leaves are grouped by
//! their key without the language; a key with names in two languages or
//! more becomes a glossary [`Term`] ([`build`]), with its file and key in
//! [`Term::from`].
//!
//! Only names are kept: one line of at most [`MAX_NAME`] characters without
//! markup or placeholders. A table of more than [`LARGE`] terms is most
//! likely a whole game's interface text; of those, only keys that name
//! things (a segment containing "name") are kept, so "OK" and "Back" never
//! reach the glossary. Tables live in `<data>/terms/<id>.json`, one file
//! per table ([`save`], [`load_all`]).

use crate::doc::doc_id;
use crate::glossary::Term;
use crate::store::write_atomic;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Longest name kept, in characters
pub const MAX_NAME: usize = 60;

/// Terms in a table above which only keys naming things are kept
pub const LARGE: usize = 500;

/// Largest structured file read
pub const MAX_BYTES: u64 = 32 << 20;

/// The normalized code of a language code or name: `en`, `ja`, `zh`
/// (simplified), `zh-Hant`, `ko`, `fr`, `de`, `es`, `it`, `nl`, `ru`, `pt`.
/// Regional variants fold into their language; Nintendo's region codes
/// (`USen`, `JPja`, `CNzh`, `TWzh`, ...) are understood.
pub fn language(name: &str) -> Option<&'static str> {
    let l = name.trim().to_lowercase().replace('_', "-");
    Some(match l.as_str() {
        "en" | "eng" | "english" | "en-us" | "en-gb" | "en-au" | "en-ca" | "usen" | "euen" => "en",
        "ja" | "jp" | "jpn" | "japanese" | "ja-jp" | "jpja" | "日本語" => "ja",
        "zh"
        | "zh-cn"
        | "zh-hans"
        | "zh-sg"
        | "chs"
        | "cnzh"
        | "chinese"
        | "chinese (simplified)"
        | "simplified chinese"
        | "\u{4e2d}\u{6587}"
        | "\u{7b80}\u{4f53}\u{4e2d}\u{6587}" => "zh",
        "zh-tw"
        | "zh-hk"
        | "zh-hant"
        | "cht"
        | "twzh"
        | "hkzh"
        | "chinese (traditional)"
        | "traditional chinese"
        | "\u{7e41}\u{9ad4}\u{4e2d}\u{6587}" => "zh-Hant",
        "ko" | "kr" | "kor" | "korean" | "ko-kr" | "krko" | "한국어" => "ko",
        "fr" | "fr-fr" | "fr-ca" | "eufr" | "usfr" | "french" | "français" => "fr",
        "de" | "de-de" | "eude" | "german" | "deutsch" => "de",
        "es" | "es-es" | "es-mx" | "eues" | "uses" | "spanish" | "español" => "es",
        "it" | "it-it" | "euit" | "italian" | "italiano" => "it",
        "nl" | "nl-nl" | "eunl" | "dutch" | "nederlands" => "nl",
        "ru" | "ru-ru" | "euru" | "russian" | "русский" => "ru",
        "pt" | "pt-br" | "pt-pt" | "portuguese" | "português" => "pt",
        _ => return None,
    })
}

/// Characters that separate words in keys and file names
fn is_separator(c: char) -> bool {
    matches!(c, '_' | '.' | ' ' | '-' | '(' | ')' | '[' | ']')
}

/// The language a key segment or file name names, and the segment with it
/// replaced by `*`: `ja` → `*`, `name_zh_TW` → `name_*`, `strings.ko` →
/// `strings.*`
pub fn segment_language(segment: &str) -> Option<(&'static str, String)> {
    if let Some(l) = language(segment) {
        return Some((l, String::from("*")));
    }
    // Words with their byte ranges
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, c) in segment.char_indices().chain([(segment.len(), ' ')]) {
        match (is_separator(c), start) {
            (true, Some(s)) => {
                words.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if words.len() < 2 {
        return None;
    }
    let masked = |s: usize, e: usize| alloc::format!("{}*{}", &segment[..s], &segment[e..]);
    // From the end, a pair (`zh_TW`) before a single word
    for i in (0..words.len()).rev() {
        if let Some(&(_, end)) = words.get(i + 1) {
            let (s, e) = words[i];
            let pair = alloc::format!("{}-{}", &segment[s..e], &segment[words[i + 1].0..end]);
            if let Some(l) = language(&pair) {
                return Some((l, masked(s, end)));
            }
        }
        let (s, e) = words[i];
        if let Some(l) = language(&segment[s..e]) {
            return Some((l, masked(s, e)));
        }
    }
    None
}

/// The language a file's path names (in its file name or a folder, nearest
/// first) and the path with it replaced by `*`, which names the family of
/// files that differ only by language
pub fn path_language(rel: &str) -> Option<(&'static str, String)> {
    let parts: Vec<&str> = rel.split('/').collect();
    let last = parts.len().checked_sub(1)?;
    for i in (0..parts.len()).rev() {
        let (name, ext) = match parts[i].rsplit_once('.') {
            Some((stem, ext)) if i == last && !stem.is_empty() => (stem, Some(ext)),
            _ => (parts[i], None),
        };
        if let Some((l, masked)) = segment_language(name) {
            let mut family: Vec<String> = parts.iter().map(|p| String::from(*p)).collect();
            family[i] = match ext {
                Some(ext) => alloc::format!("{masked}.{ext}"),
                None => masked,
            };
            return Some((l, family.join("/")));
        }
    }
    None
}

/// One string of a structured file
#[derive(Clone, Debug, PartialEq)]
pub struct Leaf {
    /// Key path inside the file
    pub path: Vec<String>,
    /// The string
    pub value: String,
    /// Its language when the format says so (a `.po` file's source text)
    pub language: Option<&'static str>,
}

impl Leaf {
    fn new(path: Vec<String>, value: &str) -> Self {
        Leaf {
            path,
            value: String::from(value),
            language: None,
        }
    }
}

/// Fields that name an array item, by preference
const ITEM_NAMES: [&str; 9] = [
    "id", "key", "code", "slug", "rowid", "__rowid", "name", "label", "title",
];

/// A name for an array item from one of its fields
fn item_name(item: &Value) -> Option<String> {
    let object = item.as_object()?;
    ITEM_NAMES.iter().find_map(|want| {
        object
            .iter()
            .find(|(k, _)| k.to_lowercase() == *want)
            .and_then(|(_, v)| match v {
                Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
    })
}

/// The strings of a tree by key path; with `all`, numbers and switches too
fn flatten(value: &Value, all: bool, path: &mut Vec<String>, out: &mut Vec<Leaf>) {
    match value {
        Value::String(s) => out.push(Leaf::new(path.clone(), s)),
        Value::Number(_) | Value::Bool(_) if all => {
            out.push(Leaf::new(path.clone(), &value.to_string()))
        }
        Value::Object(map) => {
            for (k, v) in map {
                path.push(k.clone());
                flatten(v, all, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                path.push(item_name(v).unwrap_or_else(|| i.to_string()));
                flatten(v, all, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Splits delimited text into rows, with quoted cells (`"a, ""b"""`)
fn split_rows(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            c if c == delimiter && !quoted => row.push(core::mem::take(&mut cell)),
            '\n' if !quoted => {
                row.push(core::mem::take(&mut cell));
                rows.push(core::mem::take(&mut row));
            }
            '\r' if !quoted => {}
            c => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|c| !c.trim().is_empty()));
    rows
}

/// Header names of a column holding row keys
const KEY_COLUMNS: [&str; 8] = ["id", "key", "code", "slug", "row", "rowid", "label", "name"];

/// Rows of a CSV or TSV table as `[row key, column]` leaves; the key is a
/// key-like column (`id`, `key`, ...) or else the first column that is not a
/// language, or the row number
fn table_leaves(text: &str, delimiter: Option<char>) -> Vec<Leaf> {
    let first = text.lines().next().unwrap_or_default();
    let delimiter = delimiter.unwrap_or_else(|| {
        [',', ';', '\t']
            .into_iter()
            .max_by_key(|d| first.matches(*d).count())
            .unwrap_or(',')
    });
    let rows = split_rows(text, delimiter);
    let Some((header, rows)) = rows.split_first() else {
        return Vec::new();
    };
    let header: Vec<&str> = header.iter().map(|h| h.trim()).collect();
    let is_language = |h: &str| segment_language(h).is_some();
    let key = KEY_COLUMNS
        .iter()
        .find_map(|k| {
            header
                .iter()
                .position(|h| h.to_lowercase() == *k && !is_language(h))
        })
        .or_else(|| header.iter().position(|h| !is_language(h)));
    let mut out = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let name = key
            .and_then(|k| row.get(k))
            .map(|k| k.trim())
            .filter(|k| !k.is_empty())
            .map_or_else(|| alloc::format!("row {}", r + 1), String::from);
        for (c, cell) in row.iter().enumerate() {
            if Some(c) != key && !cell.trim().is_empty() {
                let column = header.get(c).copied().unwrap_or_default();
                out.push(Leaf::new(
                    alloc::vec![name.clone(), String::from(column)],
                    cell.trim(),
                ));
            }
        }
    }
    out
}

/// A gettext string: `"a" "b"` continued over lines, unescaped
fn po_string(text: &str) -> String {
    let mut out = String::new();
    for part in text.split('"').skip(1).step_by(2) {
        out.push_str(&part.replace("\\n", "\n").replace("\\t", "\t"));
    }
    out
}

/// A gettext catalogue: each translation under its `msgid`, which is also
/// kept as the source text (English, as is usual); the catalogue's
/// `Language:` header, if any
fn po_leaves(text: &str) -> (Vec<Leaf>, Option<&'static str>) {
    let mut out = Vec::new();
    let mut declared = None;
    for block in text.split("\n\n") {
        let (mut id, mut value, mut field) = (String::new(), String::new(), "");
        for line in block.lines().map(str::trim) {
            if let Some(rest) = line.strip_prefix("msgid ") {
                field = "id";
                id = po_string(rest);
            } else if let Some(rest) = line.strip_prefix("msgstr ") {
                field = "str";
                value = po_string(rest);
            } else if line.starts_with('"') {
                match field {
                    "id" => id.push_str(&po_string(line)),
                    "str" => value.push_str(&po_string(line)),
                    _ => {}
                }
            } else if !line.starts_with('#') {
                field = "";
            }
        }
        if id.is_empty() {
            declared = value
                .lines()
                .find_map(|l| l.strip_prefix("Language:"))
                .and_then(language);
        } else if !value.is_empty() {
            out.push(Leaf::new(alloc::vec![id.clone()], &value));
            out.push(Leaf {
                language: Some("en"),
                ..Leaf::new(alloc::vec![id.clone()], &id)
            });
        }
    }
    (out, declared)
}

/// `key = value` lines of a `.properties` file
fn properties_leaves(text: &str) -> Vec<Leaf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && !l.starts_with('!'))
        .filter_map(|l| l.split_once(['=', ':']))
        .map(|(k, v)| Leaf::new(alloc::vec![String::from(k.trim())], v.trim()))
        .collect()
}

/// A file's extension, lowercase, and its text
fn read_text(path: &Path) -> Result<(String, String)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let bytes =
        std::fs::read(path).with_context(|| alloc::format!("reading {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    Ok((ext, String::from(text.trim_start_matches('\u{feff}'))))
}

/// The tree of a JSON, YAML or TOML text; `None` for other formats
fn parse_tree(ext: &str, text: &str) -> Result<Option<Value>> {
    Ok(Some(match ext {
        "json" => serde_json::from_str(text).context("not JSON")?,
        "yaml" | "yml" => serde_yaml_ng::from_str(text).context("not YAML")?,
        "toml" => serde_json::to_value(toml::from_str::<toml::Value>(text).context("not TOML")?)?,
        _ => return Ok(None),
    }))
}

/// Largest data table (a structured file without names in several
/// languages) kept as a text document
pub const MAX_TEXT: u64 = 1 << 20;

/// A data table as text for a document: one `key / path: value` line per
/// value of a JSON, YAML or TOML file (numbers and switches included), and
/// other formats (CSV, ...) as they are
pub fn as_text(path: &Path) -> Result<String> {
    let (ext, text) = read_text(path)?;
    let Some(tree) = parse_tree(&ext, &text)? else {
        return Ok(text);
    };
    let mut leaves = Vec::new();
    flatten(&tree, true, &mut Vec::new(), &mut leaves);
    Ok(leaves
        .iter()
        .map(|l| alloc::format!("{}: {}", l.path.join(" / "), l.value))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// The strings of a structured file (by extension: `json`, `yaml`, `yml`,
/// `toml`, `csv`, `tsv`, `po`, `properties`) and the language the file
/// declares, if any
pub fn read(path: &Path) -> Result<(Vec<Leaf>, Option<&'static str>)> {
    let (ext, text) = read_text(path)?;
    let text = text.as_str();
    if let Some(tree) = parse_tree(&ext, text)? {
        let mut out = Vec::new();
        flatten(&tree, false, &mut Vec::new(), &mut out);
        return Ok((out, None));
    }
    Ok(match ext.as_str() {
        "csv" => (table_leaves(text, None), None),
        "tsv" => (table_leaves(text, Some('\t')), None),
        "po" => po_leaves(text),
        "properties" => (properties_leaves(text), None),
        _ => bail!("not a structured file: .{ext}"),
    })
}

/// Whether a string can be a name: one line of at most [`MAX_NAME`]
/// characters with a letter, no markup, placeholders or control codes, and
/// at least 3 characters when plain ASCII (2 otherwise)
pub fn is_name(value: &str) -> bool {
    let v = value.trim();
    let n = v.chars().count();
    let min = if v.is_ascii() { 3 } else { 2 };
    (min..=MAX_NAME).contains(&n)
        && v.chars().any(char::is_alphabetic)
        && !v.chars().any(|c| {
            c.is_control() || matches!(c, '{' | '}' | '%' | '<' | '>' | '[' | ']' | '|' | '\\')
        })
        && !v.starts_with("http")
}

/// Lowercase ASCII words joined by `-`
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    String::from(out.trim_end_matches('-'))
}

/// A structured file's strings, with the language its path names
#[derive(Clone, Debug)]
pub struct Member {
    /// Path of the file
    pub file: String,
    /// Language named by the path ([`path_language`]) or declared inside
    pub language: Option<&'static str>,
    /// Its strings
    pub leaves: Vec<Leaf>,
}

/// A name table imported into the glossary
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    /// Id ([`doc_id`] of `source`), the file name in `terms/`
    pub id: String,
    /// The file, or the family of files (`locales/*/weapons.json`)
    pub source: String,
    /// The files read
    pub files: Vec<String>,
    /// Languages of the kept terms
    pub languages: Vec<String>,
    /// Strings read
    pub strings: usize,
    /// What was kept and why, in words
    pub note: String,
    /// The terms
    pub terms: Vec<Term>,
}

/// Groups the strings of a file or family (`source`) into terms; a table
/// without terms has nothing in several languages
pub fn build(source: &str, members: &[Member]) -> Table {
    // Key without language → language → names
    let mut groups: BTreeMap<String, BTreeMap<&'static str, Vec<String>>> = BTreeMap::new();
    let (mut strings, mut single, mut long) = (0, 0, 0);
    for m in members {
        for leaf in &m.leaves {
            strings += 1;
            let mut path = leaf.path.clone();
            let lang = leaf.language.or(m.language).or_else(|| {
                (0..path.len()).rev().find_map(|i| {
                    let (l, masked) = segment_language(&path[i])?;
                    let rest = masked
                        .replace('*', "")
                        .trim_matches(is_separator)
                        .to_string();
                    if rest.is_empty() {
                        path.remove(i);
                    } else {
                        path[i] = rest;
                    }
                    Some(l)
                })
            });
            let Some(lang) = lang else {
                single += 1;
                continue;
            };
            if !is_name(&leaf.value) {
                long += 1;
                continue;
            }
            let names = groups
                .entry(path.join("/"))
                .or_default()
                .entry(lang)
                .or_default();
            let value = String::from(leaf.value.trim());
            if !names.contains(&value) {
                names.push(value);
            }
        }
    }
    groups.retain(|_, langs| langs.len() >= 2);
    let found = groups.len();
    let mut note = alloc::format!("{found} names in several languages");
    if found > LARGE {
        groups.retain(|key, _| key.to_lowercase().contains("name"));
        note = alloc::format!(
            "{} of {found} kept: a table this large is mostly interface text, so only keys naming things (\"name\") are taken",
            groups.len()
        );
    }
    if long > 0 {
        note.push_str(&alloc::format!(
            "; {long} strings too long or with markup to be names"
        ));
    }
    if single > 0 {
        note.push_str(&alloc::format!("; {single} strings without a language"));
    }
    let mut terms: Vec<Term> = Vec::new();
    let mut languages = BTreeSet::new();
    for (key, langs) in groups {
        let last = key.rsplit('/').next().unwrap_or_default();
        let id = langs
            .get("en")
            .map(|n| slug(&n[0]))
            .filter(|s| !s.is_empty())
            .or_else(|| Some(slug(last)).filter(|s| !s.is_empty()))
            .unwrap_or_else(|| doc_id(&key));
        let from = alloc::format!("{source}#{key}");
        let term = match terms.iter_mut().find(|t| t.id == id) {
            Some(t) => t,
            None => {
                terms.push(Term {
                    id,
                    definition: String::new(),
                    forms: BTreeMap::new(),
                    from: Vec::new(),
                });
                terms.last_mut().unwrap()
            }
        };
        term.from.push(from);
        for (lang, names) in langs {
            languages.insert(String::from(lang));
            let forms = term.forms.entry(String::from(lang)).or_default();
            for n in names {
                if !forms.contains(&n) {
                    forms.push(n);
                }
            }
        }
    }
    Table {
        id: doc_id(source),
        source: String::from(source),
        files: members.iter().map(|m| m.file.clone()).collect(),
        languages: languages.into_iter().collect(),
        strings,
        note,
        terms,
    }
}

/// Folder of the tables in a data folder
fn dir(root: &Path) -> std::path::PathBuf {
    root.join("terms")
}

/// Writes a table into the data folder
pub fn save(root: &Path, table: &Table) -> Result<()> {
    write_atomic(
        &dir(root).join(alloc::format!("{}.json", table.id)),
        &serde_json::to_vec_pretty(table)?,
    )
}

/// Removes a table, if there is one
pub fn remove(root: &Path, id: &str) -> Result<()> {
    let path = dir(root).join(alloc::format!("{id}.json"));
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Every table in the data folder, by id; a file that does not read
/// (half-synced, a conflict copy) is skipped with a warning
pub fn load_all(root: &Path) -> Vec<Table> {
    let Ok(entries) = std::fs::read_dir(dir(root)) else {
        return Vec::new();
    };
    let mut tables: Vec<Table> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        if path.extension().is_none_or(|x| x != "json") || !crate::store::is_id(&stem) {
            continue;
        }
        let read = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Table>(&b)?));
        match read {
            Ok(t) => tables.push(t),
            Err(e) => log::warn!("skipping {}: {e:#}", path.display()),
        }
    }
    tables.sort_by(|a, b| a.id.cmp(&b.id));
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(file: &str, text: &str) -> Member {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-tables-{}", std::process::id()));
        let path = dir.join(file.replace('/', "_"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, text).unwrap();
        let (leaves, declared) = read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        Member {
            file: String::from(file),
            language: path_language(file).map(|(l, _)| l).or(declared),
            leaves,
        }
    }

    fn names<'a>(t: &'a Table, id: &str) -> &'a BTreeMap<String, Vec<String>> {
        &t.terms
            .iter()
            .find(|t| t.id == id)
            .unwrap_or_else(|| panic!("no {id} in {t:?}"))
            .forms
    }

    #[test]
    fn languages_from_codes_names_and_paths() {
        assert_eq!(language("USen"), Some("en"));
        assert_eq!(language("zh_TW"), Some("zh-Hant"));
        assert_eq!(language("Japanese"), Some("ja"));
        assert_eq!(language("\u{7b80}\u{4f53}\u{4e2d}\u{6587}"), Some("zh"));
        assert_eq!(language("weapon"), None);
        assert_eq!(
            segment_language("name_zh_TW"),
            Some(("zh-Hant", String::from("name_*")))
        );
        assert_eq!(segment_language("Shooter_Short_00"), None);
        assert_eq!(
            path_language("repo/locales/ja/weapons.json"),
            Some(("ja", String::from("repo/locales/*/weapons.json")))
        );
        assert_eq!(
            path_language("splat3/data/language/CNzh.json"),
            Some(("zh", String::from("splat3/data/language/*.json")))
        );
        assert_eq!(
            path_language("po/strings_ko.po"),
            Some(("ko", String::from("po/strings_*.po")))
        );
        assert_eq!(path_language("guides/weapons.json"), None);
    }

    #[test]
    fn locale_files_form_one_table() {
        let en = member(
            "app/locales/en/weapons.json",
            r#"{"weapons": {"splattershot": "Splattershot", "hydra": "Hydra Splatling", "desc": "Fires {0} shots"}}"#,
        );
        let ja = member(
            "app/locales/ja/weapons.json",
            r#"{"weapons": {"splattershot": "スプラシューター", "hydra": "ハイドラント"}}"#,
        );
        let zh = member(
            "app/locales/zh-CN/weapons.json",
            "{\"weapons\": {\"splattershot\": \"\u{5c04}\u{51fb}\u{67aa}\"}}",
        );
        let table = build("app/locales/*/weapons.json", &[en, ja, zh]);
        assert_eq!(table.terms.len(), 2, "{table:?}");
        let shot = names(&table, "splattershot");
        assert_eq!(shot["ja"], ["スプラシューター"]);
        assert_eq!(shot["zh"], ["\u{5c04}\u{51fb}\u{67aa}"]);
        assert_eq!(table.languages, ["en", "ja", "zh"]);
        let term = table
            .terms
            .iter()
            .find(|t| t.id == "hydra-splatling")
            .unwrap();
        assert_eq!(term.from, ["app/locales/*/weapons.json#weapons/hydra"]);
        assert_eq!(table.strings, 6);
    }

    #[test]
    fn nintendo_message_dumps() {
        let us = member(
            "splat3/language/USen.json",
            r#"{"CommonMsg/Weapon/WeaponName_Main": {"Shooter_Normal_00": "Splattershot"},
                "CommonMsg/Coop/CoopEnemy": {"SakelienBomber": "Steelhead"}}"#,
        );
        let jp = member(
            "splat3/language/JPja.json",
            r#"{"CommonMsg/Weapon/WeaponName_Main": {"Shooter_Normal_00": "スプラシューター"},
                "CommonMsg/Coop/CoopEnemy": {"SakelienBomber": "バクダン"}}"#,
        );
        let table = build("splat3/language/*.json", &[us, jp]);
        assert_eq!(names(&table, "steelhead")["ja"], ["バクダン"]);
        assert_eq!(
            table.terms[0].from[0],
            "splat3/language/*.json#CommonMsg/Coop/CoopEnemy/SakelienBomber"
        );
    }

    #[test]
    fn languages_inside_one_file() {
        let json = member(
            "data/bosses.json",
            r#"[{"id": "steelhead", "name": {"en": "Steelhead", "ja": "バクダン", "fr": "Tête-de-pneu"}, "hp": 1000},
                {"id": "maws", "name_en": "Maws", "name_ja": "モグラ"},
                {"id": "lonely", "name": {"en": "Only English"}}]"#,
        );
        let table = build("data/bosses.json", &[json]);
        assert_eq!(table.terms.len(), 2, "{table:?}");
        assert_eq!(names(&table, "steelhead")["fr"], ["Tête-de-pneu"]);
        assert_eq!(names(&table, "maws")["ja"], ["モグラ"]);
        let yaml = member(
            "data/stages.yaml",
            "spawning_grounds:\n  en: Spawning Grounds\n  ja: シェケナダム\n",
        );
        let table = build("data/stages.yaml", &[yaml]);
        assert_eq!(names(&table, "spawning-grounds")["ja"], ["シェケナダム"]);
    }

    #[test]
    fn spreadsheets_and_catalogues() {
        let csv = member(
            "names.csv",
            "key,English,Japanese,Chinese (Simplified)\nSakelienBomber,Steelhead,バクダン,\"\u{94c1}\u{76d4}\"\nSakelienCupTwins,\"Flyfish, the\",カタパッド,\n",
        );
        let table = build("names.csv", &[csv]);
        assert_eq!(table.terms.len(), 2);
        assert_eq!(names(&table, "steelhead")["zh"], ["\u{94c1}\u{76d4}"]);
        assert_eq!(names(&table, "flyfish-the")["en"], ["Flyfish, the"]);
        let po = member(
            "po/ja.po",
            "msgid \"\"\nmsgstr \"Language: ja\\n\"\n\nmsgid \"Golden Egg\"\nmsgstr \"金イクラ\"\n",
        );
        let table = build("po/*.po", &[po]);
        assert_eq!(names(&table, "golden-egg")["ja"], ["金イクラ"]);
    }

    #[test]
    fn no_table_without_languages() {
        let package = member(
            "app/package.json",
            r#"{"name": "splat-tool", "scripts": {"build": "vite build"}, "dependencies": {"vue": "^3"}}"#,
        );
        let table = build("app/package.json", &[package]);
        assert!(table.terms.is_empty());
        assert!(table.note.contains("without a language"), "{}", table.note);
    }

    #[test]
    fn large_tables_keep_names_only() {
        let mut en = serde_json::Map::new();
        let mut ja = serde_json::Map::new();
        for i in 0..LARGE + 10 {
            en.insert(
                alloc::format!("Menu/Button{i}"),
                Value::from(alloc::format!("Back {i}")),
            );
            ja.insert(
                alloc::format!("Menu/Button{i}"),
                Value::from(alloc::format!("戻る {i}")),
            );
        }
        en.insert("Weapon/WeaponName".into(), Value::from("Splattershot"));
        ja.insert("Weapon/WeaponName".into(), Value::from("スプラシューター"));
        let m = |file: &str, map: serde_json::Map<String, Value>| {
            member(file, &Value::Object(map).to_string())
        };
        let table = build("ui/*.json", &[m("ui/en.json", en), m("ui/ja.json", ja)]);
        assert_eq!(table.terms.len(), 1);
        assert!(table.note.starts_with("1 of 511 kept"), "{}", table.note);
    }

    #[test]
    fn data_tables_as_text() {
        let path = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-data-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"[{"__RowId": "Shooter_Normal_00", "Range": 1.5, "Auto": true, "Special": "Trizooka"}]"#,
        )
        .unwrap();
        let text = as_text(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            text,
            "Shooter_Normal_00 / Auto: true\nShooter_Normal_00 / Range: 1.5\nShooter_Normal_00 / Special: Trizooka\nShooter_Normal_00 / __RowId: Shooter_Normal_00"
        );
    }

    #[test]
    fn names_only() {
        assert!(is_name("Splattershot Jr."));
        assert!(is_name("バクダン"));
        assert!(!is_name("OK"));
        assert!(!is_name("Deals {0} damage"));
        assert!(!is_name("[color=red]Hot[/color]"));
        assert!(!is_name("line one\nline two"));
        assert!(!is_name("123"));
        assert_eq!(slug("Splattershot Jr."), "splattershot-jr");
    }
}
