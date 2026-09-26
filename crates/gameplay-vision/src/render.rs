//! Frames with their boxes drawn, as PNG, through ffmpeg's `drawbox` and
//! `drawtext` filters. For looking at results, not for speed.

use crate::frames::{HEIGHT, Segment, WIDTH};
use crate::labels::ObjectBox;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Colors for classes without one, `rrggbb`
const PALETTE: [&str; 8] = [
    "e6194b", "3cb44b", "ffe119", "4363d8", "f58231", "911eb4", "46f0f0", "f032e6",
];

/// A color for `class`: from `colors` (`(name, "#rrggbb")`) or picked from
/// a fixed palette by the name
pub fn class_color(class: &str, colors: &[(String, String)]) -> String {
    if let Some((_, c)) = colors.iter().find(|(name, _)| name == class) {
        return c.trim_start_matches('#').to_string();
    }
    let hash = class
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b.into()));
    PALETTE[hash % PALETTE.len()].to_string()
}

/// Draw `boxes` on frame `frame` of `segment` and save it as `out` (PNG)
pub fn render_frame(
    segment: &Segment,
    frame: u64,
    boxes: &[ObjectBox],
    colors: &[(String, String)],
    out: &Path,
) -> Result<()> {
    let mut filter = format!("scale={WIDTH}:{HEIGHT}");
    for b in boxes {
        let color = class_color(&b.class, colors);
        let (x, y) = ((b.x * WIDTH as f64) as i32, (b.y * HEIGHT as f64) as i32);
        let (w, h) = ((b.w * WIDTH as f64) as i32, (b.h * HEIGHT as f64) as i32);
        filter.push_str(&format!(
            ",drawbox=x={x}:y={y}:w={w}:h={h}:color=0x{color}:t=2"
        ));
        let mut text = String::new();
        if let Some(id) = b.id {
            text.push_str(&format!("#{id} "));
        }
        // Names are plain identifiers; keep only characters safe in a filter
        text.extend(
            b.class
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ' '),
        );
        if let Some(score) = b.score {
            text.push_str(&format!(" {score:.2}"));
        }
        filter.push_str(&format!(
            ",drawtext=text='{text}':x={x}:y=max({y}-14\\,0):fontsize=12:fontcolor=white:box=1:boxcolor=0x{color}@0.8:boxborderw=2"
        ));
    }
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-ss"])
        .arg(format!("{:.4}", segment.seek_s(frame)))
        .arg("-i")
        .arg(&segment.video)
        .args(["-an", "-frames:v", "1", "-vf", &filter])
        .arg(out)
        .output()
        .context("cannot run ffmpeg")?;
    if !output.status.success() {
        bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
