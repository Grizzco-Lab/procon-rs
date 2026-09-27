//! The corpus's VODs as reviews of the studio's Cuttlefish app: a folder
//! `<reviews>/discord-<conversation id>/review.json` per VOD whose video
//! is on disk ([`write`]).
//!
//! The review's video is the file in the knowledge folder by its full
//! path (kind `file`), not a copy: the VODs add up to gigabytes, and the
//! knowledge and reviews folders are synced folders where a hard link may
//! not be possible (FUSE mounts have none), so a reference is what keeps
//! working. Each message becomes comments at the moments it places in the
//! video (one per aligned moment, with the text from that moment to the
//! next) and one note for the rest: the text without a time, and the
//! wave-timer moments waiting for the video's wave table, listed as
//! [`Unplaced`] (wave and seconds left) so a HUD pass can place them
//! (`cuttlefish corpus align`); the next run then turns them into
//! comments. Everything written carries an
//! [`Origin`] (`from: discord` with the message's link) and an id starting
//! with [`ID_PREFIX`]; a run replaces those and nothing else, so comments
//! and notes people add in the studio stay, and a run that changes nothing
//! writes nothing.

use crate::corpus::{Corpus, CorpusMoment, ReviewMessage, VideoKind, Vod};
use crate::corpus_videos::{self, Info};
use crate::moments::MomentKind;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use core::fmt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Ids of the reviews, comments and notes written here start with this
pub const ID_PREFIX: &str = "discord-";
/// The review file in a review folder
pub const REVIEW_FILE: &str = "review.json";
/// The `from` of what comes from the archive
pub const FROM_DISCORD: &str = "discord";

/// Where an imported review, comment or note comes from
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    /// `discord`
    pub from: String,
    /// The link to the message (a comment or note) or to the conversation
    /// (a review)
    pub url: String,
    /// The video's link as posted (a review)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
    /// The Eggstra Work event the VOD was probably played in (a review;
    /// [`crate::corpus::Vod::eggstra_event`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eggstra_event: Option<u8>,
}

impl Origin {
    /// From a Discord message or conversation
    pub fn discord(url: &str) -> Self {
        Origin {
            from: String::from(FROM_DISCORD),
            url: String::from(url),
            video: None,
            eggstra_event: None,
        }
    }
}

/// A moment of a note that is not placed in the video yet
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Unplaced {
    /// As written (`W2 :50`)
    pub raw: String,
    pub kind: MomentKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<u8>,
    /// Seconds left on the wave timer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_s: Option<f32>,
}

/// The review id of a VOD
pub fn review_id(vod: &Vod) -> String {
    alloc::format!("{ID_PREFIX}{}", vod.id)
}

/// Whether an id (a review's, a comment's, a note's) was written here
pub fn is_ours(id: &str) -> bool {
    id.starts_with(ID_PREFIX)
}

/// A piece of a message: from a moment that starts a comment or a note
/// to the next
struct Segment<'a> {
    text: String,
    moment: Option<&'a CorpusMoment>,
}

/// Whether a moment starts a piece of its own: placed in the video, or
/// waiting to be (a timer, a wave without a time); a time of another
/// video is text
fn splits(m: &CorpusMoment) -> bool {
    m.aligned || m.needs_hud || m.kind == MomentKind::Unknown
}

/// Text before the first moment this short, on the same line, leads into
/// it (`at 1:20 ...`) rather than standing on its own
const LEAD_IN_CHARS: usize = 40;

/// A message's text cut at its moments: the text before the first as a
/// piece without one (a short lead-in on the moment's line joins it),
/// then one per moment. A piece that is only its moment (`W1` on a line
/// of its own) joins the next.
fn segments(m: &ReviewMessage) -> Vec<Segment<'_>> {
    let text = m.text.as_str();
    let mut starts: Vec<(usize, &CorpusMoment)> = Vec::new();
    let mut cursor = 0;
    for moment in m.moments.iter().filter(|x| splits(x)) {
        let at = text[cursor..]
            .find(&moment.raw)
            .map_or(cursor, |p| cursor + p);
        starts.push((at, moment));
        cursor = (at + moment.raw.len()).min(text.len());
    }
    let mut out: Vec<Segment> = Vec::new();
    let first = starts.first().map_or(text.len(), |(p, _)| *p);
    let lead = text[..first].trim();
    let mut carry = String::new();
    if !lead.is_empty() {
        if !starts.is_empty() && lead.len() <= LEAD_IN_CHARS && !text[..first].contains('\n') {
            carry.push_str(lead);
            carry.push(' ');
        } else {
            out.push(Segment {
                text: String::from(lead),
                moment: None,
            });
        }
    }
    for (i, (at, moment)) in starts.iter().enumerate() {
        let end = starts.get(i + 1).map_or(text.len(), |(p, _)| *p);
        let piece = text[*at..end].trim();
        let alone = piece
            .strip_prefix(moment.raw.as_str())
            .is_some_and(|rest| rest.trim_matches(|c: char| !c.is_alphanumeric()).is_empty());
        if alone && i + 1 < starts.len() {
            carry.push_str(piece);
            carry.push(' ');
            continue;
        }
        out.push(Segment {
            text: alloc::format!("{carry}{piece}"),
            moment: Some(moment),
        });
        carry.clear();
    }
    out
}

/// The text without links to the VOD's own video (the time of a link is
/// in the comment's time), lines of only spaces dropped
fn without_own_links(text: &str, vod: &Vod) -> String {
    let own = |token: &str| {
        (token.starts_with("https://") || token.starts_with("http://"))
            && crate::corpus::Video::from_url(token, None, &|_| None, Path::new("")).key
                == vod.video.key
    };
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            l.split_whitespace()
                .filter(|t| !own(t))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let mut out = String::new();
    let mut blank = true;
    for l in lines {
        if l.trim().is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(l.trim());
        blank = false;
    }
    String::from(out.trim())
}

/// The comments and the note a message gives
fn comments_and_note(m: &ReviewMessage, vod: &Vod) -> (Vec<Value>, Option<Value>) {
    let created_ms = m.time.timestamp_millis().max(0) as u64;
    let source = Origin::discord(&m.url);
    let mut comments = Vec::new();
    let mut note_text: Vec<String> = Vec::new();
    let mut unplaced: Vec<Unplaced> = Vec::new();
    for seg in segments(m) {
        let text = without_own_links(&seg.text, vod);
        match seg.moment {
            Some(moment) if moment.aligned => {
                comments.push(json!({
                    "id": alloc::format!("{ID_PREFIX}{}-{}", m.id, comments.len()),
                    "t_s": moment.t_s.unwrap_or_default(),
                    "author": m.author,
                    "text": text,
                    "shapes": [],
                    "created_ms": created_ms,
                    "source": source,
                }));
            }
            Some(moment) => {
                unplaced.push(Unplaced {
                    raw: moment.raw.clone(),
                    kind: moment.kind,
                    wave: moment.wave,
                    timer_s: (moment.kind == MomentKind::WaveTimer)
                        .then_some(moment.seconds)
                        .flatten(),
                });
                if !text.is_empty() {
                    note_text.push(text);
                }
            }
            None => {
                if !text.is_empty() {
                    note_text.push(text);
                }
            }
        }
    }
    let note = (!note_text.is_empty() || !unplaced.is_empty()).then(|| {
        let mut note = json!({
            "id": alloc::format!("{ID_PREFIX}{}-note", m.id),
            "author": m.author,
            "text": note_text.join("\n"),
            "created_ms": created_ms,
            "source": source,
        });
        if !unplaced.is_empty() {
            note["unplaced"] = json!(unplaced);
        }
        note
    });
    (comments, note)
}

/// The title a review of a VOD gets: the poster and the day
pub fn title(vod: &Vod) -> String {
    alloc::format!("{}, {}", vod.poster, vod.date)
}

/// What the corpus says a VOD's review holds: the video, title, era and
/// origin, and the comments and notes of its messages
pub fn review_value(vod: &Vod, knowledge: &Path) -> Result<Value> {
    let path = vod
        .local_video(knowledge)
        .context("the video is not on disk")?;
    let path = path.canonicalize().unwrap_or(path);
    let mut video = json!({ "kind": "file", "ref": path });
    match vod.video.kind {
        VideoKind::Youtube => {
            if let Some(info) = vod
                .video
                .id
                .as_deref()
                .and_then(|id| Info::load(&corpus_videos::dir(knowledge), id))
            {
                for (key, value) in [
                    ("title", info.title),
                    ("channel", info.channel),
                    ("upload_date", info.upload_date),
                ] {
                    if let Some(v) = value {
                        video[key] = json!(v);
                    }
                }
            }
        }
        VideoKind::Attachment => {
            if let Some(name) = &vod.video.name {
                video["title"] = json!(name);
            }
            video["upload_date"] = json!(vod.date.to_string());
        }
        VideoKind::Other => {}
    }
    let mut comments = Vec::new();
    let mut notes = Vec::new();
    for m in &vod.messages {
        let (c, n) = comments_and_note(m, vod);
        comments.extend(c);
        notes.extend(n);
    }
    let mut source = Origin::discord(&vod.url);
    source.video = Some(vod.video.url.clone());
    source.eggstra_event = vod.eggstra_event;
    Ok(json!({
        "video": video,
        "title": title(vod),
        "game": vod.game,
        "source": source,
        "comments": comments,
        "notes": notes,
    }))
}

/// Our part of a review replaced: the video (unless a person put another
/// kind of video there), title, era and origin, and the comments and
/// notes with our ids, each where it was, new ones at the end, gone ones
/// dropped; everything else stays where it is
fn merge(mut existing: Value, ours: &Value) -> Value {
    if !existing.is_object() {
        existing = json!({});
    }
    let keep_video = existing["video"].is_object() && existing["video"]["kind"] != "file";
    if !keep_video {
        existing["video"] = ours["video"].clone();
    }
    for key in ["title", "game", "source"] {
        existing[key] = ours[key].clone();
    }
    for key in ["comments", "notes"] {
        let id_of = |v: &Value| v["id"].as_str().map(String::from);
        let new: Vec<&Value> = ours[key].as_array().into_iter().flatten().collect();
        let mut used: Vec<String> = Vec::new();
        let mut kept: Vec<Value> = Vec::new();
        for c in existing[key].as_array().into_iter().flatten() {
            match id_of(c) {
                Some(id) if is_ours(&id) => {
                    if let Some(o) = new.iter().find(|o| id_of(o).as_deref() == Some(&id)) {
                        kept.push((*o).clone());
                        used.push(id);
                    }
                }
                _ => kept.push(c.clone()),
            }
        }
        for o in new {
            if !id_of(o).is_some_and(|id| used.contains(&id)) {
                kept.push(o.clone());
            }
        }
        existing[key] = Value::Array(kept);
    }
    existing
}

/// What [`write`] did
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// VODs without their video on disk, which get no review
    pub without_video: usize,
    /// Comments, notes and unplaced moments in the reviews written or kept
    pub comments: usize,
    pub notes: usize,
    pub unplaced: usize,
}

impl fmt::Display for Written {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} reviews created, {} updated, {} unchanged; {} VODs without their video on disk get none; {} comments, {} notes with {} moments waiting for the HUD",
            self.created,
            self.updated,
            self.unchanged,
            self.without_video,
            self.comments,
            self.notes,
            self.unplaced
        )
    }
}

/// The review folder of a VOD
pub fn review_dir(reviews: &Path, vod: &Vod) -> PathBuf {
    reviews.join(review_id(vod))
}

/// Writes or updates the review of every VOD whose video is on disk
pub fn write(corpus: &Corpus, knowledge: &Path, reviews: &Path) -> Result<Written> {
    let mut out = Written::default();
    for vod in &corpus.vods {
        if vod.local_video(knowledge).is_none() {
            out.without_video += 1;
            continue;
        }
        let ours = review_value(vod, knowledge)?;
        let path = review_dir(reviews, vod).join(REVIEW_FILE);
        let existing: Option<Value> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let merged = merge(existing.clone().unwrap_or(Value::Null), &ours);
        out.comments += merged["comments"].as_array().map_or(0, Vec::len);
        out.notes += merged["notes"].as_array().map_or(0, Vec::len);
        out.unplaced += merged["notes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| n["unplaced"].as_array().map_or(0, Vec::len))
            .sum::<usize>();
        match existing {
            Some(old) if old == merged => out.unchanged += 1,
            Some(_) => {
                crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&merged)?)?;
                out.updated += 1;
            }
            None => {
                crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&merged)?)?;
                out.created += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{self, tests::wave_table};

    #[test]
    fn reviews_of_vods_on_disk() {
        let root = corpus::tests::fixture("reviews");
        let reviews = root.join("Reviews");
        let c = corpus::build(&root).unwrap();
        let done = write(&c, &root, &reviews).unwrap();
        assert_eq!(
            done,
            Written {
                created: 1,
                without_video: 2,
                comments: 1,
                notes: 2,
                unplaced: 1,
                ..Written::default()
            }
        );
        let path = reviews.join("discord-300").join(REVIEW_FILE);
        let review: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(review["video"]["kind"], "file");
        assert_eq!(
            review["video"]["ref"],
            json!(
                root.join("media/discord/1/2/300/wipe.mp4")
                    .canonicalize()
                    .unwrap()
            )
        );
        assert_eq!(review["video"]["title"], "wipe.mp4");
        assert_eq!(review["title"], "Cy, 2023-06-01");
        assert_eq!(review["game"], "S3");
        assert_eq!(review["source"]["from"], "discord");
        assert_eq!(
            review["source"]["url"],
            "https://discord.com/channels/1/2/300"
        );
        assert_eq!(
            review["source"]["video"],
            "https://cdn.discordapp.com/attachments/2/77/wipe.mp4?ex=1"
        );
        let comment = &review["comments"][0];
        assert_eq!(comment["id"], "discord-301-0");
        assert_eq!(comment["t_s"], 12.0);
        assert_eq!(comment["author"], "Dee");
        assert_eq!(comment["text"], "0:12 nobody had the Flyfish");
        assert_eq!(comment["created_ms"], 1_685_614_200_000u64);
        assert_eq!(
            comment["source"]["url"],
            "https://discord.com/channels/1/2/301"
        );
        let note = &review["notes"][0];
        assert_eq!(note["id"], "discord-300-note");
        assert_eq!(note["text"], "wipe on W3");
        assert_eq!(
            note["unplaced"],
            json!([{"raw": "W3", "kind": "unknown", "wave": 3}])
        );
        // A pointer to another video is a note, its link kept
        let other = &review["notes"][1];
        assert_eq!(other["id"], "discord-302-note");
        assert_eq!(
            other["text"],
            "see https://youtu.be/abcdefghijk?t=83 for a cleaner one"
        );
        assert!(other.get("unplaced").is_none());

        // Again: nothing changes, nothing is written
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let again = write(&c, &root, &reviews).unwrap();
        assert_eq!(again.unchanged, 1);
        assert_eq!(again.created + again.updated, 0);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );

        // A person's comment and note, and a chat, stay through a re-run
        let mut edited = review.clone();
        edited["comments"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "cabc", "t_s": 3.0, "author": "user", "text": "mine", "shapes": [], "created_ms": 1}));
        edited["notes"].as_array_mut().unwrap().insert(
            0,
            json!({"id": "nabc", "author": "user", "text": "my note", "created_ms": 1}),
        );
        edited["messages"] = json!([{"id": "m1", "role": "user", "text": "hi", "created_ms": 1}]);
        std::fs::write(&path, serde_json::to_vec_pretty(&edited).unwrap()).unwrap();
        let third = write(&c, &root, &reviews).unwrap();
        assert_eq!(third.unchanged, 1);
        let review: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let ids: Vec<&str> = review["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["discord-301-0", "cabc"], "each where it was");
        assert_eq!(review["notes"].as_array().unwrap().len(), 3);
        assert_eq!(review["messages"][0]["text"], "hi");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hud_tables_turn_notes_into_comments() {
        let root = corpus::tests::fixture("reviews-hud");
        let reviews = root.join("Reviews");
        // souper's YouTube video downloaded, and its wave starts known
        let yt = corpus_videos::dir(&root);
        std::fs::create_dir_all(&yt).unwrap();
        std::fs::write(yt.join("abcdefghijk.mp4"), b"video").unwrap();
        Info {
            id: String::from("abcdefghijk"),
            title: Some(String::from("Run 3")),
            channel: Some(String::from("souper")),
            upload_date: Some(String::from("2023-04-30")),
            ..Info::default()
        }
        .save(&yt)
        .unwrap();
        let c = corpus::build(&root).unwrap();
        let done = write(&c, &root, &reviews).unwrap();
        assert_eq!(done.created, 2);
        let path = reviews.join("discord-200").join(REVIEW_FILE);
        let review: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(review["video"]["title"], "Run 3");
        assert_eq!(review["video"]["channel"], "souper");
        assert_eq!(review["title"], "souper, 2023-05-01");
        // The post's own link is not in its note; Ben's timers wait
        assert_eq!(review["notes"][0]["text"], "my run\nfeedback welcome");
        assert_eq!(review["comments"].as_array().unwrap().len(), 1);
        let waiting = &review["notes"][1];
        assert_eq!(waiting["id"], "discord-202-note");
        assert_eq!(
            waiting["text"],
            "W2 was rough\n:50 you were alone on the far side\n:30 good save"
        );
        assert_eq!(waiting["unplaced"].as_array().unwrap().len(), 3);
        assert_eq!(
            waiting["unplaced"][1],
            json!({"raw": ":50", "kind": "wave_timer", "wave": 2, "timer_s": 50.0})
        );
        assert_eq!(
            review["notes"].as_array().unwrap().len(),
            3,
            "souper's thanks too"
        );

        wave_table(
            &yt.join("abcdefghijk.mp4"),
            None,
            &[(2, 130.0, 230.0, 100.0)],
        );
        let c = corpus::build(&root).unwrap();
        let done = write(&c, &root, &reviews).unwrap();
        assert_eq!((done.updated, done.unchanged), (1, 1));
        let review: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let comments = review["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 4);
        // The wave named on its own is a comment where the wave starts
        assert_eq!(comments[1]["id"], "discord-202-0");
        assert_eq!(comments[1]["t_s"], 130.0);
        assert_eq!(comments[1]["text"], "W2 was rough");
        assert_eq!(comments[2]["t_s"], 180.0);
        assert_eq!(comments[2]["text"], ":50 you were alone on the far side");
        assert_eq!(comments[3]["t_s"], 200.0);
        assert_eq!(comments[3]["text"], ":30 good save");
        assert!(
            review["notes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|n| n["id"] != "discord-202-note")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn segments_cut_at_moments() {
        let root = corpus::tests::fixture("segments");
        let c = corpus::build(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let ben = &c.vods[1].messages[2];
        let s = segments(ben);
        let texts: Vec<&str> = s.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "W2 was rough",
                ":50 you were alone on the far side",
                ":30 good save"
            ]
        );
        assert!(s.iter().all(|x| x.moment.is_some()));
        // Text before the first moment is a piece of its own
        let dee = &c.vods[2].messages[1];
        let s = segments(dee);
        assert_eq!(s.len(), 1);
        assert!(s[0].moment.is_some_and(|m| m.aligned));
        let m = ReviewMessage {
            text: String::from("hi all\nW1\n:50 late"),
            moments: alloc::vec![
                CorpusMoment {
                    raw: String::from("W1"),
                    kind: MomentKind::Unknown,
                    seconds: None,
                    wave: Some(1),
                    t_s: None,
                    aligned: false,
                    needs_hud: false,
                    placed_by: None,
                    confidence: None
                },
                CorpusMoment {
                    raw: String::from(":50"),
                    kind: MomentKind::WaveTimer,
                    seconds: Some(50.0),
                    wave: Some(1),
                    t_s: None,
                    aligned: false,
                    needs_hud: true,
                    placed_by: None,
                    confidence: None
                }
            ],
            ..ben.clone()
        };
        let s = segments(&m);
        let texts: Vec<&str> = s.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(texts, ["hi all", "W1 :50 late"]);
        assert!(s[0].moment.is_none());
        // A short lead-in on the moment's line joins it
        let ben = &c.vods[1].messages[1];
        let s = segments(ben);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].text, "at 1:20 go left, the basket starved");
    }
}
