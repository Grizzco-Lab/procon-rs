//! Documents: one page, video transcript, conversation or file, with where
//! it came from and under which terms it may be used.

use alloc::string::String;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Kind of source a document came from; sets its default retrieval weight
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    /// A web page
    Web,
    /// A wiki article (Inkipedia, the Overfishing wiki, ...)
    Wiki,
    /// A curated guide such as "Overfishing Fundamentals"
    Guide,
    /// A YouTube or Twitch transcript
    Video,
    /// The Overfishing Discord's #vod-review channel
    DiscordVodReview,
    /// Any other Discord channel
    Discord,
    /// A local file
    File,
}

impl SourceKind {
    /// Default retrieval weight ([`crate::index::score`]); high-end VOD
    /// review is the most trusted, generic web pages and auto-captions the
    /// least
    pub fn default_weight(self) -> f32 {
        match self {
            SourceKind::DiscordVodReview => 1.2,
            SourceKind::Guide => 1.15,
            SourceKind::Wiki | SourceKind::Discord | SourceKind::File => 1.0,
            SourceKind::Web | SourceKind::Video => 0.9,
        }
    }
}

/// One ingested document
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Stable id from [`doc_id`] of the url (or path); ingesting the same
    /// url again replaces the document
    pub id: String,
    /// Kind of source
    pub source: SourceKind,
    /// Where it can be read
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Title shown with citations
    pub title: String,
    /// Language code (`en`, `ja`, `zh`, `es`, `ru`, `fr`, ...), when known
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// License or terms of use (for example `CC BY-NC-SA 3.0`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Authors or channel to credit
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// When it was fetched or imported
    pub fetched_at: DateTime<Utc>,
    /// Retrieval weight, [`SourceKind::default_weight`] unless overridden
    pub weight: f32,
    /// Plain text; markdown headings (`#`) mark sections
    pub text: String,
}

impl Document {
    /// A document fetched now, with the source's default weight and the
    /// language guessed from the text
    pub fn new(source: SourceKind, key: &str, title: String, text: String) -> Self {
        Document {
            id: doc_id(key),
            source,
            url: None,
            title,
            language: guess_language(&text).map(String::from),
            license: None,
            attribution: None,
            fetched_at: Utc::now(),
            weight: source.default_weight(),
            text,
        }
    }
}

/// Stable id of a url or path (64-bit FNV-1a, hex)
pub fn doc_id(key: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    alloc::format!("{h:016x}")
}

/// Guesses the language from the script: kana is Japanese, Hangul Korean,
/// Han without kana Chinese, Cyrillic Russian. Latin text is not guessed
/// (English, Spanish and French look alike); pass it explicitly.
pub fn guess_language(text: &str) -> Option<&'static str> {
    let (mut kana, mut han, mut hangul, mut cyrillic, mut latin) = (0, 0, 0, 0, 0);
    for c in text.chars().take(5000) {
        match c as u32 {
            0x3040..=0x30ff => kana += 1,
            0x4e00..=0x9fff => han += 1,
            0xac00..=0xd7af => hangul += 1,
            0x0400..=0x04ff => cyrillic += 1,
            _ if c.is_ascii_alphabetic() => latin += 1,
            _ => {}
        }
    }
    let cjk = kana + han + hangul;
    if kana > 0 && kana * 10 >= cjk && cjk * 4 >= latin {
        Some("ja")
    } else if hangul * 2 > cjk && cjk * 4 >= latin && hangul > 0 {
        Some("ko")
    } else if han > 0 && cjk * 4 >= latin {
        Some("zh")
    } else if cyrillic > latin {
        Some("ru")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable() {
        assert_eq!(doc_id("a"), doc_id("a"));
        assert_ne!(doc_id("a"), doc_id("b"));
        assert_eq!(doc_id("").len(), 16);
    }

    #[test]
    fn guesses_scripts() {
        assert_eq!(guess_language("バクダンを優先して倒す"), Some("ja"));
        // Han characters only
        assert_eq!(
            guess_language("\u{4f18}\u{5148}\u{5904}\u{7406}"),
            Some("zh")
        );
        assert_eq!(guess_language("Сначала убейте"), Some("ru"));
        assert_eq!(guess_language("Kill the Steelhead first"), None);
    }
}
