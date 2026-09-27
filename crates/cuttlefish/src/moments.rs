//! Moments a review comment points at: YouTube links with a time, and
//! times written in the text ("1:20", "W1 :50", "86s", "wave 3").
//!
//! Salmon Run's wave timer counts down from 100 s, so a bare number of
//! seconds up to 100 usually means the timer, not the video. Each moment
//! keeps the text as written and a guess of what it means
//! ([`MomentKind`]), for aligning comments with a video later.

use alloc::string::String;
use alloc::vec::Vec;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The wave timer starts at this many seconds
pub const WAVE_S: f32 = 100.0;

/// What a time in a comment refers to
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MomentKind {
    /// Time into the video
    VideoTime,
    /// Seconds left on the wave timer
    WaveTimer,
    /// Cannot tell (a wave without a time, an ambiguous number)
    Unknown,
}

/// One reference to a moment
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Moment {
    /// As written
    pub raw: String,
    /// The guess
    pub kind: MomentKind,
    /// Seconds into the video, or left on the wave timer
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<f32>,
    /// Wave number when the text says one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<u8>,
    /// The video, for links
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The chat message it is in
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Who wrote it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// When it was written
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
}

impl Moment {
    fn new(raw: &str, kind: MomentKind) -> Self {
        Moment {
            raw: String::from(raw),
            kind,
            seconds: None,
            wave: None,
            url: None,
            message_id: None,
            author: None,
            at: None,
        }
    }
}

/// A YouTube link, up to whitespace or a closing bracket
static YOUTUBE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)https?://(?:www\.|m\.|music\.)?(?:youtube\.com/(?:watch|live|shorts|embed)[^\s<>()\[\]]*|youtu\.be/[^\s<>()\[\]]*)",
    )
    .unwrap()
});

/// The start time of a YouTube link: `t=` or `start=`, `83`, `83s`, `1m23s`
static START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[?&#](?:t|start)=([0-9hms]+)").unwrap());

/// A wave, with a time after it or not: `W1 :50`, `wave 2 at 1:20`,
/// `wave 3`, `w3, 86s`
static WAVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:wave|w)\s?([1-5])\b[\s,@-]*(?:at\s+)?((?:\d{1,2}:)?\d{1,2}:\d{2}|:\d{2}|\d{1,3}\s?(?:s\b|\u{79d2})|\d{1,2}m\d{1,2}s?\b)?",
    )
    .unwrap()
});

/// A time on its own: `1:02:03`, `1:20`, `:50`, `86s`, `86 s`, `1m20s`
/// (and seconds written with the CJK second sign)
static TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^\d:])(\d{1,2}:\d{2}:\d{2}|\d{1,2}:\d{2}|:\d{2}|\d{1,3}\s?(?:s\b|\u{79d2})|\d{1,2}m\d{1,2}s?\b)(?:[^\d:]|$)",
    )
    .unwrap()
});

/// Seconds of a time written as `h:mm:ss`, `m:ss`, `:ss`, `86s`, `86 s`,
/// `1m20s`, `1m20`, `83` (plain seconds, links only when `plain`)
fn parse_seconds(s: &str, plain: bool) -> Option<f32> {
    let s = s.trim().to_lowercase();
    if let Some(rest) = s.strip_prefix(':') {
        return rest.parse::<f32>().ok();
    }
    if s.contains(':') {
        let mut total = 0.0;
        for part in s.split(':') {
            total = total * 60.0 + part.trim().parse::<f32>().ok()?;
        }
        return Some(total);
    }
    let digits: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || matches!(c, 'h' | 'm' | 's'))
        .collect();
    if digits.is_empty() || !digits.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if digits.chars().all(|c| c.is_ascii_digit()) {
        return if plain || s.ends_with('\u{79d2}') {
            digits.parse().ok()
        } else {
            None
        };
    }
    let (mut total, mut n) = (0.0, String::new());
    for c in digits.chars() {
        match c {
            'h' | 'm' | 's' => {
                let v: f32 = n.parse().ok()?;
                total += v * match c {
                    'h' => 3600.0,
                    'm' => 60.0,
                    _ => 1.0,
                };
                n.clear();
            }
            d => n.push(d),
        }
    }
    if !n.is_empty() {
        total += n.parse::<f32>().ok()?;
    }
    Some(total)
}

/// Whether a time in a comment is the wave timer or the video: `m:ss` and
/// `1m20s` are how video times are written, so those are the video unless
/// a wave is named and the time fits the timer; bare seconds up to
/// [`WAVE_S`] are the timer
fn guess(raw: &str, seconds: f32, with_wave: bool) -> MomentKind {
    let video_form = (raw.contains(':') && !raw.starts_with(':')) || raw.contains(['m', 'M']);
    if seconds > WAVE_S {
        MomentKind::VideoTime
    } else if with_wave || !video_form {
        MomentKind::WaveTimer
    } else {
        MomentKind::VideoTime
    }
}

/// The moments a text refers to, in order of appearance: YouTube links
/// (with their start time when they have one), waves with or without a
/// time, and times on their own
pub fn extract(text: &str) -> Vec<Moment> {
    let mut out = Vec::new();
    // Text with what was taken blanked, so a time is not taken twice
    let mut rest = String::from(text);
    for m in YOUTUBE.find_iter(text) {
        let url = m.as_str().trim_end_matches(['.', ',', ';', '!', '?']);
        let mut moment = Moment::new(url, MomentKind::VideoTime);
        moment.url = Some(String::from(url));
        moment.seconds = START.captures(url).and_then(|c| parse_seconds(&c[1], true));
        out.push((m.start(), moment));
        blank(&mut rest, m.start(), m.end());
    }
    let waves: Vec<(usize, usize, Moment)> = WAVE
        .captures_iter(&rest)
        .map(|c| {
            let whole = c.get(0).unwrap();
            let raw = whole
                .as_str()
                .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, ',' | '@' | '-'));
            let wave = c[1].parse::<u8>().ok();
            let mut moment = Moment::new(raw, MomentKind::Unknown);
            moment.wave = wave;
            if let Some(t) = c.get(2) {
                moment.seconds = parse_seconds(t.as_str(), false);
                if let Some(s) = moment.seconds {
                    moment.kind = guess(t.as_str(), s, true);
                }
            }
            (whole.start(), whole.end(), moment)
        })
        .collect();
    for (start, end, moment) in waves {
        out.push((start, moment));
        blank(&mut rest, start, end);
    }
    let times: Vec<(usize, Moment)> = TIME
        .captures_iter(&rest)
        .filter_map(|c| {
            let t = c.get(1)?;
            let seconds = parse_seconds(t.as_str(), false)?;
            let mut moment = Moment::new(t.as_str(), guess(t.as_str(), seconds, false));
            moment.seconds = Some(seconds);
            Some((t.start(), moment))
        })
        .collect();
    out.extend(times);
    out.sort_by_key(|(start, _)| *start);
    out.into_iter().map(|(_, m)| m).collect()
}

/// Replaces `text[start..end]` with spaces
fn blank(text: &mut String, start: usize, end: usize) {
    let spaces: String = core::iter::repeat_n(' ', end - start).collect();
    text.replace_range(start..end, &spaces);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(String, MomentKind, Option<f32>, Option<u8>)> {
        extract(text)
            .into_iter()
            .map(|m| (m.raw, m.kind, m.seconds, m.wave))
            .collect()
    }

    #[test]
    fn youtube_links_with_start_times() {
        let m = extract(
            "see https://youtu.be/abc?t=83 and https://www.youtube.com/watch?v=x&t=1m23s, also https://youtube.com/watch?v=y.",
        );
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].url.as_deref(), Some("https://youtu.be/abc?t=83"));
        assert_eq!(m[0].seconds, Some(83.0));
        assert_eq!(m[0].kind, MomentKind::VideoTime);
        assert_eq!(m[1].seconds, Some(83.0));
        assert_eq!(m[2].url.as_deref(), Some("https://youtube.com/watch?v=y"));
        assert_eq!(m[2].seconds, None);
    }

    #[test]
    fn waves_and_timers() {
        assert_eq!(
            kinds("W1 :50 the basket starved"),
            [(
                String::from("W1 :50"),
                MomentKind::WaveTimer,
                Some(50.0),
                Some(1)
            )]
        );
        assert_eq!(
            kinds("wave 2 at 1:20 you left"),
            [(
                String::from("wave 2 at 1:20"),
                MomentKind::WaveTimer,
                Some(80.0),
                Some(2)
            )]
        );
        assert_eq!(
            kinds("wave 3 was fine"),
            [(String::from("wave 3"), MomentKind::Unknown, None, Some(3))]
        );
        // A wave at the end of a line
        assert_eq!(
            kinds("wipe on W3\nclip.mp4: https://cdn.example/clip.mp4")[0].0,
            "W3"
        );
        assert_eq!(
            kinds("w3, 86s: two Steelheads"),
            [(
                String::from("w3, 86s"),
                MomentKind::WaveTimer,
                Some(86.0),
                Some(3)
            )]
        );
        // Over 100 s cannot be the timer
        assert_eq!(
            kinds("wave 1 2:30"),
            [(
                String::from("wave 1 2:30"),
                MomentKind::VideoTime,
                Some(150.0),
                Some(1)
            )]
        );
    }

    #[test]
    fn times_on_their_own() {
        assert_eq!(
            kinds("at 1:20 you should have gone left"),
            [(
                String::from("1:20"),
                MomentKind::VideoTime,
                Some(80.0),
                None
            )]
        );
        assert_eq!(
            kinds("86s is when the Flyfish landed"),
            [(String::from("86s"), MomentKind::WaveTimer, Some(86.0), None)]
        );
        assert_eq!(
            kinds("around :50 the eggs were left"),
            [(String::from(":50"), MomentKind::WaveTimer, Some(50.0), None)]
        );
        assert_eq!(
            kinds("1:02:03 in the vod, 1m20s in the clip"),
            [
                (
                    String::from("1:02:03"),
                    MomentKind::VideoTime,
                    Some(3723.0),
                    None
                ),
                (
                    String::from("1m20s"),
                    MomentKind::VideoTime,
                    Some(80.0),
                    None
                )
            ]
        );
        assert_eq!(
            kinds("50\u{79d2}"),
            [(
                String::from("50\u{79d2}"),
                MomentKind::WaveTimer,
                Some(50.0),
                None
            )]
        );
        // Not times: plain numbers, ids, words ending in s
        assert!(extract("3 eggs, 12 bosses, id 1234567890").is_empty());
    }

    #[test]
    fn order_and_no_double_take() {
        let m = extract("W1 :50 then 1:20 https://youtu.be/a?t=5");
        let raws: Vec<&str> = m.iter().map(|m| m.raw.as_str()).collect();
        assert_eq!(raws, ["W1 :50", "1:20", "https://youtu.be/a?t=5"]);
    }
}
