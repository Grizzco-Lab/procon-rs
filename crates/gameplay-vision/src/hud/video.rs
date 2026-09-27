//! HUD crops of any video file, decoded by an ffmpeg subprocess.
//!
//! ffmpeg does the expensive part: it decodes, drops frames down to the
//! sampling rate, crops the HUD corner of the game picture and scales it
//! to a [`HudCrop`], so only 144 KB per sample reach Rust. The game
//! picture is the frame without black bars ([`detect_region`]) unless
//! given.

use super::{CROP_FRAC_H, CROP_FRAC_W, CROP_H, CROP_W, HudCrop};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

/// Size, length and frame rate of a video file
#[derive(Clone, Copy, Debug)]
pub struct VideoInfo {
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Length in seconds
    pub duration_s: f64,
    /// Average frames per second
    pub fps: f64,
}

/// Probe `path` with ffprobe
pub fn probe(path: &Path) -> Result<VideoInfo> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries"])
        .arg("stream=width,height,avg_frame_rate:format=duration")
        .args(["-of", "default=noprint_wrappers=1"])
        .arg(path)
        .output()
        .context("cannot run ffprobe")?;
    if !out.status.success() {
        bail!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .with_context(|| format!("ffprobe gave no {key} for {}", path.display()))
    };
    let fps = match field("avg_frame_rate")?.split_once('/') {
        Some((n, d)) => n.parse::<f64>()? / d.parse::<f64>()?.max(1.0),
        None => field("avg_frame_rate")?.parse()?,
    };
    Ok(VideoInfo {
        width: field("width")?.parse()?,
        height: field("height")?.parse()?,
        duration_s: field("duration")?.parse().unwrap_or(0.0),
        fps,
    })
}

/// The game picture inside a frame, in source pixels
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Region {
    /// Left edge
    pub x: u32,
    /// Top edge
    pub y: u32,
    /// Width
    pub w: u32,
    /// Height
    pub h: u32,
}

impl Region {
    /// The whole frame
    pub fn full(info: &VideoInfo) -> Self {
        Self {
            x: 0,
            y: 0,
            w: info.width,
            h: info.height,
        }
    }

    /// Parse `x,y,w,h`
    pub fn parse(s: &str) -> Result<Self> {
        let v: Vec<u32> = s
            .split(',')
            .map(|p| p.trim().parse())
            .collect::<Result<_, _>>()
            .with_context(|| format!("expected x,y,w,h, got {s:?}"))?;
        let [x, y, w, h] = v[..] else {
            bail!("expected x,y,w,h, got {s:?}");
        };
        Ok(Self { x, y, w, h })
    }

    /// ffmpeg filter cropping this region's HUD corner to a [`HudCrop`]
    pub fn crop_filter(&self) -> String {
        let w = ((f64::from(self.w) * CROP_FRAC_W).round() as u32).max(2);
        let h = ((f64::from(self.h) * CROP_FRAC_H).round() as u32).max(2);
        format!(
            "crop={w}:{h}:{}:{},scale={CROP_W}:{CROP_H}:flags=area",
            self.x, self.y
        )
    }
}

/// Pixel value above which a row or column counts as picture, not a bar
const BAR_LEVEL: u8 = 40;
/// Frames looked at to find black bars
const BAR_SAMPLES: u32 = 9;
/// Width the frames are scaled to for finding bars
const BAR_WIDTH: u32 = 320;

/// The game picture without black bars: rows and columns where no sampled
/// frame (nine, spread over the video) has more than 2% of pixels brighter
/// than a dark gray are bars. Falls back to the whole frame when the
/// picture found is under half the frame.
pub fn detect_region(path: &Path, info: &VideoInfo) -> Result<Region> {
    let bw = BAR_WIDTH;
    let bh =
        ((f64::from(info.height) * f64::from(bw) / f64::from(info.width)).round() as u32).max(2);
    let (bw, bh) = (bw as usize, bh as usize);
    let mut rows = vec![0usize; bh];
    let mut cols = vec![0usize; bw];
    let mut row_hit = vec![false; bh];
    let mut col_hit = vec![false; bw];
    for i in 1..=BAR_SAMPLES {
        let t = info.duration_s * f64::from(i) / f64::from(BAR_SAMPLES + 1);
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-ss", &format!("{t:.3}"), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-an", "-vf"])
            .arg(format!("scale={bw}:{bh}"))
            .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .stdin(Stdio::null())
            .output()
            .context("cannot run ffmpeg")?;
        if out.stdout.len() != bw * bh {
            continue;
        }
        rows.fill(0);
        cols.fill(0);
        for (j, &v) in out.stdout.iter().enumerate() {
            if v > BAR_LEVEL {
                rows[j / bw] += 1;
                cols[j % bw] += 1;
            }
        }
        for (hit, &n) in row_hit.iter_mut().zip(&rows) {
            *hit |= n * 50 > bw;
        }
        for (hit, &n) in col_hit.iter_mut().zip(&cols) {
            *hit |= n * 50 > bh;
        }
    }
    let span = |hit: &[bool]| {
        let first = hit.iter().position(|&h| h)?;
        let last = hit.iter().rposition(|&h| h)?;
        Some((first, last + 1))
    };
    let (Some((y0, y1)), Some((x0, x1))) = (span(&row_hit), span(&col_hit)) else {
        return Ok(Region::full(info));
    };
    if (y1 - y0) * 2 < bh || (x1 - x0) * 2 < bw {
        return Ok(Region::full(info));
    }
    // Back to source pixels; a bar thinner than 1% is noise
    let sx = f64::from(info.width) / bw as f64;
    let sy = f64::from(info.height) / bh as f64;
    let trim = |a: usize, b: usize, n: usize, s: f64, full: u32| {
        let a = if a * 100 <= n { 0 } else { a };
        let b = if (n - b) * 100 <= n { n } else { b };
        let lo = (a as f64 * s).round() as u32;
        let hi = ((b as f64 * s).round() as u32).min(full);
        (lo, hi - lo)
    };
    let (x, w) = trim(x0, x1, bw, sx, info.width);
    let (y, h) = trim(y0, y1, bh, sy, info.height);
    Ok(Region { x, y, w, h })
}

/// HUD crops read from a running ffmpeg, with their video times
pub struct CropReader {
    child: Child,
    stdout: ChildStdout,
    start_s: f64,
    step_s: f64,
    read: u64,
}

impl CropReader {
    /// Decode `path` from `start_s` for `duration_s` (to the end if
    /// `None`), one crop of `region` every `every_s` seconds, or every
    /// frame if `None` (then `fps` gives the times)
    pub fn start(
        path: &Path,
        region: Region,
        every_s: Option<f64>,
        fps: f64,
        start_s: f64,
        duration_s: Option<f64>,
    ) -> Result<Self> {
        let mut filter = region.crop_filter();
        if let Some(every) = every_s {
            filter = format!("fps={:.6}:round=up,{filter}", 1.0 / every);
        }
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin"]);
        if start_s > 0.0 {
            cmd.args(["-ss", &format!("{start_s:.3}")]);
        }
        cmd.arg("-i").arg(path);
        if let Some(d) = duration_s {
            cmd.args(["-t", &format!("{d:.3}")]);
        }
        cmd.args(["-an", "-sn", "-dn", "-vf", &filter])
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let mut child = cmd.spawn().context("cannot run ffmpeg")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
        Ok(Self {
            child,
            stdout,
            start_s,
            step_s: every_s.unwrap_or(1.0 / fps),
            read: 0,
        })
    }
}

impl Iterator for CropReader {
    /// Video time in seconds and the crop
    type Item = Result<(f64, HudCrop)>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut rgb = vec![0u8; CROP_W * CROP_H * 3];
        match self.stdout.read_exact(&mut rgb) {
            Ok(()) => {
                let t = self.start_s + self.read as f64 * self.step_s;
                self.read += 1;
                Some(Ok((t, HudCrop { rgb })))
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => None,
            Err(e) => Some(Err(e.into())),
        }
    }
}

impl Drop for CropReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_parses_and_crops_the_corner() {
        let r = Region::parse("0, 12, 1280,720").unwrap();
        assert_eq!(
            r,
            Region {
                x: 0,
                y: 12,
                w: 1280,
                h: 720
            }
        );
        assert_eq!(
            r.crop_filter(),
            "crop=400:120:0:12,scale=400:120:flags=area"
        );
        assert!(Region::parse("1,2,3").is_err());
    }
}
