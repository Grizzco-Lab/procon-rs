//! What AgentZero may press when it plays the Switch ([`Limits`]), held to
//! in the last step before the proxy ([`Limiter`], in every line
//! [`super::Bot`] writes to the replay port), and a person's fastest
//! tapping, measured from the controller's reports ([`Tapping`]), which the
//! cap can be set from
//!
//! **Masks.** Buttons the bot never presses, whatever the policy says: the
//! d-pad (Splatoon's signals, "This way!" and "Booyah!", which disturb
//! teammates) and the special (the right stick's click) while they are
//! blocked on the page, which they are by default, and Home and Capture
//! always (Home would suspend the game; neither is play). They hold back the
//! bot's presses only: a person's own reach the Switch, since the proxy adds
//! the controller's buttons to the bot's (see [`procon_core::replay`]).
//!
//! **The press-rate cap.** Pressing a button faster than a person can is what
//! a turbo or a macro does, and what cheat detection looks for; rapid fire on
//! a semi-automatic weapon (the L-3 or H-3 Nozzlenose) is the classic case.
//! So no button of the bot's is pressed more than [`Limits::max_hz`] times a
//! second: a press starts at least `1 / max_hz` after the one before (a
//! press that comes sooner waits, the button up, until an action after
//! that), and lasts at least [`Limits::min_hold_ms`] (a release that comes
//! sooner waits, the button down). Masks and cap apply to what is written,
//! after everything else, so no model or setting gets past them; only the
//! stop's neutral line lets go at once. The proxy applies a line to its next
//! report (every 8 to 16 ms), so one gap at the Switch can come out up to a
//! report shorter; the rate over several presses holds.
//!
//! The default cap is [`HEADROOM`] times [`HUMAN_MAX_HZ`], how fast a person
//! presses a button:
//!
//! - ordinary adults tapping a key with the index finger as fast as they can
//!   for 10 s: 164 ms between taps on average (6.1 a second), about 150 ms
//!   (6.7 a second) at their fastest, in the first two seconds, slowing from
//!   the fourth or fifth (Barut, Kızıltan, Gelir and Köktürk, "Advanced
//!   Analysis of Finger-Tapping Performance: A Preliminary Study", Balkan
//!   Medical Journal 30(2):167-171, 2013; 35 right-handed young men). Over
//!   three minutes, 5.7 to 6.0 a second at first falls by 40%
//!   (Madinabeitia-Mancebo et al., "Temporal dynamics of muscle, spinal and
//!   cortical excitability and their association with kinematics during
//!   three minutes of maximal-rate finger tapping", Scientific Reports
//!   10:3166, 2020). The index is the fastest finger (Aoki, Francis and
//!   Kinoshita, "Differences in the abilities of individual fingers during
//!   the performance of fast, repetitive tapping movements", Experimental
//!   Brain Research 152:270-280, 2003);
//! - records, with the whole arm shaking from the elbow: Takahashi Meijin's
//!   16 presses a second on a Famicom pad in 1985 (17 when filmed and
//!   counted; 12 to 13 a second decades later),
//!   en.wikipedia.org/wiki/Takahashi_Meijin and his interview in Friday
//!   Digital, en.friday.news/article/8747;
//! - the owner's own play, 111 minutes of Salmon Run in the 33 sessions of
//!   2026-09-25/27: ZR pressed 3203 times, 5% of them within 128 ms of the
//!   one before, the fastest six in a row at 8.9 a second (8.3 at the 99th
//!   percentile of such runs), A, B, R and the d-pad about as fast, and the
//!   shortest presses (1st percentile) 40 ms long.
//!
//! A cap holds for as long as the bot plays, so its reference is the rate a
//! person keeps up beyond a moment rather than a burst: 7 a second, a little
//! over ordinary people's best and under the owner's bursts. It is ZR's
//! figure, taken for every button: ZR is the button a turbo repeats and is
//! pressed with the fastest finger, and the owner presses none faster. With
//! the headroom that is 7.7 a second, presses 130 ms apart; the policy acts
//! 30 times a second, so it presses a button on every fourth action at most
//! (7.5 a second). The page's measurement (a person tapping ZR as fast as
//! they can for [`TAP_WINDOW_MS`]) sets the cap to the headroom times their
//! own fastest instead.

use alloc::collections::BTreeMap;
use anyhow::{Result, ensure};
use core::time::Duration;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// How much faster than a person the bot may press
pub const HEADROOM: f64 = 1.1;

/// Presses a second a person keeps up at most, beyond a moment (see the
/// module docs)
pub const HUMAN_MAX_HZ: f64 = 7.0;

/// The shortest press, in ms: the owner's shortest presses (the 1st
/// percentile of theirs, see the module docs)
pub const DEFAULT_MIN_HOLD_MS: f64 = 40.0;

/// The cap may be set between these, presses a second (the policy acts 30
/// times a second, so 15 is all it could press)
pub const CAP_RANGE_HZ: (f64, f64) = (2.0, 15.0);

/// The shortest press may be set between these, in ms
pub const HOLD_RANGE_MS: (f64, f64) = (20.0, 250.0);

/// The d-pad: Splatoon's signals
const DPAD: [&str; 4] = ["up", "down", "left", "right"];

/// The special: the right stick's click
const SPECIAL: &str = "r_stick";

/// Never pressed by the bot: Home would suspend the game
const NEVER: [&str; 2] = ["home", "capture"];

/// What the bot may press; set on the page, kept in the lab's state file
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Never press the d-pad
    pub block_dpad: bool,
    /// Never press the special (the right stick's click)
    pub block_special: bool,
    /// Presses a second each button may make at most
    pub max_hz: f64,
    /// Shortest press, in ms
    pub min_hold_ms: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            block_dpad: true,
            block_special: true,
            max_hz: round_tenth(HEADROOM * HUMAN_MAX_HZ),
            min_hold_ms: DEFAULT_MIN_HOLD_MS,
        }
    }
}

/// `value` to one decimal
fn round_tenth(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

impl Limits {
    /// Check the cap and the shortest press are in their ranges
    pub fn validate(&self) -> Result<()> {
        let (low, high) = CAP_RANGE_HZ;
        ensure!(
            (low..=high).contains(&self.max_hz),
            "the cap goes from {low} to {high} presses a second, not {}",
            self.max_hz
        );
        let (low, high) = HOLD_RANGE_MS;
        ensure!(
            (low..=high).contains(&self.min_hold_ms),
            "the shortest press goes from {low} to {high} ms, not {}",
            self.min_hold_ms
        );
        Ok(())
    }

    /// Whether the bot may press `button` at all
    pub fn allows(&self, button: &str) -> bool {
        !(NEVER.contains(&button)
            || self.block_dpad && DPAD.contains(&button)
            || self.block_special && button == SPECIAL)
    }

    /// Shortest time from one press of a button to the next
    fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.max_hz)
    }

    fn min_hold(&self) -> Duration {
        Duration::from_secs_f64(self.min_hold_ms / 1000.0)
    }
}

/// One button as the bot has sent it
#[derive(Clone, Copy, Debug, Default)]
struct Press {
    /// When it went down, while it is down
    since: Option<Instant>,
    /// When its last press started
    started: Option<Instant>,
}

/// The cap's state: each button as sent lately (see the module docs)
#[derive(Debug, Default)]
pub struct Limiter {
    buttons: BTreeMap<String, Press>,
}

impl Limiter {
    /// The buttons to send at `now` when the policy wants `wanted` pressed:
    /// the masked ones left out, presses and releases held to the cap
    pub fn buttons(&mut self, wanted: &[String], limits: &Limits, now: Instant) -> Vec<String> {
        for name in wanted {
            if limits.allows(name) {
                self.buttons.entry(name.clone()).or_default();
            }
        }
        let mut pressed = Vec::new();
        for (name, press) in &mut self.buttons {
            let want = limits.allows(name) && wanted.contains(name);
            let down = match press.since {
                // A press waits until the last one is far enough behind
                None => {
                    let due = press
                        .started
                        .is_none_or(|at| now.duration_since(at) >= limits.interval());
                    if want && due {
                        press.since = Some(now);
                        press.started = Some(now);
                    }
                    want && due
                }
                // A release waits until the press is long enough
                Some(since) => want || now.duration_since(since) < limits.min_hold(),
            };
            if down {
                pressed.push(name.clone());
            } else {
                press.since = None;
            }
        }
        pressed
    }
}

/// How long a measurement of a person's tapping counts presses, from their
/// first press, in ms
pub const TAP_WINDOW_MS: u64 = 10_000;

/// A measurement without a press ends after this
pub const TAP_WAIT: Duration = Duration::from_secs(30);

/// A measurement ends this long after its window on this machine's clock
/// too, should the reports stop coming
const TAP_GRACE: Duration = Duration::from_secs(1);

/// The fastest rate is taken over this many presses in a row
const FASTEST_OVER: usize = 6;

/// A person tapping a button as fast as they can, measured from the
/// controller's reports: from their first press, for [`TAP_WINDOW_MS`]
#[derive(Debug)]
pub struct Tapping {
    button: String,
    asked: Instant,
    /// When the first press came, on this machine's clock
    began: Option<Instant>,
    /// Start of each press, ms on the proxy's clock
    starts: Vec<u64>,
    /// Length of each press that ended, ms
    holds: Vec<u64>,
    /// The press under way
    down_since: Option<u64>,
    /// The newest report's time
    last_ms: u64,
    done: bool,
    cancelled: bool,
}

/// Where a measurement is, for the page
#[derive(Clone, Debug, Serialize)]
pub struct TappingStatus {
    pub button: String,
    /// `waiting` for the first press, `counting`, `done`, `none` (no press
    /// came, or one alone) or `cancelled`
    pub state: &'static str,
    pub presses: usize,
    /// Seconds of the window left, while counting
    pub left_s: Option<f64>,
    pub result: Option<TapResult>,
}

/// A measurement's result
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct TapResult {
    pub presses: usize,
    /// The fastest [`FASTEST_OVER`] presses in a row (all of them when
    /// fewer), per second
    pub fastest_hz: f64,
    /// From the first press to the last, per second
    pub average_hz: f64,
    /// The shortest and the median press, ms
    pub shortest_hold_ms: Option<u64>,
    pub median_hold_ms: Option<u64>,
    /// The cap it suggests: [`HEADROOM`] times the fastest, within
    /// [`CAP_RANGE_HZ`]
    pub cap_hz: f64,
}

impl Tapping {
    /// A measurement of `button`, waiting for the first press
    pub fn new(button: &str) -> Self {
        Self {
            button: button.to_string(),
            asked: Instant::now(),
            began: None,
            starts: Vec::new(),
            holds: Vec::new(),
            down_since: None,
            last_ms: 0,
            done: false,
            cancelled: false,
        }
    }

    /// The button measured
    pub fn button(&self) -> &str {
        &self.button
    }

    /// Waiting for the first press or counting presses
    pub fn active(&self) -> bool {
        !self.over() && !self.cancelled && !self.timed_out()
    }

    /// The window has passed (by the reports, or by this clock if they
    /// stopped)
    fn over(&self) -> bool {
        let window = Duration::from_millis(TAP_WINDOW_MS) + TAP_GRACE;
        self.done || self.began.is_some_and(|at| at.elapsed() > window)
    }

    /// No press came
    fn timed_out(&self) -> bool {
        self.began.is_none() && self.asked.elapsed() > TAP_WAIT
    }

    pub fn cancel(&mut self) {
        if self.active() {
            self.cancelled = true;
        }
    }

    /// A report the proxy read at `at_ms` (its clock), with the button
    /// `pressed` or not
    pub fn report(&mut self, at_ms: u64, pressed: bool) {
        if !self.active() {
            return;
        }
        self.last_ms = at_ms;
        if let Some(&first) = self.starts.first()
            && at_ms >= first + TAP_WINDOW_MS
        {
            self.done = true;
            return;
        }
        match (self.down_since, pressed) {
            (None, true) => {
                self.began.get_or_insert_with(Instant::now);
                self.starts.push(at_ms);
                self.down_since = Some(at_ms);
            }
            (Some(since), false) => {
                self.holds.push(at_ms - since);
                self.down_since = None;
            }
            _ => {}
        }
    }

    /// The presses so far as a result, when there are two or more
    pub fn result(&self) -> Option<TapResult> {
        let starts = &self.starts;
        if starts.len() < 2 {
            return None;
        }
        let rate = |first: u64, last: u64, gaps: usize| {
            if last > first {
                gaps as f64 * 1000.0 / (last - first) as f64
            } else {
                0.0
            }
        };
        let run = FASTEST_OVER.min(starts.len());
        let fastest_hz = starts
            .windows(run)
            .map(|w| rate(w[0], w[run - 1], run - 1))
            .fold(0.0, f64::max);
        let average_hz = rate(starts[0], starts[starts.len() - 1], starts.len() - 1);
        let mut holds = self.holds.clone();
        holds.sort_unstable();
        let (low, high) = CAP_RANGE_HZ;
        Some(TapResult {
            presses: starts.len(),
            fastest_hz: round_tenth(fastest_hz),
            average_hz: round_tenth(average_hz),
            shortest_hold_ms: holds.first().copied(),
            median_hold_ms: holds.get(holds.len() / 2).copied(),
            cap_hz: round_tenth(HEADROOM * fastest_hz).clamp(low, high),
        })
    }

    pub fn status(&self) -> TappingStatus {
        let result = self.result();
        let state = if self.cancelled {
            "cancelled"
        } else if self.over() {
            if result.is_some() { "done" } else { "none" }
        } else if self.timed_out() {
            "none"
        } else if self.starts.is_empty() {
            "waiting"
        } else {
            "counting"
        };
        let left_s = (state == "counting").then(|| {
            let end = self.starts[0] + TAP_WINDOW_MS;
            end.saturating_sub(self.last_ms) as f64 / 1000.0
        });
        TappingStatus {
            button: self.button.clone(),
            state,
            presses: self.starts.len(),
            left_s,
            result: if state == "done" { result } else { None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn masks_hold_back_the_dpad_the_special_home_and_capture() {
        let limits = Limits::default();
        let mut limiter = Limiter::default();
        let now = Instant::now();
        let wanted = names(&["zr", "up", "down", "r_stick", "home", "capture", "b"]);
        assert_eq!(limiter.buttons(&wanted, &limits, now), names(&["b", "zr"]));
        // Unblocked on the page: the d-pad and the special go through, Home
        // and Capture never
        let open = Limits {
            block_dpad: false,
            block_special: false,
            ..limits
        };
        let mut limiter = Limiter::default();
        assert_eq!(
            limiter.buttons(&wanted, &open, now),
            names(&["b", "down", "r_stick", "up", "zr"])
        );
        // Blocked again while held: let go once held long enough
        let later = now + Duration::from_millis(50);
        assert_eq!(
            limiter.buttons(&wanted, &limits, later),
            names(&["b", "zr"])
        );
    }

    /// The buttons a spamming policy gets through, action by action at 30
    /// a second: (ms, pressed)
    fn spam(limits: &Limits, wanted: impl Fn(usize) -> bool, actions: usize) -> Vec<(f64, bool)> {
        let mut limiter = Limiter::default();
        let start = Instant::now();
        (0..actions)
            .map(|k| {
                let ms = k as f64 * 1000.0 / 30.0;
                let now = start + Duration::from_secs_f64(ms / 1000.0);
                let want = if wanted(k) {
                    names(&["zr"])
                } else {
                    Vec::new()
                };
                let sent = limiter.buttons(&want, limits, now);
                (ms, sent.contains(&"zr".to_string()))
            })
            .collect()
    }

    /// Starts and lengths of the presses in `sent`, in ms
    fn presses(sent: &[(f64, bool)]) -> (Vec<f64>, Vec<f64>) {
        let (mut starts, mut holds) = (Vec::new(), Vec::new());
        let mut down: Option<f64> = None;
        for &(ms, pressed) in sent {
            match (down, pressed) {
                (None, true) => {
                    starts.push(ms);
                    down = Some(ms);
                }
                (Some(since), false) => {
                    holds.push(ms - since);
                    down = None;
                }
                _ => {}
            }
        }
        (starts, holds)
    }

    #[test]
    fn a_spamming_policy_is_held_to_the_cap() {
        let limits = Limits::default();
        assert_eq!(limits.max_hz, 7.7);
        let gap = 1000.0 / limits.max_hz;
        // ZR on every other action: 15 presses a second, one action each
        let sent = spam(&limits, |k| k % 2 == 0, 300);
        let (starts, holds) = presses(&sent);
        assert!(starts.windows(2).all(|w| w[1] - w[0] >= gap - 1e-6));
        assert!(holds.iter().all(|&h| h >= limits.min_hold_ms));
        // Ten seconds of it: at most 7.5 presses a second, every fourth action
        assert!(starts.len() <= 76, "{} presses", starts.len());
        assert!(starts.len() >= 50, "{} presses", starts.len());
        // ZR wanted on every action but one in three: the same
        let sent = spam(&limits, |k| k % 3 != 0, 300);
        let (starts, _) = presses(&sent);
        assert!(starts.windows(2).all(|w| w[1] - w[0] >= gap - 1e-6));
        // ZR for two actions, off for one (10 a second): a higher cap lets
        // more through, never past it
        let pattern = |k: usize| k % 3 != 2;
        let (starts, _) = presses(&spam(&limits, pattern, 300));
        assert!(starts.len() <= 76, "{} presses", starts.len());
        let fast = Limits {
            max_hz: 11.0,
            ..limits
        };
        let (starts, _) = presses(&spam(&fast, pattern, 300));
        assert!(
            starts
                .windows(2)
                .all(|w| w[1] - w[0] >= 1000.0 / 11.0 - 1e-6)
        );
        assert_eq!(starts.len(), 100);
    }

    #[test]
    fn presses_a_person_could_make_go_through_unchanged() {
        let limits = Limits::default();
        // A press of three actions every six: 5 a second, 100 ms each
        let wanted = |k: usize| k % 6 < 3;
        let sent = spam(&limits, wanted, 120);
        for (k, &(_, pressed)) in sent.iter().enumerate() {
            assert_eq!(pressed, wanted(k), "action {k}");
        }
        // A long hold, and nothing at all
        let held = spam(&limits, |_| true, 60);
        assert!(held.iter().all(|&(_, pressed)| pressed));
        assert!(spam(&limits, |_| false, 60).iter().all(|&(_, p)| !p));
    }

    #[test]
    fn a_short_press_is_held_to_the_shortest() {
        let limits = Limits::default();
        let mut limiter = Limiter::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let zr = names(&["zr"]);
        assert_eq!(limiter.buttons(&zr, &limits, at(0)), zr);
        // Let go after 20 ms: still down, then up once 40 ms have passed
        assert_eq!(limiter.buttons(&[], &limits, at(20)), zr);
        assert!(limiter.buttons(&[], &limits, at(40)).is_empty());
        // Wanted again 60 ms after the last press started: waits
        assert!(limiter.buttons(&zr, &limits, at(60)).is_empty());
        assert!(limiter.buttons(&zr, &limits, at(129)).is_empty());
        assert_eq!(limiter.buttons(&zr, &limits, at(130)), zr);
    }

    #[test]
    fn limits_are_checked() {
        assert!(Limits::default().validate().is_ok());
        for bad in [
            Limits {
                max_hz: 0.5,
                ..Default::default()
            },
            Limits {
                max_hz: 30.0,
                ..Default::default()
            },
            Limits {
                min_hold_ms: 5.0,
                ..Default::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        // Settings of before lack fields: the defaults fill them in
        let saved: Limits = serde_json::from_str(r#"{"block_dpad": false}"#).unwrap();
        assert_eq!(
            saved,
            Limits {
                block_dpad: false,
                ..Default::default()
            }
        );
    }

    /// Reports every 16 ms (the proxy's pace with the owner's controller),
    /// from `from` until `until` ms, of a person pressing every `period` ms
    /// for `hold` ms from `origin` ms on
    fn tap(tapping: &mut Tapping, origin: u64, (from, until): (u64, u64), period: u64, hold: u64) {
        let mut at = from;
        while at < until {
            let pressed = at >= origin && (at - origin) % period < hold;
            tapping.report(at, pressed);
            at += 16;
        }
    }

    #[test]
    fn a_persons_tapping_is_measured() {
        let mut tapping = Tapping::new("zr");
        assert_eq!(tapping.status().state, "waiting");
        // Nothing pressed yet: the window has not started
        tap(&mut tapping, 5_008, (1_008, 5_008), 128, 48);
        assert_eq!(tapping.status().state, "waiting");
        // 7.8 presses a second (every 128 ms), 48 ms each
        tap(&mut tapping, 5_008, (5_008, 9_008), 128, 48);
        let status = tapping.status();
        assert_eq!(status.state, "counting");
        assert!(status.left_s.is_some_and(|s| (5.0..7.0).contains(&s)));
        tap(&mut tapping, 5_008, (9_008, 16_000), 128, 48);
        let status = tapping.status();
        assert_eq!(status.state, "done");
        let result = status.result.unwrap();
        // 10 s from the first press: the 79 presses that started in it
        assert_eq!(result.presses, 79, "{result:?}");
        assert_eq!(result.fastest_hz, 7.8);
        assert_eq!(result.average_hz, 7.8);
        assert_eq!(result.shortest_hold_ms, Some(48));
        assert_eq!(result.median_hold_ms, Some(48));
        assert_eq!(result.cap_hz, 8.6);
        // Over: later presses do not count
        tap(&mut tapping, 16_000, (16_000, 20_000), 64, 32);
        assert_eq!(tapping.status().result, Some(result));
    }

    #[test]
    fn a_burst_is_the_fastest() {
        let mut tapping = Tapping::new("zr");
        // Six presses about 100 ms apart, then slower ones
        tap(&mut tapping, 0, (0, 590), 100, 48);
        tap(&mut tapping, 600, (590, 3_000), 250, 48);
        let result = tapping.result().unwrap();
        assert!((9.5..=10.5).contains(&result.fastest_hz), "{result:?}");
        assert!(result.average_hz < 6.0, "{result:?}");
        assert!(tapping.active());
        tapping.cancel();
        assert_eq!(tapping.status().state, "cancelled");
        assert!(tapping.status().result.is_none());
        // A single press is no measurement
        let mut once = Tapping::new("zr");
        tap(&mut once, 0, (0, 11_000), 100_000, 50);
        assert_eq!(once.status().state, "none");
    }
}
