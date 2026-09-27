//! PHP array literals, as Yii message files return them:
//!
//! ```php
//! <?php
//! /**
//!  * @copyright Copyright (C) 2022-2025 AIZAWA Hina
//!  * @license https://github.com/fetus-hina/stat.ink/blob/master/LICENSE MIT
//!  */
//! return [
//!     'Big Shot' => '铁球鱼',
//!     'Slammin\' Lid' => "锅盖鱼", // a comment
//! ];
//! ```
//!
//! Nothing is executed: [`parse_array`] reads the string literals of the
//! returned array (single or double quotes, escapes, `.` concatenation,
//! nested arrays, comments, trailing commas) and skips anything else
//! (numbers, constants, calls). [`header`] reads the `@license` and
//! `@copyright` tags of the file's doc comment.

use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Result, bail, ensure};

/// The `@license` and `@copyright` of a file's leading doc comment
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    /// `@license <url> <name>` as `<name> (<url>)`, or the tag's text
    pub license: Option<String>,
    /// The `@copyright` text
    pub copyright: Option<String>,
}

/// Reads the doc comment tags at the top of a PHP file
pub fn header(text: &str) -> Header {
    let mut out = Header::default();
    let Some(start) = text.find("/*") else {
        return out;
    };
    let end = text[start..].find("*/").map_or(text.len(), |e| start + e);
    for line in text[start..end].lines() {
        let line = line.trim().trim_start_matches(['/', '*']).trim();
        if let Some(rest) = line.strip_prefix("@license") {
            let words: Vec<&str> = rest.split_whitespace().collect();
            out.license = match words.as_slice() {
                [] => None,
                [url, name @ ..] if url.starts_with("http") && !name.is_empty() => {
                    Some(alloc::format!("{} ({url})", name.join(" ")))
                }
                _ => Some(words.join(" ")),
            };
        } else if let Some(rest) = line.strip_prefix("@copyright") {
            out.copyright = Some(String::from(rest.trim())).filter(|c| !c.is_empty());
        }
    }
    out
}

/// A cursor over the text
struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn eat(&mut self, prefix: &str) -> bool {
        if self.rest().starts_with(prefix) {
            self.at += prefix.len();
            true
        } else {
            false
        }
    }

    /// Skips whitespace and comments (`//`, `#`, `/* */`)
    fn skip(&mut self) {
        loop {
            let rest = self.rest();
            let trimmed = rest.trim_start();
            self.at += rest.len() - trimmed.len();
            if self.eat("//") || self.eat("#") {
                let line = self.rest().find('\n').unwrap_or(self.rest().len());
                self.at += line;
            } else if self.eat("/*") {
                let close = self.rest().find("*/").map_or(self.rest().len(), |c| c + 2);
                self.at += close;
            } else {
                return;
            }
        }
    }

    /// A quoted string, unescaped, at the cursor
    fn string(&mut self) -> Result<Option<String>> {
        let Some(quote) = self.peek().filter(|c| matches!(c, '\'' | '"')) else {
            return Ok(None);
        };
        self.at += 1;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        loop {
            let Some((i, c)) = chars.next() else {
                bail!("unterminated string at byte {}", self.at);
            };
            match c {
                c if c == quote => {
                    self.at += i + 1;
                    return Ok(Some(out));
                }
                '\\' => {
                    let Some((_, e)) = chars.next() else {
                        bail!("unterminated string at byte {}", self.at);
                    };
                    match (quote, e) {
                        (_, '\\') => out.push('\\'),
                        (q, e) if e == q => out.push(q),
                        ('"', 'n') => out.push('\n'),
                        ('"', 't') => out.push('\t'),
                        ('"', 'r') => out.push('\r'),
                        ('"', '$') => out.push('$'),
                        ('"', 'u') => {
                            // "\u{1F980}"
                            let rest = &self.rest()[i + 2..];
                            let hex = rest
                                .strip_prefix('{')
                                .and_then(|r| r.split_once('}'))
                                .map(|(h, _)| h);
                            match hex.and_then(|h| u32::from_str_radix(h, 16).ok()) {
                                Some(code) => {
                                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                                    for _ in 0..hex.map_or(0, |h| h.len() + 2) {
                                        chars.next();
                                    }
                                }
                                None => out.push_str("\\u"),
                            }
                        }
                        (_, e) => {
                            // PHP keeps an unknown escape as it is
                            out.push('\\');
                            out.push(e);
                        }
                    }
                }
                c => out.push(c),
            }
        }
    }

    /// A string, possibly concatenated with `.`
    fn strings(&mut self) -> Result<Option<String>> {
        let Some(mut s) = self.string()? else {
            return Ok(None);
        };
        loop {
            self.skip();
            let save = self.at;
            if !self.eat(".") {
                return Ok(Some(s));
            }
            self.skip();
            match self.string()? {
                Some(more) => s.push_str(&more),
                None => {
                    self.at = save;
                    return Ok(Some(s));
                }
            }
        }
    }

    /// The opening of an array (`[` or `array(`) at the cursor; returns its
    /// closing token
    fn open(&mut self) -> Option<&'static str> {
        if self.eat("[") {
            Some("]")
        } else if self.eat("array") {
            self.skip();
            self.eat("(").then_some(")")
        } else {
            None
        }
    }

    /// Skips a value that is not a string or an array: up to the next `,`
    /// or closing token at this depth
    fn skip_value(&mut self) {
        let mut depth = 0i32;
        while let Some(c) = self.peek() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth > 0 => depth -= 1,
                ',' | ')' | ']' if depth == 0 => return,
                '\'' | '"' => {
                    let _ = self.string();
                    continue;
                }
                _ => {}
            }
            self.at += c.len_utf8();
        }
    }

    /// The `key => value` pairs of the array whose opening was just read,
    /// nested keys joined with `/`
    fn array(&mut self, close: &str, prefix: &str, out: &mut Vec<(String, String)>) -> Result<()> {
        loop {
            self.skip();
            if self.eat(close) {
                return Ok(());
            }
            ensure!(self.at < self.text.len(), "unterminated array");
            let start = self.at;
            let key = match self.strings()? {
                Some(k) => k,
                None => {
                    // A number or a constant as the key
                    while self
                        .peek()
                        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | ':' | '-'))
                    {
                        self.at += 1;
                    }
                    String::from(self.text[start..self.at].trim())
                }
            };
            self.skip();
            if self.eat("=>") {
                self.skip();
                self.pair(Some(key), prefix, out)?;
            } else {
                // A list item without a key: read again as a value
                self.at = start;
                self.pair(None, prefix, out)?;
            }
            self.skip();
            if !self.eat(",") {
                self.skip();
                ensure!(
                    self.eat(close),
                    "expected `,` or `{close}` at byte {}",
                    self.at
                );
                return Ok(());
            }
        }
    }

    /// The value of one pair, read at the cursor; nested arrays go into
    /// `out` with their keys under `key`
    fn pair(
        &mut self,
        key: Option<String>,
        prefix: &str,
        out: &mut Vec<(String, String)>,
    ) -> Result<()> {
        let full = match &key {
            Some(k) if prefix.is_empty() => k.clone(),
            Some(k) => alloc::format!("{prefix}/{k}"),
            None => String::from(prefix),
        };
        if let Some(value) = self.strings()? {
            if key.is_some() {
                out.push((full, value));
            }
        } else if let Some(close) = self.open() {
            self.array(close, &full, out)?;
        } else {
            self.skip_value();
        }
        Ok(())
    }
}

/// The `'key' => 'value'` pairs of the array a PHP file returns, in file
/// order; keys of nested arrays are joined with `/`. Pairs whose value is
/// not a string are left out. Fails when the file returns no array or the
/// array does not parse.
pub fn parse_array(text: &str) -> Result<Vec<(String, String)>> {
    let mut p = Parser { text, at: 0 };
    let mut out = Vec::new();
    // The `return` statement, outside strings and comments
    loop {
        p.skip();
        if p.at >= text.len() {
            bail!("no `return [...]` found");
        }
        if p.rest().starts_with("return")
            && p.rest()[6..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        {
            p.at += 6;
            break;
        }
        if p.string()?.is_none() {
            p.at += p.peek().map_or(1, char::len_utf8);
        }
    }
    p.skip();
    if let Some(close) = p.open() {
        p.array(close, "", &mut out)?;
        return Ok(out);
    }
    // `return array_merge([...], require(...))`: the array literals among
    // the call's arguments, the rest skipped
    let start = p.at;
    while p
        .peek()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '\\')
    {
        p.at += 1;
    }
    p.skip();
    ensure!(
        p.at > start && p.eat("("),
        "`return` is not followed by an array"
    );
    loop {
        p.skip();
        if p.eat(")") {
            break;
        }
        ensure!(p.at < text.len(), "unterminated call after `return`");
        match p.open() {
            Some(close) => p.array(close, "", &mut out)?,
            None => p.skip_value(),
        }
        p.skip();
        if !p.eat(",") {
            ensure!(p.eat(")"), "expected `,` or `)` at byte {}", p.at);
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_yii_message_file() {
        let text = r#"<?php

/**
 * @copyright Copyright (C) 2022-2025 AIZAWA Hina
 * @license https://github.com/fetus-hina/stat.ink/blob/master/LICENSE MIT
 * @author AIZAWA Hina <hina@fetus.jp>
 */

declare(strict_types=1);

return [
    'Big Shot' => '铁球鱼',
    // v2.0.0
    'Slammin\' Lid' => '锅盖鱼', # trailing comment
    "Marooner's Bay" => "漂浮\"落难\"船\n",
    'Any Weapon' => '',
    'Steel' . ' Eel' => 'ヘビ',
    'Count' => 3,
    'Constant' => SOME_CONSTANT,
    'Call' => Yii::t('app', 'x'),
    'Nested' => [
        'Fog' => '霧',
        'Rush' => "ラッシュ",
    ],
    'Old' => array('Tide' => '潮'),
    'Last' => 'value',
];
"#;
        let pairs = parse_array(text).unwrap();
        let get = |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("Big Shot"), Some("铁球鱼"));
        assert_eq!(get("Slammin' Lid"), Some("锅盖鱼"));
        assert_eq!(get("Marooner's Bay"), Some("漂浮\"落难\"船\n"));
        assert_eq!(get("Any Weapon"), Some(""));
        assert_eq!(get("Steel Eel"), Some("ヘビ"));
        assert_eq!(get("Count"), None);
        assert_eq!(get("Constant"), None);
        assert_eq!(get("Call"), None);
        assert_eq!(get("Nested/Fog"), Some("霧"));
        assert_eq!(get("Nested/Rush"), Some("ラッシュ"));
        assert_eq!(get("Old/Tide"), Some("潮"));
        assert_eq!(get("Last"), Some("value"));
        assert_eq!(pairs.len(), 9);
        let h = header(text);
        assert_eq!(
            h.license.as_deref(),
            Some("MIT (https://github.com/fetus-hina/stat.ink/blob/master/LICENSE)")
        );
        assert_eq!(
            h.copyright.as_deref(),
            Some("Copyright (C) 2022-2025 AIZAWA Hina")
        );
    }

    #[test]
    fn escapes_and_edge_cases() {
        let pairs =
            parse_array(r#"return ['a\\b' => 'c\nd', "e\\f" => "g\th\u{1F980}", 'x' => "\$var"];"#)
                .unwrap();
        assert_eq!(pairs[0], (String::from("a\\b"), String::from("c\\nd")));
        assert_eq!(
            pairs[1],
            (String::from("e\\f"), String::from("g\th\u{1F980}"))
        );
        assert_eq!(pairs[2].1, "$var");
        // Trailing comma optional, `array()` form, empty array
        assert_eq!(parse_array("return array('k' => 'v');").unwrap().len(), 1);
        // Arrays merged with files that are not read here
        let merged = parse_array(
            "return array_merge(\n    ['Headgear' => 'アタマ', 'Shoes' => 'クツ'],\n    require(__DIR__ . '/gear-headgear.php'),\n    ['Clothing' => 'フク']\n);",
        )
        .unwrap();
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[2].0, "Clothing");
        assert!(parse_array("<?php return [];").unwrap().is_empty());
        // `return` inside a string or comment does not count
        let pairs =
            parse_array("<?php\n// return [bad]\n$x = 'return [';\nreturn ['k' => 'v'];").unwrap();
        assert_eq!(pairs.len(), 1);
        assert!(parse_array("<?php echo 'hi';").is_err());
        assert!(parse_array("return ['k' => 'v'").is_err());
        assert!(parse_array("return ['k' => 'v").is_err());
        assert!(parse_array("return 42;").is_err());
        assert_eq!(header("<?php return [];"), Header::default());
    }
}
