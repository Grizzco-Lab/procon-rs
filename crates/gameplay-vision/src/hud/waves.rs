//! Per-sample HUD readings to a wave table, using the timer's physics.
//!
//! Within a wave the timer counts down by one per second, so every correct
//! reading `T` at video time `t` gives the same `t + T` (up to the one
//! second a value stays shown). Readings are grouped by that sum: a wave
//! is a group of at least [`MIN_READINGS`] readings of at least
//! [`MIN_VALUES`] different values whose sums agree within [`JOIN_S`] (a
//! glyph misread the same way for a while stays one value); misread digits
//! land in small groups of their own, and of two groups seen at the same
//! time the smaller is dropped.
//!
//! The wave number is the label's most read digit within the group. In a
//! game it only increases, so a weak vote (under three readings or under
//! 60%) that does not go up, or no reading at all, gives the wave before
//! plus one. A clear vote is kept even when it does not go up: an edited
//! video (a VOD review with cuts) can show one wave in several parts, each
//! with its own countdown fit. An extra wave (`XTRAWAVE`: no digit in the
//! label, no egg counter) is numbered [`EXTRA_WAVE`].
//!
//! The timer shows `T` from the moment it switches to `T` for one second,
//! so the sums of a wave's correct readings fall in one second `[c, c+1)`;
//! `c` is taken from the one-second window holding the most sums (the
//! middle of what those sums allow). The display switches to `T` at video
//! time `c - T`: that is [`WaveTable::to_video_time`].

use super::Hud;
use super::video::Region;
use serde::{Deserialize, Serialize};

/// Largest difference of `t + T` within one wave's readings, in seconds
pub const JOIN_S: f64 = 1.5;
/// Fewest timer readings that make a wave
pub const MIN_READINGS: usize = 4;
/// Wave number given to an extra wave (it follows wave 3)
pub const EXTRA_WAVE: u8 = 4;
/// Fewest different timer values that make a wave
pub const MIN_VALUES: usize = 3;

/// One sampled frame
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Sample {
    /// Video time in seconds
    pub t: f64,
    /// What the HUD showed; `None` when no HUD was found
    pub hud: Option<Hud>,
}

/// One wave found in a video
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Wave {
    /// Wave number (1, 2, 3, ...; an extra wave follows the last one)
    pub wave: u8,
    /// Video time the wave's countdown starts (not before 0), or it is first
    /// seen
    pub start_video_s: f64,
    /// Video time the timer reaches 0, or the wave is last seen
    pub end_video_s: f64,
    /// Seconds on the countdown at `start_video_s` (100 for a whole wave;
    /// fractional: the display shows the next whole number up)
    pub timer_at_start: f64,
    /// Whether `wave` was read from the label, not counted on
    pub wave_read: bool,
    /// An extra wave (`XTRAWAVE`, the King Salmonid): numbered
    /// [`EXTRA_WAVE`]
    #[serde(default)]
    pub extra: bool,
    /// Timer readings from 1 to 99 within the wave
    pub readings: usize,
    /// Share of those readings that agree with the fitted countdown
    pub agree: f64,
}

impl Wave {
    /// Video time the display switches to `timer_s`
    pub fn to_video_time(&self, timer_s: f64) -> f64 {
        self.start_video_s + self.timer_at_start - timer_s
    }

    /// Timer value shown at video time `t` (unclamped), the inverse of
    /// [`Wave::to_video_time`]
    pub fn timer_at(&self, t: f64) -> f64 {
        (self.start_video_s + self.timer_at_start - t).ceil()
    }
}

/// The waves of one video: the `wave_starts.json` file
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WaveTable {
    /// Video file name
    pub video: String,
    /// Video length in seconds
    pub duration_s: f64,
    /// Seconds between samples
    pub every_s: f64,
    /// The game picture in the video's pixels
    pub region: Option<Region>,
    /// Samples read
    pub samples: usize,
    /// Samples with a HUD
    pub hud_samples: usize,
    /// Waves in video order
    pub waves: Vec<Wave>,
}

impl WaveTable {
    /// Video time at which wave `wave`'s timer switches to `timer_s`, such
    /// as `to_video_time(2, 83.0)` for a comment "W2 83s"; `None` when the
    /// wave is not in the video or that moment is outside the parts seen.
    /// An edited video may show a wave in several parts: the first part
    /// holding the moment answers.
    pub fn to_video_time(&self, wave: u8, timer_s: f64) -> Option<f64> {
        self.waves
            .iter()
            .filter(|w| w.wave == wave)
            .map(|w| (w, w.to_video_time(timer_s)))
            .find(|(w, t)| *t >= w.start_video_s - 1.0 && *t <= w.end_video_s + 1.0)
            .map(|(_, t)| t)
    }

    /// Wave and timer value at video time `t`, from the fitted countdown;
    /// `None` outside every wave
    pub fn at(&self, t: f64) -> Option<(u8, u8)> {
        let w = self
            .waves
            .iter()
            .find(|w| t >= w.start_video_s && t <= w.end_video_s)?;
        Some((w.wave, w.timer_at(t).clamp(0.0, 100.0) as u8))
    }
}

/// One group of readings with the same `t + T`
struct Group {
    /// `(t, t + T)` in time order
    points: Vec<(f64, f64)>,
}

impl Group {
    fn median_sum(&self) -> f64 {
        let mut z: Vec<f64> = self.points.iter().map(|p| p.1).collect();
        z.sort_by(f64::total_cmp);
        z[z.len() / 2]
    }

    fn span(&self) -> (f64, f64) {
        (self.points[0].0, self.points[self.points.len() - 1].0)
    }
}

/// The waves in `samples` (in time order), sampled every `every_s` seconds
pub fn find_waves(samples: &[Sample], every_s: f64) -> Vec<Wave> {
    // Group readings by t + T; 100 and 0 are left out (the timer rests on
    // them before and after the countdown)
    let mut groups: Vec<Group> = Vec::new();
    for s in samples {
        let Some(timer) = s.hud.and_then(|h| h.timer_s) else {
            continue;
        };
        if !(1..=99).contains(&timer) {
            continue;
        }
        let p = (s.t, s.t + f64::from(timer));
        match groups
            .iter_mut()
            .rev()
            .find(|g| (g.median_sum() - p.1).abs() <= JOIN_S)
        {
            Some(g) => g.points.push(p),
            None => groups.push(Group { points: vec![p] }),
        }
    }
    groups.retain(|g| {
        let mut values: Vec<i64> = g
            .points
            .iter()
            .map(|p| (p.1 - p.0).round() as i64)
            .collect();
        values.sort_unstable();
        values.dedup();
        g.points.len() >= MIN_READINGS && values.len() >= MIN_VALUES
    });
    // Two countdowns cannot run at once: keep the larger group
    groups.sort_by_key(|g| core::cmp::Reverse(g.points.len()));
    let mut kept: Vec<Group> = Vec::new();
    for g in groups {
        let (a0, a1) = g.span();
        let overlaps = kept.iter().any(|k| {
            let (b0, b1) = k.span();
            a0 <= b1 && b0 <= a1
        });
        if !overlaps {
            kept.push(g);
        }
    }
    kept.sort_by(|a, b| a.span().0.total_cmp(&b.span().0));

    let mut waves = Vec::new();
    let mut last_wave = 0u8;
    for g in kept {
        let (lo, n_in) = densest_second(&g.points);
        let inliers: Vec<(f64, f64)> = g
            .points
            .iter()
            .copied()
            .filter(|p| p.1 >= lo && p.1 < lo + 1.0)
            .collect();
        let zmin = inliers.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        let zmax = inliers
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max);
        let c = (zmin + zmax - 1.0) / 2.0;
        let first = inliers.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
        let last = inliers
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max);
        // Seen from its first second, or to its last: the whole countdown
        let start = if first - (c - 100.0) < 1.0 + every_s {
            (c - 100.0).max(0.0)
        } else {
            first
        };
        let end = if c - last < 1.0 + every_s { c } else { last };
        let readings = samples
            .iter()
            .filter(|s| s.t >= start && s.t <= end)
            .filter(|s| {
                s.hud
                    .and_then(|h| h.timer_s)
                    .is_some_and(|v| (1..=99).contains(&v))
            })
            .count();
        // The label's most read digit
        let mut votes = [0usize; 256];
        for s in samples
            .iter()
            .filter(|s| s.t >= start - 0.5 && s.t <= end + 0.5)
        {
            if let Some(w) = s.hud.and_then(|h| h.wave) {
                votes[usize::from(w)] += 1;
            }
        }
        let (voted, count) = votes
            .iter()
            .enumerate()
            .max_by_key(|(_, n)| **n)
            .map(|(w, n)| (w as u8, *n))
            .unwrap_or_default();
        // A clear vote is trusted even when it does not go up (a cut in an
        // edited video); a weak one only when it does
        let labeled: usize = votes.iter().sum();
        let clear = count >= 3 && count * 5 >= labeled * 3;
        // An extra wave's label has no digit and it has no egg counter
        let seen: Vec<&Hud> = samples
            .iter()
            .filter(|s| s.t >= start && s.t <= end)
            .filter_map(|s| s.hud.as_ref())
            .collect();
        let with_eggs = seen.iter().filter(|h| h.eggs.is_some()).count();
        let extra =
            seen.len() >= MIN_READINGS && labeled * 10 < seen.len() && with_eggs * 10 < seen.len();
        let wave_read = !extra && count > 0 && (clear || voted > last_wave);
        let wave = if extra {
            EXTRA_WAVE
        } else if wave_read {
            voted
        } else {
            last_wave.saturating_add(1)
        };
        last_wave = wave;
        waves.push(Wave {
            wave,
            start_video_s: round2(start),
            end_video_s: round2(end),
            timer_at_start: round2(c - start),
            wave_read,
            extra,
            readings,
            agree: round2(n_in as f64 / readings.max(1) as f64),
        });
    }
    waves
}

/// The one-second window `[lo, lo + 1)` holding the most sums: its lower
/// edge and how many it holds
fn densest_second(points: &[(f64, f64)]) -> (f64, usize) {
    let mut z: Vec<f64> = points.iter().map(|p| p.1).collect();
    z.sort_by(f64::total_cmp);
    let mut best = (z[0], 0);
    let mut j = 0;
    for i in 0..z.len() {
        j = j.max(i);
        while j < z.len() && z[j] < z[i] + 1.0 {
            j += 1;
        }
        if j - i > best.1 {
            best = (z[i], j - i);
        }
    }
    best
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hud(wave: Option<u8>, timer: u8) -> Option<Hud> {
        Some(Hud {
            wave,
            timer_s: Some(timer),
            eggs: Some((0, 20)),
            confidence: 1.0,
        })
    }

    /// Samples every `every` s from `from` to `to`; a wave whose display
    /// switches to 99 at `s99` (so 100 before it) and to 0 at `s99 + 99`
    fn countdown(wave: u8, s99: f64, from: f64, to: f64, every: f64) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut t = from;
        while t < to {
            let left = (s99 + 99.0 - t).ceil().clamp(0.0, 100.0) as u8;
            // The display switches at s99 + k: the value shown at t
            let shown = if t < s99 { 100 } else { left.min(99) };
            out.push(Sample {
                t,
                hud: hud(Some(wave), shown),
            });
            t += every;
        }
        out
    }

    #[test]
    fn two_whole_waves_are_found_with_their_starts() {
        let mut s = countdown(1, 20.3, 10.0, 125.0, 0.5);
        s.extend((250..260).map(|i| Sample {
            t: f64::from(i) * 0.5,
            hud: None,
        }));
        s.extend(countdown(2, 140.8, 130.0, 245.0, 0.5));
        let waves = find_waves(&s, 0.5);
        assert_eq!(waves.len(), 2);
        assert_eq!(waves[0].wave, 1);
        assert!(waves[0].wave_read);
        // The countdown starts a second before 99 shows
        assert!(
            (waves[0].start_video_s - 19.3).abs() <= 0.26,
            "{:?}",
            waves[0]
        );
        assert!((waves[0].timer_at_start - 100.0).abs() < 1e-9);
        assert!((waves[0].end_video_s - 119.3).abs() <= 0.26);
        assert!(
            (waves[1].start_video_s - 139.8).abs() <= 0.26,
            "{:?}",
            waves[1]
        );
        assert!(waves[0].agree > 0.99);
    }

    #[test]
    fn to_video_time_inverts_the_countdown() {
        let s = countdown(2, 40.0, 30.0, 140.0, 0.5);
        let table = WaveTable {
            video: "v.mp4".into(),
            duration_s: 140.0,
            every_s: 0.5,
            region: None,
            samples: s.len(),
            hud_samples: s.len(),
            waves: find_waves(&s, 0.5),
        };
        // 83 shows from 40 + 16 = 56 s
        let t = table.to_video_time(2, 83.0).unwrap();
        assert!((t - 56.0).abs() <= 0.26, "{t}");
        assert_eq!(table.at(t + 0.3), Some((2, 83)));
        assert_eq!(table.to_video_time(1, 83.0), None);
        assert_eq!(table.to_video_time(2, -30.0), None);
    }

    #[test]
    fn misreads_and_a_clip_starting_mid_wave() {
        // A clip from timer 60.x of wave 3 to its end
        let mut s = countdown(3, 0.0 - 39.0, 0.0, 70.0, 1.0);
        // A 3 read as 8 for a few seconds, and a label misread once
        for x in s.iter_mut().filter(|x| (20.0..24.0).contains(&x.t)) {
            let h = x.hud.as_mut().unwrap();
            h.timer_s = Some(h.timer_s.unwrap() + 5);
        }
        s[5].hud.as_mut().unwrap().wave = Some(8);
        let waves = find_waves(&s, 1.0);
        assert_eq!(waves.len(), 1, "{waves:?}");
        let w = &waves[0];
        assert_eq!((w.wave, w.wave_read), (3, true));
        assert_eq!(w.start_video_s, 0.0);
        // 60 left at t=0 (99 at -39, 0 at 60)
        assert!((w.timer_at_start - 60.0).abs() <= 0.5, "{w:?}");
        assert!((w.end_video_s - 60.0).abs() <= 0.5);
        assert!(w.agree > 0.9 && w.agree < 1.0);
    }

    #[test]
    fn unread_or_backward_labels_count_on() {
        let mut s = countdown(1, 5.0, 0.0, 110.0, 1.0);
        let mut second = countdown(1, 130.0, 125.0, 235.0, 1.0);
        for x in &mut second {
            x.hud.as_mut().unwrap().wave = None;
        }
        s.extend(second);
        let waves = find_waves(&s, 1.0);
        assert_eq!(waves.iter().map(|w| w.wave).collect::<Vec<_>>(), [1, 2]);
        assert!(waves[0].wave_read && !waves[1].wave_read);
    }

    #[test]
    fn an_extra_wave_has_no_label_digit_and_no_egg_counter() {
        let mut s = countdown(3, 5.0, 0.0, 110.0, 1.0);
        let mut extra = countdown(1, 130.0, 125.0, 160.0, 1.0);
        for x in &mut extra {
            let h = x.hud.as_mut().unwrap();
            (h.wave, h.eggs) = (None, None);
        }
        s.extend(extra);
        let waves = find_waves(&s, 1.0);
        assert_eq!(
            waves.iter().map(|w| w.wave).collect::<Vec<_>>(),
            [3, EXTRA_WAVE]
        );
        assert!(!waves[0].extra && waves[1].extra && !waves[1].wave_read);
    }

    #[test]
    fn a_cut_in_an_edited_video_keeps_the_wave_number() {
        // Wave 2 from 90 left, then 3 s of it cut out at 20 s of video
        let mut s = countdown(2, -9.0, 0.0, 20.0, 0.5);
        s.extend(countdown(2, -12.0, 20.0, 40.0, 0.5));
        let table = WaveTable {
            video: "v.mp4".into(),
            duration_s: 40.0,
            every_s: 0.5,
            region: None,
            samples: s.len(),
            hud_samples: s.len(),
            waves: find_waves(&s, 0.5),
        };
        assert_eq!(table.waves.len(), 2, "{:?}", table.waves);
        assert!(table.waves.iter().all(|w| w.wave == 2 && w.wave_read));
        // 80 shows at 10 s (before the cut), 60 at 27 s (after it)
        assert!((table.to_video_time(2, 80.0).unwrap() - 10.0).abs() <= 0.26);
        assert!((table.to_video_time(2, 60.0).unwrap() - 27.0).abs() <= 0.26);
        // Cut out: 69 would show at 21 s in the first part, 18 s in the second
        assert_eq!(table.to_video_time(2, 69.0), None);
    }
}
