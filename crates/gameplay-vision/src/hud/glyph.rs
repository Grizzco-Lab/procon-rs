//! Glyphs: connected blobs of a text mask, scaled to a small grid and
//! matched against templates.

use anyhow::{Context, Result, bail};
use core::fmt::Write;

/// Grid width a glyph is scaled to
pub const GW: usize = 20;
/// Grid height a glyph is scaled to; the blob's height fills it
pub const GH: usize = 24;

/// A glyph on the grid: the share of each cell covered by the blob, 0 to 1
pub type Cells = [f32; GW * GH];

/// A connected blob of mask pixels (4-connected)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blob {
    /// Label in [`Blobs::labels`]
    pub id: u32,
    /// Left edge
    pub x: usize,
    /// Top edge
    pub y: usize,
    /// Width
    pub w: usize,
    /// Height
    pub h: usize,
    /// Pixels in the blob
    pub area: usize,
}

/// The blobs of a mask, with the label image
pub struct Blobs {
    /// Mask width
    pub width: usize,
    /// Mask height
    pub height: usize,
    /// Per pixel: 0 for background, else the blob's id
    pub labels: Vec<u32>,
    /// All blobs; blob `id` is at index `id - 1`
    pub list: Vec<Blob>,
}

impl Blobs {
    /// Label the connected blobs of `mask` (`width` x `height`)
    pub fn find(mask: &[bool], width: usize, height: usize) -> Self {
        let mut labels = vec![0u32; width * height];
        let mut list = Vec::new();
        let mut stack = Vec::new();
        for start in 0..mask.len() {
            if !mask[start] || labels[start] != 0 {
                continue;
            }
            let id = list.len() as u32 + 1;
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            let mut area = 0;
            labels[start] = id;
            stack.push(start);
            while let Some(i) = stack.pop() {
                let (x, y) = (i % width, i / width);
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                area += 1;
                let mut visit = |j: usize| {
                    if mask[j] && labels[j] == 0 {
                        labels[j] = id;
                        stack.push(j);
                    }
                };
                if x > 0 {
                    visit(i - 1);
                }
                if x + 1 < width {
                    visit(i + 1);
                }
                if y > 0 {
                    visit(i - width);
                }
                if y + 1 < height {
                    visit(i + width);
                }
            }
            list.push(Blob {
                id,
                x: x0,
                y: y0,
                w: x1 - x0 + 1,
                h: y1 - y0 + 1,
                area,
            });
        }
        Self {
            width,
            height,
            labels,
            list,
        }
    }

    /// 1 inside blob `id`, 0 elsewhere (and outside the mask)
    fn inside(&self, id: u32, x: isize, y: isize) -> f32 {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return 0.0;
        }
        f32::from(u8::from(
            self.labels[y as usize * self.width + x as usize] == id,
        ))
    }
}

/// Blob `b` on the grid: scaled so its height fills [`GH`], centered
/// horizontally (a narrow `1` stays narrow), sampled bilinearly
pub fn cells(blobs: &Blobs, b: &Blob) -> Cells {
    stretched(blobs, b, 1.0)
}

/// Blob `b` on the grid as [`cells`] puts it, then stretched sideways by
/// `stretch` (a glyph of a narrower font, scaled to the usual width)
pub fn stretched(blobs: &Blobs, b: &Blob, stretch: f32) -> Cells {
    let scale = GH as f32 / b.h as f32;
    let scale_x = scale * stretch;
    let offset = (GW as f32 - b.w as f32 * scale_x) / 2.0;
    let mut out = [0.0; GW * GH];
    for gy in 0..GH {
        let sy = b.y as f32 + (gy as f32 + 0.5) / scale - 0.5;
        let (y0, ty) = (sy.floor(), sy - sy.floor());
        for gx in 0..GW {
            let sx = b.x as f32 + (gx as f32 + 0.5 - offset) / scale_x - 0.5;
            let (x0, tx) = (sx.floor(), sx - sx.floor());
            let (xi, yi) = (x0 as isize, y0 as isize);
            let p = |dx: isize, dy: isize| blobs.inside(b.id, xi + dx, yi + dy);
            let top = p(0, 0) * (1.0 - tx) + p(1, 0) * tx;
            let bottom = p(0, 1) * (1.0 - tx) + p(1, 1) * tx;
            out[gy * GW + gx] = top * (1.0 - ty) + bottom * ty;
        }
    }
    out
}

/// Similarity of two grids: 1 minus the mean absolute difference
pub fn score(a: &Cells, b: &Cells) -> f32 {
    let diff: f32 = a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum();
    1.0 - diff / (GW * GH) as f32
}

/// What a template is for
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// Timer digits, also used for the egg counter's digits
    Timer,
    /// The wave label's digit
    Wave,
    /// The egg counter's `/`
    Slash,
}

impl Role {
    /// Name in the templates file
    pub fn name(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Wave => "wave",
            Self::Slash => "slash",
        }
    }

    /// The role named `s` in the templates file
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "timer" => Self::Timer,
            "wave" => Self::Wave,
            "slash" => Self::Slash,
            _ => bail!("unknown template role {s:?}"),
        })
    }
}

/// One template: the mean grid of a character's glyphs
#[derive(Clone)]
pub struct Template {
    /// Game the glyphs come from, such as `s3` or `s2`
    pub game: String,
    /// What the character is for
    pub role: Role,
    /// The character: a digit or `/`
    pub ch: char,
    /// Glyphs averaged
    pub count: usize,
    /// Mean grid
    pub cells: Cells,
}

/// A set of templates
#[derive(Clone, Default)]
pub struct Templates {
    /// All templates, in file order
    pub list: Vec<Template>,
}

impl Templates {
    /// Parse the text format: per template a line `<game> <role> <char>
    /// <count>`, then [`GH`] lines of [`GW`] hex digits (coverage x 15).
    /// Lines starting with `#` are comments.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'));
        let mut list = Vec::new();
        while let Some(head) = lines.next() {
            let f: Vec<&str> = head.split_whitespace().collect();
            let [game, role, ch, count] = f[..] else {
                bail!("bad template header {head:?}");
            };
            let mut cells = [0.0; GW * GH];
            for row in 0..GH {
                let line = lines.next().context("template ends early")?;
                if line.len() != GW {
                    bail!("template row {line:?} is not {GW} wide");
                }
                for (col, c) in line.chars().enumerate() {
                    let v = c.to_digit(16).context("template cell is not hex")?;
                    cells[row * GW + col] = v as f32 / 15.0;
                }
            }
            list.push(Template {
                game: game.to_string(),
                role: Role::parse(role)?,
                ch: ch.chars().next().context("empty template character")?,
                count: count.parse()?,
                cells,
            });
        }
        Ok(Self { list })
    }

    /// The text format read by [`Templates::parse`]
    pub fn to_text(&self) -> String {
        let mut s = String::from(
            "# gameplay-vision HUD glyph templates, written by `gameplay-vision hud learn`\n\
             # <game> <role> <char> <glyphs averaged>, then the grid: coverage x 15 in hex\n",
        );
        for t in &self.list {
            let _ = writeln!(s, "{} {} {} {}", t.game, t.role.name(), t.ch, t.count);
            for row in t.cells.chunks(GW) {
                for v in row {
                    let _ = write!(s, "{:x}", (v * 15.0).round().clamp(0.0, 15.0) as u8);
                }
                s.push('\n');
            }
        }
        s
    }

    /// The best matching character among templates passing `filter`, with
    /// its score
    pub fn best(&self, cells: &Cells, filter: impl Fn(&Template) -> bool) -> Option<(char, f32)> {
        self.list
            .iter()
            .filter(|t| filter(t))
            .map(|t| (t.ch, score(cells, &t.cells)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blobs_are_4_connected() {
        #[rustfmt::skip]
        let m = [
            1, 1, 0, 0,
            0, 1, 0, 1,
            0, 0, 1, 1,
        ];
        let mask: Vec<bool> = m.iter().map(|&v| v == 1).collect();
        let blobs = Blobs::find(&mask, 4, 3);
        assert_eq!(blobs.list.len(), 2);
        assert_eq!(
            (blobs.list[0].w, blobs.list[0].h, blobs.list[0].area),
            (2, 2, 3)
        );
        assert_eq!(
            (blobs.list[1].x, blobs.list[1].y, blobs.list[1].area),
            (2, 1, 3)
        );
    }

    #[test]
    fn a_full_block_fills_its_height_and_keeps_its_aspect() {
        // A 5 x 12 block: scaled by 2 to 10 x 24, centered in 20 columns
        let (w, h) = (9, 14);
        let mask: Vec<bool> = (0..w * h)
            .map(|i| (2..7).contains(&(i % w)) && (1..13).contains(&(i / w)))
            .collect();
        let blobs = Blobs::find(&mask, w, h);
        let c = cells(&blobs, &blobs.list[0]);
        assert!(c[12 * GW + 10] > 0.99);
        assert!(c[12 * GW + 2] < 0.01 && c[12 * GW + 17] < 0.01);
        assert!((score(&c, &c) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn templates_round_trip_through_text() {
        let mut cells = [0.0; GW * GH];
        cells[5] = 1.0;
        cells[30] = 7.0 / 15.0;
        let t = Templates {
            list: vec![Template {
                game: "s3".into(),
                role: Role::Slash,
                ch: '/',
                count: 4,
                cells,
            }],
        };
        let back = Templates::parse(&t.to_text()).unwrap();
        assert_eq!(back.list.len(), 1);
        assert_eq!((back.list[0].role, back.list[0].ch), (Role::Slash, '/'));
        assert_eq!(back.list[0].cells, cells);
    }
}
