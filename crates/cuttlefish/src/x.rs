//! Posts captured from X (Twitter) by `tools/capture/xcap.mjs`: the Salmon
//! Run posts of the accounts the user follows, each with its replies, one
//! JSON line per thread in `<knowledge>/inbox/x/<handle>/posts.jsonl`.
//! The inbox reads them as community documents, one per thread, with
//! source kind [`SourceKind::X`]: the post, its quoted post, and the
//! replies in order, every line with its author and time, like a Discord
//! conversation's. The capture's `state.json` beside the folders is
//! skipped.

use crate::doc::{Document, SourceKind, doc_id, guess_language};
use crate::game;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The `tool` the capture writes into its `state.json`
pub const TOOL: &str = "xcap";
/// The `source` every thread record carries
pub const SOURCE: &str = "x";
/// The file of an account's threads, in `<inbox>/x/<handle>/`
pub const POSTS_FILE: &str = "posts.jsonl";
/// Terms recorded with the documents
pub const LICENSE: &str = "Posts by their authors on X; captured from the user's own account: study use only, do not republish";

/// Who wrote a post
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

impl Author {
    /// `Name (@handle)`, or whichever is known
    fn label(&self) -> String {
        match (&self.name, &self.handle) {
            (Some(n), Some(h)) if n != h => alloc::format!("{n} (@{h})"),
            (_, Some(h)) => alloc::format!("@{h}"),
            (Some(n), None) => n.clone(),
            (None, None) => String::from("?"),
        }
    }
}

/// A photo or video of a post
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Media {
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub url: Option<String>,
    /// The video file, for a video
    #[serde(default)]
    pub video: Option<String>,
}

/// The post a post replies to
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplyTo {
    pub id: String,
    #[serde(default)]
    pub handle: Option<String>,
}

/// A post quoted by another
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quoted {
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub text: String,
}

/// One post: the thread's root or a reply
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    #[serde(default)]
    pub media: Vec<Media>,
    #[serde(default)]
    pub reply_to: Option<ReplyTo>,
}

/// One line of `posts.jsonl`: a post with its replies
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Thread {
    #[serde(flatten)]
    pub post: Post,
    #[serde(default)]
    pub quoted: Option<Quoted>,
    #[serde(default)]
    pub replies: Vec<Post>,
    /// The Salmon Run terms the capture matched
    #[serde(default)]
    pub matched: Vec<String>,
    #[serde(default)]
    pub captured_at: Option<DateTime<Utc>>,
}

/// Whether the first bytes of a JSON lines file are a capture's
pub fn is_posts_file(head: &str) -> bool {
    let compact = head.replace(char::is_whitespace, "");
    compact.contains(&alloc::format!("\"source\":\"{SOURCE}\"")) && compact.contains("\"replies\"")
}

/// Whether the first bytes of a JSON file are the capture's `state.json`
pub fn is_state_file(head: &str) -> bool {
    head.replace(char::is_whitespace, "")
        .contains(&alloc::format!("\"tool\":\"{TOOL}\""))
}

/// The threads of a `posts.jsonl`, in file order; a line that does not
/// parse is skipped with a warning. A thread appearing twice (a re-run
/// that read the same post) keeps its last record.
pub fn read(path: &Path) -> Result<Vec<Thread>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| alloc::format!("reading {}", path.display()))?;
    let mut threads: Vec<Thread> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Thread>(line) {
            Ok(t) => {
                if let Some(i) = threads.iter().position(|o| o.post.id == t.post.id) {
                    threads[i] = t;
                } else {
                    threads.push(t);
                }
            }
            Err(e) => log::warn!("{}:{}: not a thread record: {e}", path.display(), n + 1),
        }
    }
    Ok(threads)
}

/// The address of a post: the record's, else built from the handle and id
fn url_of(post: &Post) -> String {
    match (&post.url, &post.author.handle) {
        (Some(u), _) => u.clone(),
        (None, Some(h)) => alloc::format!("https://x.com/{h}/status/{}", post.id),
        (None, None) => alloc::format!("https://x.com/i/status/{}", post.id),
    }
}

fn stamp(date: Option<DateTime<Utc>>) -> String {
    match date {
        Some(d) => d.format("%Y-%m-%d %H:%M UTC").to_string(),
        None => String::from("date unknown"),
    }
}

/// One post as lines of the document: its time, author, whom it answers,
/// the text, then its links and media on lines of their own
fn write_post(text: &mut String, post: &Post, first: bool) {
    let reply = match &post.reply_to {
        Some(r) if !first => match &r.handle {
            Some(h) => alloc::format!(" \u{21aa} @{h}"),
            None => String::from(" \u{21aa} ?"),
        },
        _ => String::new(),
    };
    text.push_str(&alloc::format!(
        "[{}] {}{reply}: {}\n",
        stamp(post.date),
        post.author.label(),
        post.text.trim()
    ));
    for u in &post.urls {
        text.push_str(&alloc::format!("  [{u}]\n"));
    }
    for m in &post.media {
        if let Some(u) = m.video.as_ref().or(m.url.as_ref()) {
            text.push_str(&alloc::format!("  [{}: {u}]\n", m.r#type));
        }
    }
}

/// The title of a thread's document: the author, the day and the start of
/// the text
fn title_of(t: &Thread) -> String {
    let head: String = t.post.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut head: String = head.chars().take(60).collect();
    if head.chars().count() == 60 && t.post.text.chars().count() > 60 {
        head.push('\u{2026}');
    }
    let day = t.post.date.map_or_else(String::new, |d| {
        alloc::format!(", {}", d.format("%Y-%m-%d"))
    });
    alloc::format!("{} on X{day}: {head}", t.post.author.label())
}

/// One document per thread: the post, its quoted post and its replies as
/// timestamped lines, keyed by the post's address; the authors are the
/// attribution, the language the post's own (`lang`) when X knew it
pub fn to_documents(threads: &[Thread]) -> Vec<Document> {
    threads
        .iter()
        .map(|t| {
            let url = url_of(&t.post);
            let mut text = String::new();
            write_post(&mut text, &t.post, true);
            if let Some(q) = &t.quoted {
                text.push_str(&alloc::format!(
                    "  Quoting {} ({}): {}\n",
                    q.author.label(),
                    stamp(q.date),
                    q.text.trim()
                ));
                if let Some(u) = &q.url {
                    text.push_str(&alloc::format!("  [{u}]\n"));
                }
            }
            let mut authors: Vec<String> = alloc::vec![t.post.author.label()];
            for r in &t.replies {
                text.push('\n');
                write_post(&mut text, r, false);
                let who = r.author.label();
                if !authors.contains(&who) {
                    authors.push(who);
                }
            }
            let mut doc = Document::new(
                SourceKind::X,
                &url,
                title_of(t),
                String::from(text.trim_end()),
            );
            doc.url = Some(url);
            doc.language = t
                .post
                .lang
                .as_deref()
                .filter(|l| matches!(*l, "en" | "ja" | "zh" | "ko" | "es" | "fr" | "ru" | "de"))
                .map(String::from)
                .or_else(|| guess_language(&doc.text).map(String::from));
            doc.attribution = Some(alloc::format!("{} on X", authors.join(", ")));
            doc.license = Some(String::from(LICENSE));
            doc.game = t.post.date.map(game::era);
            doc
        })
        .collect()
}

/// The document id a thread's record gets
pub fn document_id(post: &Post) -> String {
    doc_id(&url_of(post))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Game;

    const LINE: &str = r#"{"source":"x","id":"1971000000000000010","url":"https://x.com/ikura_coach/status/1971000000000000010","author":{"handle":"ikura_coach","name":"Ikura Coach"},"date":"2026-09-24T12:00:00.000Z","text":"バクダンは湧いた瞬間に処理。続きは動画で https://youtu.be/abc123","lang":"ja","urls":["https://youtu.be/abc123"],"media":[{"type":"photo","url":"https://pbs.twimg.com/media/one.jpg"},{"type":"video","url":"https://pbs.twimg.com/thumb.jpg","video":"https://video.twimg.com/two.mp4"}],"quoted":{"id":"6","url":"https://x.com/salmon_lab/status/6","author":{"handle":"salmon_lab","name":"Salmon Lab"},"date":"2026-09-24T10:00:00.000Z","text":"干潮のハコビヤはカゴ前"},"reply_to":null,"replies":[{"id":"1971000000000000020","url":"https://x.com/wave3/status/1971000000000000020","author":{"handle":"wave3","name":"Wave 3"},"date":"2026-09-24T12:20:00.000Z","text":"@ikura_coach カタパッドが2体同時のときは？","lang":"ja","urls":[],"media":[],"reply_to":{"id":"1971000000000000010","handle":"ikura_coach"}},{"id":"1971000000000000021","url":"https://x.com/ikura_coach/status/1971000000000000021","author":{"handle":"ikura_coach","name":"Ikura Coach"},"date":"2026-09-24T12:30:00.000Z","text":"@wave3 ボムを2個","lang":"ja","urls":[],"media":[],"reply_to":{"id":"1971000000000000020","handle":"wave3"}}],"matched":["バクダン"],"captured_at":"2026-09-27T00:00:00.000Z"}"#;

    #[test]
    fn recognises_the_captures_files() {
        assert!(is_posts_file(LINE));
        assert!(is_posts_file(
            r#"{"source": "x", "id": "1", "replies": []}"#
        ));
        assert!(!is_posts_file(
            r#"{"id": "1", "channel_id": "2", "author": {}}"#
        ));
        assert!(is_state_file(r#"{"tool": "xcap", "version": 1}"#));
        assert!(!is_state_file(r#"{"tool": "cuttlefish fetch discord"}"#));
    }

    #[test]
    fn reads_threads_and_makes_one_document_each() {
        let dir = std::env::temp_dir().join(alloc::format!("cuttlefish-x-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("posts.jsonl");
        let second = LINE
            .replace("1971000000000000010", "1971000000000000012")
            .replace("\"lang\":\"ja\"", "\"lang\":\"en\"")
            .replace(
                "バクダンは湧いた瞬間に処理。続きは動画で",
                "Eggstra Work this weekend",
            );
        std::fs::write(
            &path,
            alloc::format!("{LINE}\nnot json\n\n{second}\n{LINE}\n"),
        )
        .unwrap();
        let threads = read(&path).unwrap();
        // The repeated record replaces the first, in its place
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].post.id, "1971000000000000010");
        assert_eq!(threads[0].replies.len(), 2);
        assert_eq!(threads[0].matched, ["バクダン"]);
        let docs = to_documents(&threads);
        let doc = &docs[0];
        assert_eq!(doc.source, SourceKind::X);
        assert_eq!(doc.weight, SourceKind::X.default_weight());
        assert_eq!(
            doc.url.as_deref(),
            Some("https://x.com/ikura_coach/status/1971000000000000010")
        );
        assert_eq!(doc.id, document_id(&threads[0].post));
        assert_eq!(
            doc.title,
            "Ikura Coach (@ikura_coach) on X, 2026-09-24: バクダンは湧いた瞬間に処理。続きは動画で https://youtu.be/abc123"
        );
        assert_eq!(doc.language.as_deref(), Some("ja"));
        assert_eq!(doc.game, Some(Game::S3));
        assert_eq!(
            doc.attribution.as_deref(),
            Some("Ikura Coach (@ikura_coach), Wave 3 (@wave3) on X")
        );
        assert_eq!(doc.license.as_deref(), Some(LICENSE));
        let expected = "[2026-09-24 12:00 UTC] Ikura Coach (@ikura_coach): バクダンは湧いた瞬間に処理。続きは動画で https://youtu.be/abc123\n  [https://youtu.be/abc123]\n  [photo: https://pbs.twimg.com/media/one.jpg]\n  [video: https://video.twimg.com/two.mp4]\n  Quoting Salmon Lab (@salmon_lab) (2026-09-24 10:00 UTC): 干潮のハコビヤはカゴ前\n  [https://x.com/salmon_lab/status/6]\n\n[2026-09-24 12:20 UTC] Wave 3 (@wave3) \u{21aa} @ikura_coach: @ikura_coach カタパッドが2体同時のときは？\n\n[2026-09-24 12:30 UTC] Ikura Coach (@ikura_coach) \u{21aa} @wave3: @wave3 ボムを2個";
        assert_eq!(doc.text, expected);
        // The second thread: X's language wins over the guess
        assert_eq!(docs[1].language.as_deref(), Some("en"));
        assert!(
            docs[1]
                .title
                .starts_with("Ikura Coach (@ikura_coach) on X, 2026-09-24: Eggstra Work")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bare_records_still_make_documents() {
        let t = Thread {
            post: Post {
                id: String::from("5"),
                text: String::from("Salmon Run tonight"),
                ..Post::default()
            },
            ..Thread::default()
        };
        let docs = to_documents(&[t]);
        assert_eq!(docs[0].url.as_deref(), Some("https://x.com/i/status/5"));
        assert_eq!(docs[0].title, "? on X: Salmon Run tonight");
        assert_eq!(docs[0].text, "[date unknown] ?: Salmon Run tonight");
        assert_eq!(docs[0].game, None);
        assert_eq!(docs[0].language, None);
    }
}
