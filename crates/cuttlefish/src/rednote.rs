//! Notes captured from Xiaohongshu (RedNote, 小红书) by
//! `tools/capture/rednote.mjs`: the Salmon Run notes (笔记) of the creators
//! the user follows, each with its comments and their replies, one JSON
//! line per note in `<knowledge>/inbox/rednote/<user id>/notes.jsonl`.
//! The inbox reads them as community documents, one per note, with source
//! kind [`SourceKind::Rednote`]: a header with the creator and the date,
//! the title, the text, the tags, then the comments as timestamped lines
//! with their replies indented, like a Discord conversation's. The
//! capture's `state.json` beside the folders is skipped.

use crate::doc::{Document, SourceKind, doc_id, guess_language};
use crate::game;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The `tool` the capture writes into its `state.json`
pub const TOOL: &str = "rncap";
/// The `source` every note record carries
pub const SOURCE: &str = "rednote";
/// The file of a creator's notes, in `<inbox>/rednote/<user id>/`
pub const NOTES_FILE: &str = "notes.jsonl";
/// The site
pub const SITE: &str = "https://www.xiaohongshu.com";
/// Terms recorded with the documents
pub const LICENSE: &str = "Xiaohongshu notes and comments by their authors; captured from the user's own account: study use only, do not republish";

/// Who wrote a note or a comment
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub nickname: String,
}

impl Author {
    /// The nickname, else the id
    fn label(&self) -> &str {
        if self.nickname.is_empty() {
            &self.user_id
        } else {
            &self.nickname
        }
    }
}

/// A comment, with the replies under it
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub likes: u64,
    /// The region the site shows with it
    #[serde(default)]
    pub location: Option<String>,
    /// The comment a reply answers, when not the root comment
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Who wrote that comment
    #[serde(default)]
    pub reply_to_author: Option<String>,
    /// Replies the site lists under this comment
    #[serde(default)]
    pub replies: Vec<Comment>,
    /// Replies the site counts (more than listed when not all were loaded)
    #[serde(default)]
    pub replies_total: u64,
}

/// One line of `notes.jsonl`: a note with its comments
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub author: Author,
    /// When it was posted
    #[serde(default)]
    pub date: Option<DateTime<Utc>>,
    /// When it was last edited, if ever
    #[serde(default)]
    pub updated: Option<DateTime<Utc>>,
    /// `normal` (images) or `video`
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    /// The body (the site's `desc`)
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Image addresses, not downloaded
    #[serde(default)]
    pub images: Vec<String>,
    /// The video's address, not downloaded
    #[serde(default)]
    pub video: Option<String>,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub collects: u64,
    #[serde(default)]
    pub shares: u64,
    /// Comments the site counts
    #[serde(default)]
    pub comment_count: u64,
    #[serde(default)]
    pub comments: Vec<Comment>,
    /// Every comment was loaded (else a cap cut them)
    #[serde(default)]
    pub comments_complete: bool,
    /// The Salmon Run terms the capture matched
    #[serde(default)]
    pub matched: Vec<String>,
    #[serde(default)]
    pub captured_at: Option<DateTime<Utc>>,
}

impl Note {
    /// The note's page: the record's, else built from the id
    pub fn url(&self) -> String {
        match &self.url {
            Some(u) => u.clone(),
            None => alloc::format!("{SITE}/explore/{}", self.id),
        }
    }

    /// Comments and replies together
    pub fn comment_total(&self) -> usize {
        self.comments.iter().map(|c| 1 + c.replies.len()).sum()
    }
}

/// Whether the first bytes of a JSON lines file are a capture's
pub fn is_notes_file(head: &str) -> bool {
    let compact = head.replace(char::is_whitespace, "");
    compact.contains(&alloc::format!("\"source\":\"{SOURCE}\"")) && compact.contains("\"comments\"")
}

/// Whether the first bytes of a JSON file are the capture's `state.json`
pub fn is_state_file(head: &str) -> bool {
    head.replace(char::is_whitespace, "")
        .contains(&alloc::format!("\"tool\":\"{TOOL}\""))
}

/// The notes of a `notes.jsonl`, in file order; a line that does not
/// parse is skipped with a warning. A note appearing twice (a later run
/// that read it again) keeps its last record.
pub fn read(path: &Path) -> Result<Vec<Note>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| alloc::format!("reading {}", path.display()))?;
    let mut notes: Vec<Note> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Note>(line) {
            Ok(note) if note.id.is_empty() => {
                log::warn!("{}:{}: a note without an id", path.display(), n + 1)
            }
            Ok(note) => {
                if let Some(i) = notes.iter().position(|o| o.id == note.id) {
                    notes[i] = note;
                } else {
                    notes.push(note);
                }
            }
            Err(e) => log::warn!("{}:{}: not a note record: {e}", path.display(), n + 1),
        }
    }
    Ok(notes)
}

fn stamp(date: Option<DateTime<Utc>>) -> String {
    match date {
        Some(d) => alloc::format!("[{}] ", d.format("%Y-%m-%d %H:%M UTC")),
        None => String::new(),
    }
}

/// One comment as a line: its time, author, whom it answers, the text
fn write_comment(text: &mut String, c: &Comment, answers: Option<&str>, indent: &str) {
    let reply = answers.map_or_else(String::new, |who| alloc::format!(" \u{21aa} {who}"));
    text.push_str(&alloc::format!(
        "{indent}{}{}{reply}: {}\n",
        stamp(c.date),
        c.author.label(),
        c.text.trim()
    ));
}

/// The title of a note's document: the note's title, else the start of
/// its text, else its id
fn title_of(n: &Note) -> String {
    if !n.title.trim().is_empty() {
        return String::from(n.title.trim());
    }
    let head: String = n.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut head: String = head.chars().take(60).collect();
    if head.is_empty() {
        return alloc::format!("Note {}", n.id);
    }
    if head.chars().count() == 60 && n.text.chars().count() > 60 {
        head.push('\u{2026}');
    }
    head
}

/// One document per note: a header with the creator and the day, the
/// title as a heading, the text, the tags, and the comments under a
/// heading of their own as `[date] author: text` lines with the replies
/// indented (`↳ name` for the comment they answer). The creator is the
/// attribution, the language the note's own, the era from its date.
pub fn to_documents(notes: &[Note]) -> Vec<Document> {
    notes
        .iter()
        .map(|n| {
            let url = n.url();
            let who = n.author.label();
            let day = n
                .date
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| String::from("date unknown"));
            let title = title_of(n);
            let mut text = alloc::format!(
                "Xiaohongshu note by {who}, {day}{}\n# {title}\n\n{}\n",
                if n.kind == "video" { " (video)" } else { "" },
                n.text.trim()
            );
            if !n.tags.is_empty() {
                let tags: Vec<String> = n.tags.iter().map(|t| alloc::format!("#{t}")).collect();
                text.push_str(&alloc::format!("\nTags: {}\n", tags.join(" ")));
            }
            if let Some(v) = &n.video {
                text.push_str(&alloc::format!("\n[video: {v}]\n"));
            }
            if !n.comments.is_empty() {
                text.push_str(&alloc::format!(
                    "\n## Comments ({} of {})\n\n",
                    n.comment_total(),
                    n.comment_count.max(n.comment_total() as u64)
                ));
                for c in &n.comments {
                    write_comment(&mut text, c, None, "");
                    for r in &c.replies {
                        let answers = r
                            .reply_to_author
                            .as_deref()
                            .filter(|a| !a.is_empty())
                            .unwrap_or(c.author.label());
                        write_comment(&mut text, r, Some(answers), "  ");
                    }
                }
            }
            let mut doc = Document::new(
                SourceKind::Rednote,
                &url,
                title,
                String::from(text.trim_end()),
            );
            doc.url = Some(url);
            // The note's own language, not the English header's
            doc.language =
                guess_language(&alloc::format!("{}\n{}", n.title, n.text)).map(String::from);
            doc.attribution = Some(alloc::format!("{who} on Xiaohongshu"));
            doc.license = Some(String::from(LICENSE));
            doc.game = n.date.map(game::era);
            if let Some(at) = n.captured_at {
                doc.fetched_at = at;
            }
            doc
        })
        .collect()
}

/// The document id a note's record gets
pub fn document_id(note: &Note) -> String {
    doc_id(&note.url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Game;

    const LINE: &str = r#"{"source":"rednote","id":"66aa00000000000000000001","url":"https://www.xiaohongshu.com/explore/66aa00000000000000000001","author":{"user_id":"5f0000000000000000000001","nickname":"Grizzco Coach"},"date":"2024-08-30T06:40:00.000Z","updated":"2024-08-30T07:40:00.000Z","kind":"video","title":"打工400分教学","text":"第一波决定一切：先处理炸弹鱼，再搬蛋。 #打工[话题]#","tags":["打工","Splatoon3"],"images":["https://sns-img/1.jpg"],"video":"https://sns-video/1.mp4","likes":12000,"collects":3210,"shares":12,"comment_count":88,"comments":[{"id":"c1","author":{"user_id":"u2","nickname":"alice"},"date":"2024-08-30T09:26:40.000Z","text":"Kill the Steelhead before the Flyfish","likes":5,"location":"北京","replies":[{"id":"c1-1","author":{"user_id":"u3","nickname":"bob"},"date":"2024-08-30T12:13:20.000Z","text":"Only when it is at the shore","likes":1,"reply_to":"c1","reply_to_author":"alice","replies":[],"replies_total":0}],"replies_total":2},{"id":"c2","author":{"user_id":"u4","nickname":"carol"},"date":null,"text":"nice","likes":0,"replies":[],"replies_total":0}],"comments_complete":false,"matched":["打工"],"captured_at":"2026-09-27T10:00:00.000Z"}"#;

    #[test]
    fn recognises_the_captures_files() {
        assert!(is_notes_file(LINE));
        assert!(is_notes_file(
            r#"{"source": "rednote", "id": "1", "comments": []}"#
        ));
        assert!(!is_notes_file(r#"{"source":"x","id":"1","replies":[]}"#));
        assert!(!is_notes_file(
            r#"{"id": "1", "channel_id": "2", "author": {}}"#
        ));
        assert!(is_state_file(r#"{"tool": "rncap", "version": 1}"#));
        assert!(!is_state_file(r#"{"tool": "xcap", "version": 1}"#));
    }

    #[test]
    fn reads_notes_and_makes_one_document_each() {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-rednote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(NOTES_FILE);
        let second = LINE
            .replace("66aa00000000000000000001", "66aa00000000000000000002")
            .replace("\"title\":\"打工400分教学\"", "\"title\":\"\"")
            .replace("\"kind\":\"video\"", "\"kind\":\"normal\"")
            .replace(",\"video\":\"https://sns-video/1.mp4\"", "");
        let edited = LINE.replace("\"likes\":12000", "\"likes\":12500");
        std::fs::write(
            &path,
            alloc::format!("{second}\nnot json\n\n{LINE}\n{edited}\n{{\"source\":\"rednote\"}}\n"),
        )
        .unwrap();
        let notes = read(&path).unwrap();
        // The repeated record replaces the first, in its place
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[1].id, "66aa00000000000000000001");
        assert_eq!(notes[1].likes, 12500);
        assert_eq!(notes[1].comment_total(), 3);
        assert_eq!(notes[1].matched, ["打工"]);
        let docs = to_documents(&notes);
        let doc = &docs[1];
        assert_eq!(doc.source, SourceKind::Rednote);
        assert_eq!(doc.weight, SourceKind::Rednote.default_weight());
        assert_eq!(
            doc.url.as_deref(),
            Some("https://www.xiaohongshu.com/explore/66aa00000000000000000001")
        );
        assert_eq!(doc.id, document_id(&notes[1]));
        assert_eq!(doc.title, "打工400分教学");
        assert_eq!(doc.language.as_deref(), Some("zh"));
        assert_eq!(doc.game, Some(Game::S3));
        assert_eq!(
            doc.attribution.as_deref(),
            Some("Grizzco Coach on Xiaohongshu")
        );
        assert_eq!(doc.license.as_deref(), Some(LICENSE));
        assert_eq!(doc.fetched_at.to_rfc3339(), "2026-09-27T10:00:00+00:00");
        assert!(
            doc.text.starts_with(
                "Xiaohongshu note by Grizzco Coach, 2024-08-30 (video)\n# 打工400分教学\n\n第一波决定一切"
            ),
            "{}",
            doc.text
        );
        assert!(doc.text.contains("\nTags: #打工 #Splatoon3\n"));
        assert!(doc.text.contains("\n[video: https://sns-video/1.mp4]\n"));
        assert!(doc.text.contains("\n## Comments (3 of 88)\n\n"));
        assert!(
            doc.text
                .contains("[2024-08-30 09:26 UTC] alice: Kill the Steelhead before the Flyfish\n")
        );
        assert!(doc.text.contains(
            "\n  [2024-08-30 12:13 UTC] bob \u{21aa} alice: Only when it is at the shore\n"
        ));
        // A comment without a date has no stamp
        assert!(doc.text.ends_with("\ncarol: nice"), "{}", doc.text);
        // Without a title, the start of the text names the document
        assert_eq!(
            docs[0].title,
            "第一波决定一切：先处理炸弹鱼，再搬蛋。 #打工[话题]#"
        );
        assert!(!docs[0].text.contains("(video)"));
        assert!(!docs[0].text.contains("[video:"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bare_note_still_reads() {
        let note: Note = serde_json::from_str(r#"{"source":"rednote","id":"1"}"#).unwrap();
        assert_eq!(note.url(), "https://www.xiaohongshu.com/explore/1");
        let doc = &to_documents(&[note])[0];
        assert_eq!(doc.title, "Note 1");
        assert_eq!(doc.attribution.as_deref(), Some(" on Xiaohongshu"));
        assert!(
            doc.text
                .starts_with("Xiaohongshu note by , date unknown\n# Note 1")
        );
        assert_eq!(doc.game, None);
    }
}
