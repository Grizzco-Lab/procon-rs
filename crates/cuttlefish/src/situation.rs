//! What happens at a moment of a video, as text: a few lines instead of
//! more frames, for the prompt (the `<moment>` block) and for the
//! retrieval query that finds expert comments about similar situations.
//!
//! A [`Situation`] holds what is known about the moment or range:
//!
//! - the controller input ([`summarize_input`]): recorded for the studio's
//!   own sessions (`controller.bin`, aligned per frame by `gameplay-data`),
//!   else predicted from the video by AgentZero's IDM when the Predictor
//!   has run on it. Both are per-frame [`Label`]s; the summary names held
//!   and tapped buttons with what they do in Salmon Run, squid rolls (a
//!   left-stick flick back and B while swimming, within
//!   [`ROLL_WINDOW_S`]), left-stick movement and the camera turn;
//! - the HUD ([`hud_at`]): wave and timer from the video's wave table
//!   (`<stem>.wave_starts.json`, `gameplay-vision`'s HUD reader), and the
//!   golden eggs read from the frames around the moment;
//! - objects on the frame: boxes a person labelled (the shared annotations
//!   format), and, as a hook for later, objects a detector found.

use crate::corpus;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::Result;
use gameplay_data::labels::Label;
use gameplay_vision::hud::{self, video};
use std::path::Path;

/// A button held at least this long is "held", seconds
pub const HELD_S: f64 = 0.4;
/// Presses of a button closer than this are one group of taps, seconds
pub const TAP_GAP_S: f64 = 0.5;
/// Stick deflection that counts, as a share of the half range (2048)
pub const STICK_ON: f64 = 0.35;
/// Shortest left-stick movement listed, seconds
pub const MOVE_MIN_S: f64 = 0.4;
/// Camera turn that counts, degrees per second
pub const TURN_ON_DPS: f64 = 30.0;
/// Shortest turn listed, seconds
pub const TURN_MIN_S: f64 = 0.25;
/// A squid roll's stick flick comes at most this long before its B,
/// seconds
pub const ROLL_WINDOW_S: f64 = 0.25;
/// Least change of the left stick's direction in a squid roll's flick,
/// degrees
pub const ROLL_ANGLE: f64 = 120.0;
/// Most input lines in a summary; the rest are counted
pub const MAX_LINES: usize = 14;
/// Degrees of gyro yaw per frame one raw unit of right stick is worth,
/// AgentZero's shared camera-turn fit (`b / a`, `agentzero.idm.turn`)
pub const STICK_YAW: f64 = 0.00137;
/// Pixels (at 640 x 360) the camera turns per degree of yaw, the same fit's
/// `a`: an IDM's `camera_turn` over this is degrees of yaw
pub const TURN_PX_PER_DEG: f64 = 7.68;
/// Center of a raw 12-bit stick axis
const STICK_CENTER: f64 = 2048.0;
/// HUD frames read for the egg count: this far around the moment, seconds
pub const EGGS_AROUND_S: f64 = 0.5;
/// ... every this many seconds
pub const EGGS_EVERY_S: f64 = 0.25;

/// Buttons worth naming: label name, name shown, what it does in Salmon Run
const BUTTONS: [(&str, &str, Option<&str>); 16] = [
    ("zr", "ZR", Some("shooting")),
    ("zl", "ZL", Some("swimming")),
    ("r", "R", Some("sub weapon")),
    ("r_stick", "R-stick click", Some("special")),
    ("b", "B", Some("jump")),
    ("y", "Y", Some("camera reset")),
    ("a", "A", None),
    ("x", "X", None),
    ("l", "L", None),
    ("up", "D-pad up", None),
    ("down", "D-pad down", None),
    ("left", "D-pad left", None),
    ("right", "D-pad right", None),
    ("l_stick", "L-stick click", None),
    ("plus", "+", None),
    ("minus", "-", None),
];

/// Where controller input comes from
#[derive(Clone, Debug, PartialEq)]
pub enum InputSource {
    /// The controller, recorded with the video
    Recorded,
    /// AgentZero's IDM, from the video; `model` is its checkpoint
    Predicted { model: String },
}

/// Controller input over a stretch, summarized
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub source: InputSource,
    /// The stretch, seconds of the video
    pub start_s: f64,
    pub end_s: f64,
    /// One line per event, in time order (`12.0–13.5 s: ZR held
    /// (shooting)`)
    pub lines: Vec<String>,
    /// What the player did, for the retrieval query (`shooting`, `squid
    /// roll`, `turning right`), each once
    pub actions: Vec<String>,
}

/// The HUD at a moment
#[derive(Clone, Debug, PartialEq)]
pub struct HudState {
    /// Seconds of the video
    pub t_s: f64,
    pub wave: u8,
    /// An extra wave (the King Salmonid)
    pub extra: bool,
    /// Seconds left on the timer
    pub timer_s: u8,
    /// Golden eggs delivered and the quota, when read
    pub eggs: Option<(u16, u16)>,
}

impl HudState {
    /// `wave 2, 43 s left (W2 :43), golden eggs 18/24`
    pub fn describe(&self) -> String {
        let mut s = if self.extra {
            alloc::format!("extra wave (King Salmonid), {} s left", self.timer_s)
        } else {
            alloc::format!(
                "wave {}, {} s left (W{} {})",
                self.wave,
                self.timer_s,
                self.wave,
                timer(self.timer_s)
            )
        };
        if let Some((got, quota)) = self.eggs {
            s.push_str(&alloc::format!(", golden eggs {got}/{quota}"));
        }
        s
    }
}

/// The timer as players write it: `:43`, `1:05`
fn timer(s: u8) -> String {
    if s < 60 {
        alloc::format!(":{s:02}")
    } else {
        alloc::format!("{}:{:02}", s / 60, s % 60)
    }
}

/// An object on a frame
#[derive(Clone, Debug, PartialEq)]
pub struct SeenObject {
    /// Class name (`steelhead`, `golden_egg`)
    pub class: String,
    /// Center, as fractions of the frame (x right, y down)
    pub x: f64,
    pub y: f64,
}

/// What is known about a moment or range of a video
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Situation {
    /// The controller input over the stretch
    pub input: Option<Input>,
    /// The HUD at the moment (at the start and the end of a range)
    pub hud: Vec<HudState>,
    /// The frame the objects are on, seconds
    pub objects_t_s: Option<f64>,
    /// Objects a person labelled on that frame
    pub labelled: Vec<SeenObject>,
    /// Objects a detector found on that frame. Nothing fills it yet: the
    /// studio will from the Vision app's detections (the same annotations
    /// format, `by: "model"`) once a Salmon Run detector is trained.
    pub detected: Vec<SeenObject>,
}

/// `12.0–13.5 s`, or `14.2 s` for a moment
fn span(a: f64, b: f64) -> String {
    if b - a < 0.05 {
        alloc::format!("{a:.1} s")
    } else {
        alloc::format!("{a:.1}\u{2013}{b:.1} s")
    }
}

/// Objects by class in order of appearance, with where they are:
/// `chum ×2 (left, center), steelhead (right)`
fn describe_objects(objects: &[SeenObject]) -> String {
    let mut classes: Vec<(&str, Vec<&'static str>)> = Vec::new();
    for o in objects {
        let at = if o.x < 1.0 / 3.0 {
            "left"
        } else if o.x > 2.0 / 3.0 {
            "right"
        } else {
            "center"
        };
        match classes.iter_mut().find(|(c, _)| *c == o.class) {
            Some((_, places)) => places.push(at),
            None => classes.push((&o.class, alloc::vec![at])),
        }
    }
    classes
        .iter()
        .map(|(class, places)| {
            let name = class.replace('_', " ");
            if places.len() == 1 {
                alloc::format!("{name} ({})", places[0])
            } else {
                alloc::format!("{name} \u{d7}{} ({})", places.len(), places.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl Situation {
    /// Nothing known
    pub fn is_empty(&self) -> bool {
        self.input.is_none()
            && self.hud.is_empty()
            && self.labelled.is_empty()
            && self.detected.is_empty()
    }

    /// The `<moment>` block of the prompt; empty when nothing is known
    pub fn block(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut s = String::from("<moment>\n");
        for h in &self.hud {
            s.push_str(&alloc::format!("HUD at {:.1} s: {}\n", h.t_s, h.describe()));
        }
        if let Some(input) = &self.input {
            let from = match &input.source {
                InputSource::Recorded => String::from("recorded from the controller"),
                InputSource::Predicted { model } => alloc::format!(
                    "predicted from the video by the inverse dynamics model {model}; an estimate"
                ),
            };
            s.push_str(&alloc::format!(
                "Controller input {}, {from}:\n",
                span(input.start_s, input.end_s)
            ));
            for line in &input.lines {
                s.push_str(&alloc::format!("- {line}\n"));
            }
        }
        let at = self.objects_t_s.unwrap_or_default();
        if !self.labelled.is_empty() {
            s.push_str(&alloc::format!(
                "Objects a person labelled on the frame at {at:.1} s: {}\n",
                describe_objects(&self.labelled)
            ));
        }
        if !self.detected.is_empty() {
            s.push_str(&alloc::format!(
                "Objects detected on the frame at {at:.1} s: {}\n",
                describe_objects(&self.detected)
            ));
        }
        s.push_str("</moment>");
        s
    }

    /// The situation for a retrieval query: the wave and timer as the
    /// community writes them (`W2 :43`), the eggs, what the player did and
    /// what is on screen, without times of the video
    pub fn query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(h) = self.hud.first() {
            let mut s = if h.extra {
                alloc::format!("extra wave, King Salmonid, {} seconds left", h.timer_s)
            } else {
                alloc::format!(
                    "W{} {}, wave {} with {} seconds left",
                    h.wave,
                    timer(h.timer_s),
                    h.wave,
                    h.timer_s
                )
            };
            if let Some((got, quota)) = h.eggs {
                s.push_str(&alloc::format!(", {got} of {quota} golden eggs"));
            }
            parts.push(s);
        }
        if let Some(input) = &self.input
            && !input.actions.is_empty()
        {
            parts.push(alloc::format!("player: {}", input.actions.join(", ")));
        }
        let mut seen: Vec<String> = Vec::new();
        for o in self.labelled.iter().chain(&self.detected) {
            let name = o.class.replace('_', " ");
            if !seen.contains(&name) {
                seen.push(name);
            }
        }
        if !seen.is_empty() {
            parts.push(alloc::format!("on screen: {}", seen.join(", ")));
        }
        parts.join("\n")
    }
}

/// `[start, end)` index runs of `on`, joining gaps of at most `gap`
fn runs(on: &[bool], gap: usize) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < on.len() {
        if !on[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < on.len() && on[i] {
            i += 1;
        }
        match out.last_mut() {
            Some(last) if start - last.1 <= gap => last.1 = i,
            _ => out.push((start, i)),
        }
    }
    out
}

/// A stick as `[x, y]` shares of the half range, y up
fn stick(raw: Option<[f64; 2]>) -> Option<[f64; 2]> {
    raw.map(|[x, y]| {
        [
            (x - STICK_CENTER) / STICK_CENTER,
            (y - STICK_CENTER) / STICK_CENTER,
        ]
    })
}

/// Direction of the left stick as one of eight, when deflected
fn octant(v: [f64; 2]) -> Option<usize> {
    let [x, y] = v;
    if (x * x + y * y).sqrt() < STICK_ON {
        return None;
    }
    let angle = y.atan2(x).to_degrees().rem_euclid(360.0);
    Some(((angle + 22.5) / 45.0) as usize % 8)
}

/// Names of the eight directions, counter-clockwise from the right
const DIRECTIONS: [&str; 8] = [
    "right",
    "forward-right",
    "forward",
    "forward-left",
    "left",
    "back-left",
    "back",
    "back-right",
];

/// Angle between two stick directions, degrees
fn angle_between(a: [f64; 2], b: [f64; 2]) -> f64 {
    let dot = a[0] * b[0] + a[1] * b[1];
    let norm = (a[0].hypot(a[1]) * b[0].hypot(b[1])).max(1e-9);
    (dot / norm).clamp(-1.0, 1.0).acos().to_degrees()
}

/// The camera turn to the right in degrees per second of one frame: an
/// IDM's `camera_turn` when it has one, else gyro yaw and the right stick
/// through [`STICK_YAW`] (gyro z turns left, the stick's x right)
fn turn_right_dps(label: &Label, fps: f64) -> f64 {
    let turn_x = label
        .extra
        .get("camera_turn")
        .and_then(|v| v.get(0))
        .and_then(serde_json::Value::as_f64);
    let per_frame = match turn_x {
        Some(px) => -px / TURN_PX_PER_DEG,
        None => {
            let yaw = label.gyro_deg.map_or(0.0, |g| g[2]);
            let rx = label.right_stick.map_or(STICK_CENTER, |s| s[0]);
            -yaw + STICK_YAW * (rx - STICK_CENTER)
        }
    };
    per_frame * fps
}

/// A summary of per-frame controller labels (recorded or predicted; one
/// per frame, in order, `frame` counted from the video's start) at `fps`
pub fn summarize_input(labels: &[Label], fps: f64, source: InputSource) -> Input {
    let t = |i: usize| labels[i].frame as f64 / fps;
    let end_t = |i: usize| (labels[i - 1].frame + 1) as f64 / fps;
    let (start_s, end_s) = match labels {
        [] => (0.0, 0.0),
        _ => (t(0), end_t(labels.len())),
    };
    let valid: Vec<bool> = labels.iter().map(|l| l.valid != Some(false)).collect();
    let mut input = Input {
        source,
        start_s,
        end_s,
        lines: Vec::new(),
        actions: Vec::new(),
    };
    if !valid.contains(&true) {
        input
            .lines
            .push(String::from("no controller input known here"));
        return input;
    }
    let pressed = |name: &str| -> Vec<bool> {
        labels
            .iter()
            .zip(&valid)
            .map(|(l, v)| {
                *v && l
                    .buttons
                    .as_ref()
                    .is_some_and(|b| b.iter().any(|x| x == name))
            })
            .collect()
    };
    let mut events: Vec<(f64, String)> = Vec::new();
    let act = |input: &mut Input, what: &str| {
        if !input.actions.iter().any(|a| a == what) {
            input.actions.push(String::from(what));
        }
    };
    let left: Vec<Option<[f64; 2]>> = labels
        .iter()
        .zip(&valid)
        .map(|(l, v)| stick(l.left_stick).filter(|_| *v))
        .collect();
    // Squid rolls: B while swimming, the left stick flicked back just before
    let zl = pressed("zl");
    let window = ((ROLL_WINDOW_S * fps).round() as usize).max(2);
    let mut rolls: Vec<usize> = Vec::new();
    for (b, _) in runs(&pressed("b"), 0) {
        let swimming = zl[b.saturating_sub(2)..=b].contains(&true);
        let now = (b.saturating_sub(2)..=b)
            .filter_map(|i| left[i])
            .find(|v| octant(*v).is_some());
        let flicked = now.is_some_and(|now| {
            (b.saturating_sub(window)..b.saturating_sub(2))
                .filter_map(|i| left[i])
                .any(|v| octant(v).is_some() && angle_between(v, now) >= ROLL_ANGLE)
        });
        if swimming && flicked {
            rolls.push(b);
            events.push((
                t(b),
                alloc::format!(
                    "{}: squid roll (left stick flicked back + B while swimming)",
                    span(t(b), t(b))
                ),
            ));
            act(&mut input, "squid roll");
        }
    }
    // Buttons: held, tapped, or pressed once
    let tap_gap = (TAP_GAP_S * fps).round() as usize;
    for (name, shown, what) in BUTTONS {
        let presses: Vec<(usize, usize)> = runs(&pressed(name), 1)
            .into_iter()
            .filter(|(a, _)| !(name == "b" && rolls.contains(a)))
            .collect();
        let mut groups: Vec<Vec<(usize, usize)>> = Vec::new();
        for p in presses {
            match groups.last_mut() {
                Some(g) if p.0 - g[g.len() - 1].1 <= tap_gap => g.push(p),
                _ => groups.push(alloc::vec![p]),
            }
        }
        let what_text = what.map_or(String::new(), |w| alloc::format!(" ({w})"));
        for g in groups {
            let (a, b) = (g[0].0, g[g.len() - 1].1);
            let line = if g.len() > 1 {
                alloc::format!(
                    "{}: {shown} \u{d7}{}{what_text}",
                    span(t(a), end_t(b)),
                    g.len()
                )
            } else if end_t(b) - t(a) >= HELD_S {
                alloc::format!("{}: {shown} held{what_text}", span(t(a), end_t(b)))
            } else {
                alloc::format!("{}: {shown}{what_text}", span(t(a), t(a)))
            };
            events.push((t(a), line));
            if let Some(w) = what {
                act(&mut input, w);
            }
        }
    }
    // Moving: the left stick in one direction for a while
    let min_move = (MOVE_MIN_S * fps).round() as usize;
    for (d, name) in DIRECTIONS.iter().enumerate() {
        let on: Vec<bool> = left.iter().map(|v| v.and_then(octant) == Some(d)).collect();
        for (a, b) in runs(&on, 3).into_iter().filter(|(a, b)| b - a >= min_move) {
            events.push((
                t(a),
                alloc::format!("{}: moving {name} (left stick)", span(t(a), end_t(b))),
            ));
            act(&mut input, "moving");
        }
    }
    // Turning: the camera turn, smoothed over five frames
    let raw: Vec<f64> = labels
        .iter()
        .zip(&valid)
        .map(|(l, v)| if *v { turn_right_dps(l, fps) } else { 0.0 })
        .collect();
    let smooth: Vec<f64> = (0..raw.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(2), (i + 3).min(raw.len()));
            raw[a..b].iter().sum::<f64>() / (b - a) as f64
        })
        .collect();
    let min_turn = (TURN_MIN_S * fps).round() as usize;
    for (sign, side) in [(1.0, "right"), (-1.0, "left")] {
        let on: Vec<bool> = smooth.iter().map(|r| r * sign > TURN_ON_DPS).collect();
        for (a, b) in runs(&on, 2).into_iter().filter(|(a, b)| b - a >= min_turn) {
            let rates: Vec<f64> = smooth[a..b].iter().map(|r| (r * sign).max(0.0)).collect();
            let mean = rates.iter().sum::<f64>() / rates.len() as f64;
            let peak = rates.iter().copied().fold(0.0, f64::max);
            events.push((
                t(a),
                alloc::format!(
                    "{}: turning {side} ~{mean:.0}\u{b0}/s (peak {peak:.0}\u{b0}/s)",
                    span(t(a), end_t(b))
                ),
            ));
            act(&mut input, &alloc::format!("turning {side}"));
        }
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let more = events.len().saturating_sub(MAX_LINES);
    input.lines = events.into_iter().take(MAX_LINES).map(|(_, l)| l).collect();
    if more > 0 {
        input.lines.push(alloc::format!("... and {more} more"));
    }
    if input.lines.is_empty() {
        input.lines.push(String::from(
            "no buttons, the sticks near the center, the camera still",
        ));
    }
    input
}

/// The HUD at `t_s` of a video with a wave table beside it: the wave and
/// timer from the table, the golden eggs read from the frames within
/// [`EGGS_AROUND_S`] (the reading nearest `t_s`; none if the reader fails).
/// `None` without a table or outside every wave.
pub fn hud_at(video: &Path, t_s: f64) -> Result<Option<HudState>> {
    let Some(table) = corpus::load_table(video)? else {
        return Ok(None);
    };
    let Some(wave) = table
        .waves
        .waves
        .iter()
        .find(|w| t_s >= w.start_video_s && t_s <= w.end_video_s)
    else {
        return Ok(None);
    };
    let eggs = read_eggs(video, table.waves.region, t_s).unwrap_or_else(|e| {
        log::warn!(
            "reading the eggs of {} at {t_s:.1} s: {e:#}",
            video.display()
        );
        None
    });
    Ok(Some(HudState {
        t_s,
        wave: wave.wave,
        extra: wave.extra,
        timer_s: wave.timer_at(t_s).clamp(0.0, 100.0) as u8,
        eggs,
    }))
}

/// The egg counter nearest `t_s`, read from the HUD
fn read_eggs(video: &Path, region: Option<video::Region>, t_s: f64) -> Result<Option<(u16, u16)>> {
    let region = match region {
        Some(r) => r,
        None => video::Region::full(&video::probe(video)?),
    };
    let start = (t_s - EGGS_AROUND_S).max(0.0);
    let crops = video::CropReader::start(
        video,
        region,
        Some(EGGS_EVERY_S),
        30.0,
        start,
        Some(2.0 * EGGS_AROUND_S),
    )?;
    let mut best: Option<(f64, (u16, u16))> = None;
    for item in crops {
        let (t, crop) = item?;
        if let Some(eggs) = hud::read(&crop).and_then(|h| h.eggs)
            && best.is_none_or(|(d, _)| (t - t_s).abs() < d)
        {
            best = Some(((t - t_s).abs(), eggs));
        }
    }
    Ok(best.map(|(_, eggs)| eggs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Frames at 30 fps from 12 s: idle unless `f` changes them
    fn labels(n: usize, f: impl Fn(usize, &mut Label)) -> Vec<Label> {
        (0..n)
            .map(|i| {
                let mut l = Label {
                    frame: 360 + i as u64,
                    valid: Some(true),
                    buttons: Some(Vec::new()),
                    left_stick: Some([2048.0, 2048.0]),
                    right_stick: Some([2048.0, 2048.0]),
                    gyro_deg: Some([0.0; 3]),
                    extra: serde_json::Map::new(),
                };
                f(i, &mut l);
                l
            })
            .collect()
    }

    fn press(l: &mut Label, b: &str) {
        l.buttons.as_mut().unwrap().push(String::from(b));
    }

    #[test]
    fn summarizes_buttons_rolls_movement_and_turns() {
        // 12.0-13.5 s ZR held; 13.6-14.3 s swimming forward, then back and
        // B at 14.2 s; 14.5-15.5 s turning right by gyro at 45 deg/s; R
        // tapped three times from 15.6 s
        let l = labels(150, |i, l| {
            if i < 45 {
                press(l, "zr");
            }
            if (48..70).contains(&i) {
                press(l, "zl");
                l.left_stick = Some([2048.0, 3600.0]);
            }
            if (64..70).contains(&i) {
                l.left_stick = Some([2048.0, 500.0]);
            }
            if (66..68).contains(&i) {
                press(l, "b");
            }
            if (75..105).contains(&i) {
                l.gyro_deg = Some([0.0, 0.0, -1.5]);
            }
            if [108, 109, 114, 115, 120].contains(&i) {
                press(l, "r");
            }
        });
        let s = summarize_input(&l, 30.0, InputSource::Recorded);
        assert_eq!((s.start_s, s.end_s), (12.0, 17.0));
        assert_eq!(
            s.lines,
            [
                "12.0\u{2013}13.5 s: ZR held (shooting)",
                "13.6\u{2013}14.3 s: ZL held (swimming)",
                "13.6\u{2013}14.1 s: moving forward (left stick)",
                "14.2 s: squid roll (left stick flicked back + B while swimming)",
                "14.5\u{2013}15.5 s: turning right ~44\u{b0}/s (peak 45\u{b0}/s)",
                "15.6\u{2013}16.0 s: R \u{d7}3 (sub weapon)",
            ]
        );
        assert_eq!(
            s.actions,
            [
                "squid roll",
                "shooting",
                "swimming",
                "sub weapon",
                "moving",
                "turning right"
            ]
        );
    }

    #[test]
    fn predictions_turn_by_their_camera_turn() {
        // camera_turn x negative turns right; the stick and gyro are ignored
        let l = labels(30, |_, l| {
            l.extra
                .insert(String::from("camera_turn"), json!([-15.36, 0.0]));
            l.gyro_deg = Some([0.0, 0.0, 5.0]);
        });
        let s = summarize_input(
            &l,
            30.0,
            InputSource::Predicted {
                model: String::from("m"),
            },
        );
        assert_eq!(
            s.lines,
            ["12.0\u{2013}13.0 s: turning right ~60\u{b0}/s (peak 60\u{b0}/s)"]
        );
    }

    #[test]
    fn the_right_stick_turns_too_and_idle_is_said() {
        let l = labels(30, |_, l| l.right_stick = Some([500.0, 2048.0]));
        let s = summarize_input(&l, 30.0, InputSource::Recorded);
        assert_eq!(s.actions, ["turning left"]);
        let idle = summarize_input(&labels(30, |_, _| {}), 30.0, InputSource::Recorded);
        assert_eq!(
            idle.lines,
            ["no buttons, the sticks near the center, the camera still"]
        );
        let none = summarize_input(
            &labels(3, |_, l| l.valid = Some(false)),
            30.0,
            InputSource::Recorded,
        );
        assert_eq!(none.lines, ["no controller input known here"]);
    }

    #[test]
    fn blocks_and_queries() {
        let situation = Situation {
            input: Some(Input {
                source: InputSource::Predicted {
                    model: String::from("v2"),
                },
                start_s: 12.0,
                end_s: 18.0,
                lines: alloc::vec![String::from("14.2 s: squid roll")],
                actions: alloc::vec![String::from("squid roll"), String::from("shooting")],
            }),
            hud: alloc::vec![HudState {
                t_s: 15.0,
                wave: 2,
                extra: false,
                timer_s: 43,
                eggs: Some((18, 24)),
            }],
            objects_t_s: Some(15.0),
            labelled: alloc::vec![
                SeenObject {
                    class: String::from("chum"),
                    x: 0.1,
                    y: 0.5
                },
                SeenObject {
                    class: String::from("steel_eel"),
                    x: 0.9,
                    y: 0.5
                },
                SeenObject {
                    class: String::from("chum"),
                    x: 0.5,
                    y: 0.5
                },
            ],
            detected: Vec::new(),
        };
        assert_eq!(
            situation.block(),
            "<moment>\n\
             HUD at 15.0 s: wave 2, 43 s left (W2 :43), golden eggs 18/24\n\
             Controller input 12.0\u{2013}18.0 s, predicted from the video by the inverse dynamics model v2; an estimate:\n\
             - 14.2 s: squid roll\n\
             Objects a person labelled on the frame at 15.0 s: chum \u{d7}2 (left, center), steel eel (right)\n\
             </moment>"
        );
        assert_eq!(
            situation.query(),
            "W2 :43, wave 2 with 43 seconds left, 18 of 24 golden eggs\n\
             player: squid roll, shooting\n\
             on screen: chum, steel eel"
        );
        assert_eq!(Situation::default().block(), "");
        let extra = HudState {
            t_s: 1.0,
            wave: 4,
            extra: true,
            timer_s: 75,
            eggs: None,
        };
        assert_eq!(extra.describe(), "extra wave (King Salmonid), 75 s left");
        assert_eq!(timer(75), "1:15");
    }

    #[test]
    fn the_hud_needs_a_wave_table() {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-hud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("v.mp4");
        assert_eq!(hud_at(&video, 10.0).unwrap(), None);
        crate::corpus::tests::wave_table(&video, None, &[(2, 100.0, 200.0, 100.0)]);
        // Outside every wave
        assert_eq!(hud_at(&video, 50.0).unwrap(), None);
        // In wave 2 with 43 s left; no video to read the eggs from
        let h = hud_at(&video, 157.5).unwrap().unwrap();
        assert_eq!((h.wave, h.timer_s, h.eggs), (2, 43, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
