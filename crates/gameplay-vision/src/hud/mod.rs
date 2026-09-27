//! Salmon Run HUD reader: the wave number, the wave's countdown timer and
//! the golden egg counter, from the top-left corner of a frame.
//!
//! The HUD sits in the top-left corner of the game picture in Splatoon 3
//! and Splatoon 2: the wave label (`WAVE 1`, `Wave 2`, `Welle 3`, ...), the
//! timer below it (`100` to `0`) and the egg counter to its right (`32/27`).
//! Reading goes in three steps:
//!
//! 1. The top-left 31.25% x 16.7% of the game picture is scaled to a
//!    [`HudCrop`] of [`CROP_W`] x [`CROP_H`] (a 1280x720 frame's corner at
//!    its own size), so sizes below do not depend on the video's resolution.
//! 2. Bright pixels (the smaller of red and green above [`BRIGHT`]: white
//!    and yellow digits, not the dark or orange band behind them) form
//!    connected blobs ([`glyph`]). The timer is the leftmost row of one to
//!    three digit-sized blobs in the lower half; the wave digit is the last
//!    tall blob of the label above it; the egg counter is the row of smaller
//!    blobs right of the timer, split by `/`.
//! 3. Each blob is scaled to a small grid and matched against templates
//!    built from real frames ([`learn`]), shipped in `templates.txt`.
//!
//! [`waves`] turns per-second readings into a wave table using the timer's
//! physics; [`video`] decodes the crops from a video file with ffmpeg.

pub mod glyph;
pub mod learn;
pub mod video;
pub mod waves;

use anyhow::Result;
use glyph::{Blob, Blobs, Cells, Templates};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::OnceLock;
use video::Region;
use waves::{Sample, WaveTable};

/// Width of a [`HudCrop`]
pub const CROP_W: usize = 400;
/// Height of a [`HudCrop`]
pub const CROP_H: usize = 120;
/// Share of the game picture's width a crop covers, from the left edge
pub const CROP_FRAC_W: f64 = 0.3125;
/// Share of the game picture's height a crop covers, from the top edge
pub const CROP_FRAC_H: f64 = 1.0 / 6.0;
/// Brightness (min of red and green) above which a pixel belongs to text
pub const BRIGHT: u8 = 170;
/// Least height of the wave label's digit; the label's letters (and
/// `XTRAWAVE`'s) are lower
pub const WAVE_DIGIT_H: usize = 23;
/// Lowest template score for a digit to count as read
pub const MIN_SCORE: f32 = 0.80;

/// The templates shipped with the crate (see [`learn`] to rebuild them)
const TEMPLATES: &str = include_str!("templates.txt");

/// The HUD corner of a frame, [`CROP_W`] x [`CROP_H`] packed RGB
#[derive(Clone)]
pub struct HudCrop {
    /// Packed RGB, `CROP_W * CROP_H * 3` bytes
    pub rgb: Vec<u8>,
}

impl HudCrop {
    /// The HUD corner of a whole frame (packed RGB, `width` x `height`)
    /// whose game picture fills the frame, resampled bilinearly
    pub fn from_frame(rgb: &[u8], width: usize, height: usize) -> Self {
        let sx = width as f64 * CROP_FRAC_W / CROP_W as f64;
        let sy = height as f64 * CROP_FRAC_H / CROP_H as f64;
        let mut out = vec![0u8; CROP_W * CROP_H * 3];
        for y in 0..CROP_H {
            let fy = ((y as f64 + 0.5) * sy - 0.5).clamp(0.0, (height - 1) as f64);
            let (y0, ty) = (fy.floor() as usize, fy.fract());
            let y1 = (y0 + 1).min(height - 1);
            for x in 0..CROP_W {
                let fx = ((x as f64 + 0.5) * sx - 0.5).clamp(0.0, (width - 1) as f64);
                let (x0, tx) = (fx.floor() as usize, fx.fract());
                let x1 = (x0 + 1).min(width - 1);
                for c in 0..3 {
                    let p = |xx: usize, yy: usize| f64::from(rgb[(yy * width + xx) * 3 + c]);
                    let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                    let bottom = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                    out[(y * CROP_W + x) * 3 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
                }
            }
        }
        Self { rgb: out }
    }

    /// Text mask: `true` where the pixel is bright enough to be text
    pub fn mask(&self) -> Vec<bool> {
        self.rgb
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0].min(p[1]) > BRIGHT)
            .collect()
    }
}

/// What the HUD shows in one frame
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hud {
    /// Wave number from the label; `None` for an extra wave (`XTRAWAVE`)
    /// or when the digit cannot be read
    pub wave: Option<u8>,
    /// Seconds left in the wave, 0 to 100
    pub timer_s: Option<u8>,
    /// Golden eggs delivered and the wave's quota
    pub eggs: Option<(u16, u16)>,
    /// Lowest template score of the timer's digits, 0 to 1
    pub confidence: f32,
}

/// Blobs of one HUD crop, grouped into the HUD's parts
pub struct Parts {
    /// Connected blobs of the text mask
    pub blobs: Blobs,
    /// Timer digits, left to right
    pub timer: Vec<Blob>,
    /// The wave label's last tall blob (its digit)
    pub wave: Option<Blob>,
    /// Egg counter glyphs, left to right, `/` included
    pub eggs: Vec<Blob>,
}

impl Parts {
    /// Find the HUD's parts in `crop`; `None` when there is no timer
    pub fn find(crop: &HudCrop) -> Option<Self> {
        let blobs = Blobs::find(&crop.mask(), CROP_W, CROP_H);
        let digit_like = |b: &Blob| {
            (20..=44).contains(&b.h) && b.w >= 3 && b.w * 20 <= b.h * 17 && b.area * 5 >= b.w * b.h
        };
        let mut cands: Vec<Blob> = blobs
            .list
            .iter()
            .filter(|b| digit_like(b) && b.x < 150 && (50..=112).contains(&(b.y + b.h / 2)))
            .copied()
            .collect();
        cands.sort_by_key(|b| b.x);
        // The timer: the leftmost run of aligned digits (the egg icon
        // starts right of it)
        let mut timer: Vec<Blob> = Vec::new();
        for b in cands {
            if let Some(last) = timer.last() {
                let aligned = last.y.abs_diff(b.y) <= 4 && last.h.abs_diff(b.h) * 6 <= last.h;
                let near = b.x < last.x + last.w + last.h && b.x + 1 >= last.x + last.w;
                if !(aligned && near && timer.len() < 3) {
                    break;
                }
            }
            timer.push(b);
        }
        let first = *timer.first()?;
        let last = *timer.last()?;
        let (top, h) = (first.y, first.h);
        let right = last.x + last.w;
        // The wave digit: the rightmost tall blob just above the timer
        let wave = blobs
            .list
            .iter()
            .filter(|b| {
                let bottom = b.y + b.h;
                bottom + 2 <= top
                    && bottom + 24 >= top
                    && b.h >= WAVE_DIGIT_H
                    && b.h <= h + 6
                    && b.x + b.w < right + 60
                    && b.w * 20 <= b.h * 17
            })
            .max_by_key(|b| b.x)
            .copied();
        // The egg counter: smaller glyphs on the timer's row, right of it
        let center = top + h / 2;
        let mut eggs: Vec<Blob> = blobs
            .list
            .iter()
            .filter(|b| {
                b.x > right + 20
                    && b.x < right + 240
                    && b.y < center
                    && b.y + b.h > center
                    && b.h * 20 >= h * 11
                    && b.h * 20 <= h * 19
                    && b.w * 20 <= b.h * 17
            })
            .copied()
            .collect();
        eggs.sort_by_key(|b| b.x);
        Some(Self {
            blobs,
            timer,
            wave,
            eggs,
        })
    }

    /// The grid of blob `b`, for matching
    pub fn cells(&self, b: &Blob) -> Cells {
        glyph::cells(&self.blobs, b)
    }
}

/// Reads HUDs with a set of templates
pub struct Reader {
    /// Templates the glyphs are matched against
    pub templates: Templates,
}

impl Reader {
    /// A reader with the templates shipped in the crate
    pub fn builtin() -> &'static Self {
        static READER: OnceLock<Reader> = OnceLock::new();
        READER.get_or_init(|| Reader {
            templates: Templates::parse(TEMPLATES).expect("built-in HUD templates"),
        })
    }

    /// Read the HUD of `crop`; `None` when no timer is shown (not in a wave)
    pub fn read(&self, crop: &HudCrop) -> Option<Hud> {
        let parts = Parts::find(crop)?;
        self.read_parts(&parts)
    }

    /// Read the HUD from already found parts
    pub fn read_parts(&self, parts: &Parts) -> Option<Hud> {
        // Timer
        let mut value = 0u32;
        let mut confidence = 1.0f32;
        for b in &parts.timer {
            let (ch, score) = self
                .templates
                .best(&parts.cells(b), |t| t.role == glyph::Role::Timer)?;
            confidence = confidence.min(score);
            value = value * 10 + ch.to_digit(10)?;
        }
        let digits = parts.timer.len();
        let leading_zero = digits > 1 && value < 10u32.pow(digits as u32 - 1);
        let timer_s =
            (confidence >= MIN_SCORE && value <= 100 && !leading_zero).then_some(value as u8);
        // Wave digit
        let wave = parts.wave.as_ref().and_then(|b| {
            let (ch, score) = self
                .templates
                .best(&parts.cells(b), |t| t.role == glyph::Role::Wave)?;
            let d = ch.to_digit(10)?;
            (score >= MIN_SCORE && d >= 1).then_some(d as u8)
        });
        // Egg counter: digits, `/`, digits
        let chars: Vec<char> = parts
            .eggs
            .iter()
            .map(|b| {
                self.templates
                    .best(&parts.cells(b), |t| {
                        t.role == glyph::Role::Timer || t.role == glyph::Role::Slash
                    })
                    .filter(|(_, score)| *score >= MIN_SCORE)
                    .map_or('?', |(ch, _)| ch)
            })
            .collect();
        let eggs = parse_eggs(&chars);
        if timer_s.is_none() && wave.is_none() {
            return None;
        }
        Some(Hud {
            wave,
            timer_s,
            eggs,
            confidence,
        })
    }
}

/// Read the HUD of `crop` with the built-in templates; `None` when no timer
/// is shown (not in a wave)
pub fn read(crop: &HudCrop) -> Option<Hud> {
    Reader::builtin().read(crop)
}

/// Read a video's HUD every `every_s` seconds and find its waves. The game
/// picture is `region`, or the frame without black bars.
pub fn scan(
    path: &Path,
    every_s: f64,
    region: Option<Region>,
    reader: &Reader,
) -> Result<(WaveTable, Vec<Sample>)> {
    let info = video::probe(path)?;
    let region = match region {
        Some(r) => r,
        None => video::detect_region(path, &info)?,
    };
    let mut samples = Vec::new();
    for item in video::CropReader::start(path, region, Some(every_s), info.fps, 0.0, None)? {
        let (t, crop) = item?;
        samples.push(Sample {
            t,
            hud: reader.read(&crop),
        });
    }
    let table = WaveTable {
        video: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        duration_s: info.duration_s,
        every_s,
        region: Some(region),
        samples: samples.len(),
        hud_samples: samples.iter().filter(|s| s.hud.is_some()).count(),
        waves: waves::find_waves(&samples, every_s),
    };
    Ok((table, samples))
}

/// `delivered/quota` from the egg counter's characters (`?` unread)
fn parse_eggs(chars: &[char]) -> Option<(u16, u16)> {
    let slash = chars.iter().position(|&c| c == '/')?;
    let number = |s: &[char]| -> Option<u16> {
        if s.is_empty() || s.len() > 3 {
            return None;
        }
        s.iter()
            .try_fold(0u16, |n, c| Some(n * 10 + c.to_digit(10)? as u16))
    };
    // Digits right before the slash, back to the first non-digit
    let before = &chars[..slash];
    let start = before
        .iter()
        .rposition(|c| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let after = &chars[slash + 1..];
    let end = after
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(after.len());
    Some((number(&before[start..])?, number(&after[..end])?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eggs_are_parsed_around_the_slash() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(parse_eggs(&c("32/27")), Some((32, 27)));
        assert_eq!(parse_eggs(&c("?0/27")), Some((0, 27)));
        assert_eq!(parse_eggs(&c("??5/31?")), Some((5, 31)));
        assert_eq!(parse_eggs(&c("32?27")), None);
        assert_eq!(parse_eggs(&c("/27")), None);
    }
}
