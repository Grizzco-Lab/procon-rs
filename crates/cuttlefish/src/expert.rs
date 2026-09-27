//! Expert comments: each reviewer's comment of the #vod-review corpus
//! ([`crate::corpus`]) as a unit of retrieval of its own.
//!
//! The whole conversations are documents already (one per thread, from the
//! inbox); a question about a moment is better served by the one comment
//! that talks about a similar moment. So every comment on a VOD by someone
//! other than its poster becomes an [`ExpertComment`]: the comment (a long
//! one split into pieces of at most [`MAX_CHARS`] at line ends), the
//! message it replies to as context, and an [`Expert`] record: reviewer,
//! date, era, the VOD and the moment it is about (wave, timer, video time
//! from the corpus's moments). One [`Document`] per VOD holds them
//! ([`document`]); the store indexes each comment as one chunk, headed by
//! its label ("Centritide, 2023 (S3), about a W2 :50 moment"), with the
//! Discord message as its link. [`index`] brings the store in line with
//! the corpus (`cuttlefish corpus index`), and [`evaluate`] checks how
//! well a moment's summary alone finds comments about the same VOD or
//! wave.

use crate::corpus::{Corpus, CorpusMoment, Vod};
use crate::doc::{Document, SourceKind};
use crate::embed::Embedder;
use crate::game::Game;
use crate::moments::MomentKind;
use crate::store::Store;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::Result;
use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::LazyLock;

/// Comments shorter than this (after removing custom emoji) are chatter
pub const MIN_CHARS: usize = 30;
/// Longest piece of a comment, in characters; longer comments are split
/// at line ends
pub const MAX_CHARS: usize = 900;
/// Characters of the replied-to message kept as context
pub const CONTEXT_CHARS: usize = 200;
/// Expert comments on one VOD among those retrieved for a request, at most
/// (a reviewer who wrote one comment per wave would fill the list alone)
pub const PER_VOD: usize = 2;

/// Who made an expert comment, when, and the moment it is about
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Expert {
    /// The reviewer's display name
    pub reviewer: String,
    /// The day of the comment
    pub date: NaiveDate,
    /// The era of the VOD
    pub game: Game,
    /// The VOD's id in the corpus (its conversation's first message)
    pub vod: String,
    /// The VOD's video link
    pub video: String,
    /// The wave the comment is about
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<u8>,
    /// Seconds left on the wave timer, as written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_s: Option<f32>,
    /// Seconds into the VOD's video, when the moment is placed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t_s: Option<f32>,
}

/// `m:ss`, or `:ss` under a minute (the community's way for the timer)
fn clock(s: f32, bare_seconds: bool) -> String {
    let s = s.max(0.0).round() as u32;
    if bare_seconds && s < 60 {
        alloc::format!(":{s:02}")
    } else {
        alloc::format!("{}:{:02}", s / 60, s % 60)
    }
}

impl Expert {
    /// The moment as the community writes it: `W2 :50`, `W2`, `:50` (a
    /// timer in a wave not known), or a time of the video (`1:20 of the
    /// video`)
    pub fn moment(&self) -> Option<String> {
        match (self.wave, self.timer_s, self.t_s) {
            (Some(w), Some(timer), _) => Some(alloc::format!("W{w} {}", clock(timer, true))),
            (Some(w), None, _) => Some(alloc::format!("W{w}")),
            (None, Some(timer), _) => Some(clock(timer, true)),
            (None, None, Some(t)) => Some(alloc::format!("{} of the video", clock(t, false))),
            (None, None, None) => None,
        }
    }

    /// How the prompt and the page name the comment:
    /// `Centritide, 2023 (S3), about a W2 :50 moment`
    pub fn label(&self) -> String {
        let era = match self.game {
            Game::S2 => "S2",
            Game::S3 => "S3",
        };
        let who = alloc::format!("{}, {} ({era})", self.reviewer, self.date.year());
        match self.moment() {
            Some(m) if self.wave.is_some() || self.timer_s.is_some() => {
                alloc::format!("{who}, about a {m} moment")
            }
            Some(m) => alloc::format!("{who}, about {m}"),
            None => who,
        }
    }
}

/// One expert comment, or one piece of a long one
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExpertComment {
    /// Link to the Discord message
    pub url: String,
    pub expert: Expert,
    /// The comment, after the message it replies to (`(replying to X:
    /// "...")`) for the first piece of a reply
    pub text: String,
}

/// Custom emoji (`<:grizzcowell:936767714578550834>`)
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<a?:\w+:\d+>").unwrap());

/// A message's text without custom emoji and runs of blank lines
fn clean(text: &str) -> String {
    let text = EMOJI.replace_all(text, "");
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push_str(if blank { "\n\n" } else { "\n" });
        }
        out.push_str(line);
        blank = false;
    }
    out
}

/// The first `max` characters of a text on one line, with `...` if cut
fn clip(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let mut s: String = one_line.chars().take(max).collect();
    s.push_str("...");
    s
}

/// A text in pieces of at most `max` characters at line ends (a longer
/// line is a piece of its own), with each piece's byte offset
fn pieces(text: &str, max: usize) -> Vec<(usize, &str)> {
    let mut out: Vec<(usize, &str)> = Vec::new();
    let (mut start, mut end) = (0, 0);
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let line_end = offset + line.len();
        if end > start && text[start..line_end].trim_end().chars().count() > max {
            out.push((start, text[start..end].trim()));
            start = offset;
        }
        end = line_end;
        offset = line_end;
    }
    if end > start {
        out.push((start, text[start..end].trim()));
    }
    out
}

/// The expert comments on a VOD: every message of its conversation but the
/// poster's, of at least [`MIN_CHARS`], in pieces of at most
/// [`MAX_CHARS`]; each piece is about the first moment in it with a wave or
/// a video time, else the wave named last before it in the message
pub fn comments(vod: &Vod) -> Vec<ExpertComment> {
    let mut out = Vec::new();
    for m in vod.messages.iter().filter(|m| m.author != vod.poster) {
        let text = clean(&m.text);
        if text.chars().count() < MIN_CHARS {
            continue;
        }
        let context = m
            .reply_to
            .as_ref()
            .and_then(|id| vod.messages.iter().find(|p| &p.id == id))
            .map(|p| (p, clean(&p.text)))
            .filter(|(_, t)| !t.is_empty())
            .map(|(p, t)| {
                alloc::format!(
                    "(replying to {}: \"{}\")",
                    p.author,
                    clip(&t, CONTEXT_CHARS)
                )
            });
        let parts = pieces(&text, MAX_CHARS);
        // The moments in each piece, found in order
        let mut placed: Vec<Vec<&CorpusMoment>> = alloc::vec![Vec::new(); parts.len()];
        let mut cursor = 0;
        for cm in &m.moments {
            if let Some(pos) = text[cursor..].find(&cm.raw).map(|p| p + cursor) {
                cursor = pos + cm.raw.len();
                let i = parts.iter().rposition(|(s, _)| *s <= pos).unwrap_or(0);
                placed[i].push(cm);
            }
        }
        let mut wave_before = None;
        for (i, (_, piece)) in parts.iter().enumerate() {
            let about = placed[i]
                .iter()
                .find(|c| c.wave.is_some() || c.t_s.is_some());
            let expert = Expert {
                reviewer: m.author.clone(),
                date: m.time.date_naive(),
                game: vod.game,
                vod: vod.id.clone(),
                video: vod.video.url.clone(),
                wave: about.map_or(wave_before, |c| c.wave),
                timer_s: about
                    .filter(|c| c.kind == MomentKind::WaveTimer)
                    .and_then(|c| c.seconds),
                t_s: about.and_then(|c| c.t_s),
            };
            if let Some(w) = placed[i].iter().rev().find_map(|c| c.wave) {
                wave_before = Some(w);
            }
            if parts.len() > 1 && piece.chars().count() < MIN_CHARS {
                continue;
            }
            let text = match (&context, i) {
                (Some(c), 0) => alloc::format!("{c}\n{piece}"),
                _ => String::from(*piece),
            };
            out.push(ExpertComment {
                url: m.url.clone(),
                expert,
                text,
            });
        }
    }
    out
}

/// The key of a VOD's expert document ([`crate::doc::doc_id`] of it)
pub fn doc_key(vod: &Vod) -> String {
    alloc::format!("vod-review-comments:{}", vod.id)
}

/// A VOD's expert comments as one document (none without any): source
/// `discord-vod-review`, the VOD's era, its first message as the link, and
/// the comments as its text too, each after its label
pub fn document(vod: &Vod) -> Option<Document> {
    let comments = comments(vod);
    if comments.is_empty() {
        return None;
    }
    let text = comments
        .iter()
        .map(|c| alloc::format!("{}: {}", c.expert.label(), c.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    let title = alloc::format!("#vod-review: {}'s VOD, {}", vod.poster, vod.date);
    let mut doc = Document::new(SourceKind::DiscordVodReview, &doc_key(vod), title, text);
    doc.url = Some(vod.url.clone());
    doc.license = Some(String::from(crate::discord::LICENSE));
    let mut reviewers: Vec<&str> = Vec::new();
    for c in &comments {
        if !reviewers.contains(&c.expert.reviewer.as_str()) {
            reviewers.push(&c.expert.reviewer);
        }
    }
    doc.attribution = Some(reviewers.join(", "));
    doc.game = Some(vod.game);
    doc.expert_comments = comments;
    Some(doc)
}

/// What [`index`] did
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Indexed {
    /// VODs with expert comments
    pub vods: usize,
    /// Their comments (pieces)
    pub comments: usize,
    /// Documents embedded anew
    pub embedded: usize,
    /// Documents stored already as they are
    pub unchanged: usize,
    /// Expert documents of VODs the corpus no longer has
    pub removed: usize,
}

impl core::fmt::Display for Indexed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} expert comments of {} VODs: {} documents embedded, {} unchanged, {} removed",
            self.comments, self.vods, self.embedded, self.unchanged, self.removed
        )
    }
}

/// What bringing the store in line with the corpus takes ([`plan`])
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Expert documents new or changed, to embed
    pub add: Vec<Document>,
    /// Ids of expert documents of VODs no longer in the corpus
    pub remove: Vec<String>,
    /// The counts, `embedded` and `removed` as planned
    pub counts: Indexed,
}

/// Compares the corpus's expert documents with the store's: a VOD's
/// document is to embed when new, changed or not indexed, and those of
/// VODs no longer in the corpus are to remove
pub fn plan(store: &Store, corpus: &Corpus) -> Result<Plan> {
    let mut out = Plan::default();
    let mut ids = BTreeSet::new();
    let indexed: BTreeSet<&str> = store
        .index()
        .entries()
        .iter()
        .map(|e| e.doc_id.as_str())
        .collect();
    for vod in &corpus.vods {
        let Some(doc) = document(vod) else { continue };
        out.counts.vods += 1;
        out.counts.comments += doc.expert_comments.len();
        ids.insert(doc.id.clone());
        let same = store.document(&doc_key(vod)).is_some_and(|old| {
            (&old.text, &old.expert_comments, old.game)
                == (&doc.text, &doc.expert_comments, doc.game)
                && indexed.contains(doc.id.as_str())
        });
        if same {
            out.counts.unchanged += 1;
        } else {
            out.counts.embedded += 1;
            out.add.push(doc);
        }
    }
    for old in store.documents()? {
        if !old.expert_comments.is_empty() && !ids.contains(&old.id) {
            out.counts.removed += 1;
            out.remove.push(old.id);
        }
    }
    Ok(out)
}

/// Brings the store's expert documents in line with the corpus
/// ([`plan`]), saving the index every ten documents; `report` hears the
/// progress. Hold the store's write lock and call [`Store::save`]
/// afterwards.
pub fn index(
    store: &mut Store,
    embedder: &dyn Embedder,
    corpus: &Corpus,
    report: &mut dyn FnMut(&str),
) -> Result<Indexed> {
    let plan = plan(store, corpus)?;
    for (i, doc) in plan.add.iter().enumerate() {
        store.add(doc, embedder)?;
        if (i + 1).is_multiple_of(10) {
            store.save()?;
            report(&alloc::format!(
                "{} of {} documents embedded",
                i + 1,
                plan.add.len()
            ));
        }
    }
    for id in &plan.remove {
        store.delete(id)?;
    }
    Ok(plan.counts)
}

/// The `k` best expert comments for a query that `keep` accepts, at most
/// [`PER_VOD`] of them on one VOD
pub fn search(
    store: &Store,
    embedder: &dyn Embedder,
    query: &str,
    k: usize,
    keep: &dyn Fn(&crate::index::Entry) -> bool,
) -> Result<Vec<crate::store::Hit>> {
    let hits = store.search_where(query, k * 4, embedder, &|e| e.expert.is_some() && keep(e))?;
    let mut out: Vec<crate::store::Hit> = Vec::new();
    for h in hits {
        let vod = h.entry.expert.as_ref().map(|x| &x.vod);
        let same = out
            .iter()
            .filter(|o| o.entry.expert.as_ref().map(|x| &x.vod) == vod)
            .count();
        if same < PER_VOD {
            out.push(h);
        }
        if out.len() == k {
            break;
        }
    }
    Ok(out)
}

/// One moment of [`evaluate`]
#[derive(Clone, Debug, PartialEq)]
pub struct EvalCase {
    /// The held-out comment's label and link
    pub label: String,
    pub url: String,
    /// The wave the HUD shows at the comment's moment
    pub wave: u8,
    /// The query built from the moment alone
    pub query: String,
    /// Rank (1-based) of the first comment on the same VOD in the top `k`
    pub same_vod_rank: Option<usize>,
    /// Comments about the same wave in the top 5
    pub same_wave_top5: usize,
    /// Share of all other comments on the same VOD (the chance of a
    /// random pick)
    pub same_vod_share: f64,
    /// Share of all other comments about the same wave
    pub same_wave_share: f64,
    /// Labels of the top 3, with whether each is on the same VOD
    pub top: Vec<(String, bool)>,
}

/// Per VOD whose video is on disk with a wave table and that has at least
/// three expert comments: its comments placed in the video, in order, and
/// the video
pub fn eval_candidates(corpus: &Corpus, knowledge: &Path) -> Vec<(Vec<ExpertComment>, String)> {
    let mut out = Vec::new();
    for vod in &corpus.vods {
        let Some(video) = vod.local_video(knowledge) else {
            continue;
        };
        if !crate::corpus::table_path(&video).is_file() {
            continue;
        }
        let all = comments(vod);
        if all.len() < 3 {
            continue;
        }
        let placed: Vec<ExpertComment> =
            all.into_iter().filter(|c| c.expert.t_s.is_some()).collect();
        if !placed.is_empty() {
            out.push((placed, video.to_string_lossy().into_owned()));
        }
    }
    out
}

/// Retrieval of expert comments from a moment alone. For up to `n` VODs
/// ([`eval_candidates`]) the first placed comment whose moment falls in a
/// wave is held out; the query is the moment's summary
/// ([`crate::situation::Situation::query`]: the HUD's wave, timer and eggs
/// at its time; no controller input, as the corpus's videos have no
/// predictions), after `question` if given; the comment's own message is
/// left out of the results, and the top `k` expert comments are compared
/// with it: on the same VOD, about the same wave.
pub fn evaluate(
    store: &Store,
    embedder: &dyn Embedder,
    corpus: &Corpus,
    knowledge: &Path,
    n: usize,
    k: usize,
    question: Option<&str>,
) -> Result<Vec<EvalCase>> {
    let entries: Vec<(Option<&str>, &Expert)> = store
        .index()
        .entries()
        .iter()
        .filter_map(|e| Some((e.url.as_deref(), e.expert.as_ref()?)))
        .collect();
    let mut out = Vec::new();
    for (placed, video) in eval_candidates(corpus, knowledge) {
        if out.len() >= n {
            break;
        }
        let mut found = None;
        for c in placed {
            let t = f64::from(c.expert.t_s.unwrap_or_default());
            if let Some(hud) = crate::situation::hud_at(Path::new(&video), t)? {
                found = Some((c, hud));
                break;
            }
        }
        let Some((held, hud)) = found else { continue };
        let wave = hud.wave;
        let situation = crate::situation::Situation {
            hud: alloc::vec![hud],
            ..Default::default()
        };
        let mut query = String::from(question.unwrap_or_default().trim());
        if !query.is_empty() {
            query.push('\n');
        }
        query.push_str(&situation.query());
        let hits = search(store, embedder, &query, k, &|e| {
            e.url.as_deref() != Some(held.url.as_str())
        })?;
        let same_vod = |e: &Expert| e.vod == held.expert.vod;
        let same_wave = |e: &Expert| e.wave == Some(wave);
        let others: Vec<&Expert> = entries
            .iter()
            .filter(|(url, _)| *url != Some(held.url.as_str()))
            .map(|(_, e)| *e)
            .collect();
        let share = |f: &dyn Fn(&Expert) -> bool| {
            others.iter().filter(|e| f(e)).count() as f64 / others.len().max(1) as f64
        };
        let experts: Vec<&Expert> = hits
            .iter()
            .filter_map(|h| h.entry.expert.as_ref())
            .collect();
        out.push(EvalCase {
            label: held.expert.label(),
            url: held.url.clone(),
            wave,
            query,
            same_vod_rank: experts.iter().position(|e| same_vod(e)).map(|i| i + 1),
            same_wave_top5: experts.iter().take(5).filter(|e| same_wave(e)).count(),
            same_vod_share: share(&same_vod),
            same_wave_share: share(&same_wave),
            top: experts
                .iter()
                .take(3)
                .map(|e| (e.label(), same_vod(e)))
                .collect(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::tests::fixture;
    use crate::embed::HashEmbedder;

    fn souper(root: &Path) -> Vod {
        let corpus = crate::corpus::build(root).unwrap();
        corpus
            .vods
            .into_iter()
            .find(|v| v.poster == "souper")
            .unwrap()
    }

    #[test]
    fn labels_name_reviewer_year_era_and_moment() {
        let mut e = Expert {
            reviewer: String::from("Centritide"),
            date: NaiveDate::from_ymd_opt(2023, 5, 1).unwrap(),
            game: Game::S3,
            vod: String::from("1"),
            video: String::from("https://youtu.be/x"),
            wave: Some(2),
            timer_s: Some(50.0),
            t_s: Some(130.0),
        };
        assert_eq!(e.label(), "Centritide, 2023 (S3), about a W2 :50 moment");
        e.timer_s = Some(83.0);
        assert_eq!(e.label(), "Centritide, 2023 (S3), about a W2 1:23 moment");
        e.timer_s = None;
        assert_eq!(e.label(), "Centritide, 2023 (S3), about a W2 moment");
        (e.wave, e.timer_s) = (None, Some(50.0));
        assert_eq!(e.label(), "Centritide, 2023 (S3), about a :50 moment");
        e.timer_s = None;
        e.game = Game::S2;
        assert_eq!(e.label(), "Centritide, 2023 (S2), about 2:10 of the video");
        e.t_s = None;
        assert_eq!(e.label(), "Centritide, 2023 (S2)");
    }

    #[test]
    fn long_comments_split_at_line_ends() {
        let text = "aaaa\nbbbb\ncccc";
        assert_eq!(pieces(text, 9), [(0, "aaaa\nbbbb"), (10, "cccc")]);
        assert_eq!(pieces(text, 100), [(0, text)]);
        // A line longer than the limit is a piece of its own
        assert_eq!(
            pieces("x\nyyyyyy\nz", 3),
            [(0, "x"), (2, "yyyyyy"), (9, "z")]
        );
        assert_eq!(clean("hi <:grizz:123>\n\n\n\nthere <a:x:9>"), "hi\n\nthere");
        assert_eq!(clip("a  b\nc", 3), "a b...");
    }

    #[test]
    fn comments_carry_their_moment_and_reply_context() {
        let root = fixture("expert");
        let vod = souper(&root);
        let c = comments(&vod);
        // Ben's two messages; souper's own are not expert comments
        assert_eq!(c.len(), 2, "{c:#?}");
        assert!(c.iter().all(|c| c.expert.reviewer == "Ben"));
        assert_eq!(c[0].expert.t_s, Some(80.0));
        assert_eq!(c[0].expert.wave, None);
        assert!(c[0].text.starts_with("(replying to souper: \"my run https://youtu.be/abcdefghijk feedback welcome\")\nat 1:20"));
        assert_eq!(
            c[0].expert.label(),
            "Ben, 2023 (S3), about 1:20 of the video"
        );
        // "W2 was rough" names the wave; the first timer after it is :50
        assert_eq!(c[1].expert.wave, Some(2));
        assert_eq!(c[1].expert.label(), "Ben, 2023 (S3), about a W2 moment");
        assert!(c[1].url.ends_with("/202"));
        let doc = document(&vod).unwrap();
        assert_eq!(doc.source, SourceKind::DiscordVodReview);
        assert_eq!(doc.game, Some(Game::S3));
        assert_eq!(doc.attribution.as_deref(), Some("Ben"));
        assert!(
            doc.text
                .starts_with("Ben, 2023 (S3), about 1:20 of the video: (replying to")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn indexing_makes_one_entry_per_comment() {
        let root = fixture("expert-index");
        let corpus = crate::corpus::build(&root).unwrap();
        let e = HashEmbedder { dim: 64 };
        let mut store = Store::open(&root.join("knowledge"), &e).unwrap();
        let done = index(&mut store, &e, &corpus, &mut |_| {}).unwrap();
        // Ben's two comments on souper's VOD, Dee's on Cy's (the other is short)
        assert_eq!((done.vods, done.comments, done.embedded), (2, 3, 2));
        let experts: Vec<_> = store
            .index()
            .entries()
            .iter()
            .filter(|e| e.expert.is_some())
            .collect();
        assert_eq!(experts.len(), 3);
        let ben = experts.iter().find(|e| e.text.contains("1:20")).unwrap();
        assert_eq!(ben.heading, "Ben, 2023 (S3), about 1:20 of the video");
        assert!(ben.url.as_deref().unwrap().ends_with("/201"));
        assert_eq!(ben.video.as_deref(), Some("https://youtu.be/abcdefghijk"));
        assert_eq!(ben.source, SourceKind::DiscordVodReview);
        assert_eq!(ben.weight, SourceKind::DiscordVodReview.default_weight());
        // Again: nothing changed
        let again = index(&mut store, &e, &corpus, &mut |_| {}).unwrap();
        assert_eq!((again.embedded, again.unchanged), (0, 2));
        // A VOD gone from the corpus loses its document
        let fewer = Corpus {
            vods: corpus
                .vods
                .iter()
                .filter(|v| v.poster != "Cy")
                .cloned()
                .collect(),
        };
        let gone = index(&mut store, &e, &fewer, &mut |_| {}).unwrap();
        assert_eq!(gone.removed, 1);
        assert_eq!(
            store
                .index()
                .entries()
                .iter()
                .filter(|e| e.expert.is_some())
                .count(),
            2
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
