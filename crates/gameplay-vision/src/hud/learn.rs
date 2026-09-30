//! Templates from real frames, labeled by the countdown itself.
//!
//! A video's frames are labeled with the timer value they must show, then
//! each glyph is added to the mean of its character. Two ways to know the
//! value:
//!
//! - [`Anchor::Start`], needing no templates: the timer is the only
//!   three-digit number (`100`), so the frame where its digit count drops
//!   from three to two is the switch to 99; from there it shows
//!   `ceil(99 - (t - t99))`. The wave numbers count from `first_wave`.
//!   For recordings that hold whole waves, such as our own sessions.
//! - [`Anchor::Fit`]: the wave table found with the current templates
//!   ([`super::waves`]) gives the value at every time, so a video in a
//!   different font (Splatoon 2) can be learned when enough of its digits
//!   are read right to fit the countdown.
//!
//! Frames within two frames of a switch are skipped. The egg counter's `/`
//! is the third glyph from the right when the counter has four or more.

use super::glyph::{Cells, GH, GW, Role, Template, Templates};
use super::video::{CropReader, Region, VideoInfo};
use super::waves::{Sample, WaveTable, find_waves};
use super::{Parts, Reader};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

/// How frames get their timer values
#[derive(Clone, Copy, Debug)]
pub enum Anchor {
    /// From the switch from 100 to 99; the first wave found is `first_wave`
    Start {
        /// Number of the first wave in the video
        first_wave: u8,
    },
    /// From the wave table read with the current templates
    Fit,
}

/// Where a wave's countdown is in a video: the display switches to 99 at
/// `t99` and reaches 0 at `t99 + 99`
#[derive(Clone, Copy, Debug)]
pub struct Countdown {
    /// Wave number
    pub wave: u8,
    /// Video time the display switches to 99
    pub t99: f64,
    /// Video time the digit count drops from two to one (10 to 9), if
    /// seen; `t99 + 90` when the frames are exact
    pub t9: Option<f64>,
}

impl Countdown {
    /// Timer value at `t`, or `None` outside the countdown or within
    /// `margin` seconds of a switch
    pub fn value(&self, t: f64, margin: f64) -> Option<u8> {
        let since = t - self.t99;
        if !(0.0..99.0).contains(&since) {
            return None;
        }
        let frac = since - since.floor();
        if frac < margin || frac > 1.0 - margin {
            return None;
        }
        Some(99 - since.floor() as u8)
    }
}

/// Glyph sums per character
#[derive(Default)]
pub struct Learner {
    sums: BTreeMap<(Role, char), (Box<Cells>, usize)>,
}

impl Learner {
    fn add(&mut self, role: Role, ch: char, cells: &Cells) {
        let (sum, n) = self
            .sums
            .entry((role, ch))
            .or_insert_with(|| (Box::new([0.0; GW * GH]), 0));
        for (s, v) in sum.iter_mut().zip(cells) {
            *s += v;
        }
        *n += 1;
    }

    /// Add the glyphs of one crop whose timer shows `timer` in wave `wave`
    pub fn add_crop(&mut self, parts: &Parts, wave: u8, timer: u8) {
        let digits = timer.to_string();
        if parts.timer.len() == digits.len() {
            for (b, ch) in parts.timer.iter().zip(digits.chars()) {
                self.add(Role::Timer, ch, &parts.cells(b));
            }
        }
        if let (Some(b), Some(ch)) = (&parts.wave, char::from_digit(u32::from(wave), 10)) {
            self.add(Role::Wave, ch, &parts.cells(b));
        }
        if parts.eggs.len() >= 4 {
            let b = &parts.eggs[parts.eggs.len() - 3];
            self.add(Role::Slash, '/', &parts.cells(b));
        }
    }

    /// Glyphs added per character
    pub fn counts(&self) -> Vec<(Role, char, usize)> {
        self.sums
            .iter()
            .map(|(&(r, c), (_, n))| (r, c, *n))
            .collect()
    }

    /// Mean templates for `game`
    pub fn templates(&self, game: &str) -> Vec<Template> {
        self.sums
            .iter()
            .map(|(&(role, ch), (sum, n))| {
                let mut cells = [0.0; GW * GH];
                for (c, s) in cells.iter_mut().zip(sum.iter()) {
                    *c = s / *n as f32;
                }
                Template {
                    game: game.to_string(),
                    role,
                    ch,
                    count: *n,
                    cells,
                }
            })
            .collect()
    }
}

/// Find the countdowns of a video from the timer's digit count (see
/// [`Anchor::Start`]), reading every frame
pub fn find_countdowns(
    path: &Path,
    info: &VideoInfo,
    region: Region,
    first_wave: u8,
) -> Result<Vec<Countdown>> {
    let mut counts: Vec<(f64, usize)> = Vec::new();
    for item in CropReader::start(path, region, None, info.fps, 0.0, None)? {
        let (t, crop) = item?;
        counts.push((t, Parts::find(&crop).map_or(0, |p| p.timer.len())));
    }
    let half = ((info.fps * 0.5).round() as usize).max(2);
    // The first frame showing `to` digits after half a second of mostly
    // `from` and followed by half a second of mostly `to`; with `apart`,
    // at least five seconds after the one before
    let switches = |from: usize, to: usize, apart: bool| -> Vec<f64> {
        let mut out: Vec<f64> = Vec::new();
        for i in half..counts.len().saturating_sub(half) {
            if counts[i].1 != to || counts[i - 1].1 == to {
                continue;
            }
            let before = counts[i - half..i].iter().filter(|c| c.1 == from).count();
            let after = counts[i..i + half].iter().filter(|c| c.1 == to).count();
            if before * 4 >= half * 3 && after * 4 >= half * 3 {
                let t = counts[i].0;
                if !apart || out.last().is_none_or(|&l| t - l > 5.0) {
                    out.push(t);
                }
            }
        }
        out
    };
    let nines = switches(2, 1, false);
    Ok(switches(3, 2, true)
        .into_iter()
        .enumerate()
        .map(|(i, t99)| Countdown {
            wave: first_wave + i as u8,
            t99,
            t9: nines
                .iter()
                .copied()
                .find(|&t| (t - t99 - 90.0).abs() < 1.0),
        })
        .collect())
}

/// Learn from one video: label its frames with `anchor` and add every
/// frame's glyphs to `learner`. Returns the countdowns used.
pub fn learn_video(
    learner: &mut Learner,
    reader: &Reader,
    path: &Path,
    info: &VideoInfo,
    region: Region,
    anchor: Anchor,
) -> Result<Vec<Countdown>> {
    let countdowns = match anchor {
        Anchor::Start { first_wave } => find_countdowns(path, info, region, first_wave)?,
        Anchor::Fit => {
            let every = 0.5;
            let mut samples = Vec::new();
            for item in CropReader::start(path, region, Some(every), info.fps, 0.0, None)? {
                let (t, crop) = item?;
                samples.push(Sample {
                    t,
                    hud: reader.read(&crop),
                });
            }
            let table = WaveTable {
                video: String::new(),
                duration_s: info.duration_s,
                every_s: every,
                region: Some(region),
                samples: samples.len(),
                hud_samples: 0,
                waves: find_waves(&samples, every),
            };
            table
                .waves
                .iter()
                .map(|w| Countdown {
                    wave: w.wave,
                    t99: w.to_video_time(99.0),
                    t9: None,
                })
                .collect()
        }
    };
    let margin = 2.0 / info.fps;
    for item in CropReader::start(path, region, None, info.fps, 0.0, None)? {
        let (t, crop) = item?;
        let Some((wave, timer)) = countdowns
            .iter()
            .find_map(|c| Some((c.wave, c.value(t, margin)?)))
        else {
            continue;
        };
        if let Some(parts) = Parts::find(&crop) {
            learner.add_crop(&parts, wave, timer);
        }
    }
    Ok(countdowns)
}

/// `base` with `learned` in place of its templates of the same game, role
/// and character; the others are kept, so characters a video does not show
/// (wave digits 4 and 5 outside Eggstra Work) stay as learned before
pub fn merge(base: &Templates, learned: Vec<Template>) -> Templates {
    let mut list: Vec<Template> = base
        .list
        .iter()
        .filter(|t| {
            !learned
                .iter()
                .any(|l| (&l.game, l.role, l.ch) == (&t.game, t.role, t.ch))
        })
        .cloned()
        .collect();
    list.extend(learned);
    list.sort_by(|a, b| (&a.game, a.role, a.ch).cmp(&(&b.game, b.role, b.ch)));
    Templates { list }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_values_skip_switches() {
        let c = Countdown {
            wave: 1,
            t99: 10.0,
            t9: None,
        };
        assert_eq!(c.value(10.5, 0.1), Some(99));
        assert_eq!(c.value(10.02, 0.1), None);
        assert_eq!(c.value(26.5, 0.1), Some(83));
        assert_eq!(c.value(108.5, 0.1), Some(1));
        assert_eq!(c.value(109.5, 0.1), None);
        assert_eq!(c.value(9.5, 0.1), None);
    }
}
