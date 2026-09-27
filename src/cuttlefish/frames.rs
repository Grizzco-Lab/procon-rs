//! The reviewer's frames of a video, from ffmpeg, cached on disk.
//!
//! [`cuttlefish::sampling`] picks the times (on its 0.1 s grid) and the
//! height; a [`FrameSource`] extracts what is not cached yet in one ffmpeg
//! run (the stretch decoded once at the grid's rate, the wanted grid slots
//! picked by `select`, scaled to at most the source's height, JPEG at
//! [`QUALITY`]) and keeps each frame as
//!
//! ```text
//! <cache>/frames/<video key>/<tenths of a second>_<height>_q<quality>.jpg
//! ```
//!
//! where the video key ([`video_key`]) hashes the file's path, size and
//! modification time, so a replaced file gets new frames. Follow-up
//! questions about the same moment read the same files, which also keeps
//! the frames sent byte for byte the same, as the API's prompt cache
//! needs. A range's frame-difference signal ([`FrameSource::signal`]) is
//! cached beside them. Nothing is evicted: remove the folder to reclaim
//! the space.

use super::ai;
use crate::inspect::ffprobe;
use anyhow::{Context, Result, ensure};
use cuttlefish::sampling::{self, GRID_FPS, SIGNAL_FPS, SIGNAL_SIZE};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// JPEG quality on ffmpeg's `-q:v` scale (2 best to 31 worst)
pub const QUALITY: u32 = 3;

/// A video the reviewer looks at, with its frame cache
pub struct FrameSource {
    path: PathBuf,
    /// Width and height of the video
    pub size: (u32, u32),
    /// Length, seconds, when ffprobe knows it
    pub duration_s: Option<f64>,
    /// This video's folder in the frame cache
    dir: PathBuf,
}

/// The cache folder name of a video: a hash of its path, size and
/// modification time
pub fn video_key(path: &Path, len: u64, modified_ns: u128) -> String {
    let key = format!("{}|{len}|{modified_ns}", path.display());
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:016x}")
}

/// The cache file of the frame at `t_s`, `height` lines high
pub fn frame_file(dir: &Path, t_s: f64, height: u32) -> PathBuf {
    dir.join(format!(
        "{}_{height}_q{QUALITY}.jpg",
        sampling::grid_index(t_s)
    ))
}

/// Writes a cache file through a temporary one, so a reader never sees half
/// of it (no sync: a lost frame is extracted again)
fn write_cached(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let temporary = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Runs ffmpeg and returns its output
fn ffmpeg(args: &mut Command) -> Result<Vec<u8>> {
    let output = args
        .stdin(Stdio::null())
        .output()
        .context("cannot run ffmpeg")?;
    ensure!(
        output.status.success(),
        "ffmpeg: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(output.stdout)
}

impl FrameSource {
    /// A video and its folder under `cache` (the frame cache's root)
    pub fn open(path: &Path, cache: &Path) -> Result<Self> {
        let meta =
            std::fs::metadata(path).with_context(|| format!("cannot read {}", path.display()))?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let probe = ffprobe(path, "stream=width,height:format=duration")?;
        let stream = &probe["streams"][0];
        let dim = |key: &str| stream[key].as_u64().unwrap_or(0) as u32;
        Ok(Self {
            path: path.to_path_buf(),
            size: (dim("width"), dim("height")),
            duration_s: probe["format"]["duration"]
                .as_str()
                .and_then(|d| d.parse().ok()),
            dir: cache.join(video_key(path, meta.len(), modified)),
        })
    }

    /// Size of this video's frames at `height`, never above its own
    pub fn size_at(&self, height: u32) -> (u32, u32) {
        sampling::scaled_size(self.size, height)
    }

    /// The frames at `times` (on the grid, in time order), `height` lines
    /// high at most: from the cache, the missing ones extracted first.
    /// Times past the end of the video have none.
    pub fn frames(&self, times: &[f64], height: u32) -> Result<Vec<ai::Frame>> {
        let height = self.size_at(height).1;
        let missing: Vec<f64> = times
            .iter()
            .copied()
            .filter(|&t| !frame_file(&self.dir, t, height).is_file())
            .collect();
        if !missing.is_empty() {
            self.extract(&missing, height)?;
        }
        Ok(times
            .iter()
            .filter_map(|&t_s| {
                let jpeg = std::fs::read(frame_file(&self.dir, t_s, height)).ok()?;
                Some(ai::Frame { t_s, jpeg })
            })
            .collect())
    }

    /// Extracts the frames at `times` in one run over their stretch and
    /// writes them to the cache
    fn extract(&self, times: &[f64], height: u32) -> Result<()> {
        let first = sampling::grid_index(times[0]);
        let slots: Vec<u64> = times
            .iter()
            .map(|&t| sampling::grid_index(t).saturating_sub(first))
            .collect();
        let start = first as f64 / GRID_FPS;
        let span = (slots.last().copied().unwrap_or(0) + 1) as f64 / GRID_FPS;
        let select: Vec<String> = slots.iter().map(|n| format!("eq(n\\,{n})")).collect();
        let stdout = ffmpeg(
            Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{start:.3}"), "-t"])
                .arg(format!("{span:.3}"))
                .arg("-i")
                .arg(&self.path)
                .args(["-an", "-sn", "-vf"])
                .arg(format!(
                    "fps={GRID_FPS},select='{}',scale=-2:'min(ih\\,{height})'",
                    select.join("+")
                ))
                .args(["-fps_mode", "passthrough", "-f", "image2pipe"])
                .args(["-c:v", "mjpeg", "-q:v", &QUALITY.to_string(), "-"]),
        )?;
        // Frames come in time order; any the video lacks are at the end
        for (jpeg, &t) in super::split_jpegs(&stdout).into_iter().zip(times) {
            write_cached(&frame_file(&self.dir, t, height), jpeg)?;
        }
        Ok(())
    }

    /// How much the picture changes over `start_s..end_s`: per slot of
    /// 1 / [`SIGNAL_FPS`], the mean absolute difference (0–255) of a small
    /// grey thumbnail to the previous slot's (the first slot takes the
    /// second's); cached
    pub fn signal(&self, start_s: f64, end_s: f64) -> Result<Vec<f64>> {
        let cached = self.dir.join(format!(
            "signal_{}_{}.json",
            sampling::grid_index(start_s),
            sampling::grid_index(end_s)
        ));
        if let Some(diffs) = std::fs::read(&cached)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            return Ok(diffs);
        }
        let (w, h) = SIGNAL_SIZE;
        let raw = ffmpeg(
            Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{start_s:.3}"), "-t"])
                .arg(format!("{:.3}", end_s - start_s))
                .arg("-i")
                .arg(&self.path)
                .args(["-an", "-sn", "-vf"])
                .arg(format!("fps={SIGNAL_FPS},scale={w}:{h},format=gray"))
                .args(["-f", "rawvideo", "-"]),
        )?;
        let thumbs: Vec<&[u8]> = raw.chunks_exact((w * h) as usize).collect();
        let mut diffs: Vec<f64> = thumbs
            .windows(2)
            .map(|pair| {
                let sum: u64 = pair[0]
                    .iter()
                    .zip(pair[1])
                    .map(|(a, b)| u64::from(a.abs_diff(*b)))
                    .sum();
                // Rounded, so the cached copy reads back the same
                (sum as f64 / pair[0].len() as f64 * 1000.0).round() / 1000.0
            })
            .collect();
        if let Some(&first) = diffs.first() {
            diffs.insert(0, first);
        }
        write_cached(&cached, serde_json::to_string(&diffs)?.as_bytes())?;
        Ok(diffs)
    }

    /// Where the HUD's wave changes in `start_s..end_s`: the starts and ends
    /// of the waves in the video's wave table, if it has one
    pub fn wave_events(&self, start_s: f64, end_s: f64) -> Vec<f64> {
        match cuttlefish::corpus::load_table(&self.path) {
            Ok(Some(table)) => table
                .waves
                .waves
                .iter()
                .flat_map(|w| [w.start_video_s, w.end_video_s])
                .filter(|t| (start_s..=end_s).contains(t))
                .collect(),
            Ok(None) => Vec::new(),
            Err(e) => {
                log::warn!("No wave table for the frames: {e:#}");
                Vec::new()
            }
        }
    }

    /// `count` frame times over `start_s..end_s`, more where the picture or
    /// the wave changes ([`sampling::weighted_times`]); evenly spaced when
    /// the signal cannot be read
    pub fn range_times(&self, start_s: f64, end_s: f64, count: usize) -> Vec<f64> {
        let weights = match self.signal(start_s, end_s) {
            Ok(diffs) => {
                sampling::change_weights(&diffs, start_s, &self.wave_events(start_s, end_s))
            }
            Err(e) => {
                log::warn!("No frame-difference signal: {e:#}");
                Vec::new()
            }
        };
        sampling::weighted_times(start_s, end_s, count, &weights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys() {
        let a = video_key(Path::new("/v/a.mkv"), 100, 1);
        assert_eq!(a.len(), 16);
        assert_eq!(a, video_key(Path::new("/v/a.mkv"), 100, 1));
        // Another file, or the same file replaced, gets new frames
        assert_ne!(a, video_key(Path::new("/v/b.mkv"), 100, 1));
        assert_ne!(a, video_key(Path::new("/v/a.mkv"), 101, 1));
        assert_ne!(a, video_key(Path::new("/v/a.mkv"), 100, 2));
        // Times on the grid, height and quality in the name
        let dir = Path::new("/c");
        assert_eq!(frame_file(dir, 12.34, 480), Path::new("/c/123_480_q3.jpg"));
        assert_eq!(frame_file(dir, 12.3, 480), frame_file(dir, 12.301, 480));
        assert_ne!(frame_file(dir, 12.3, 480), frame_file(dir, 12.3, 720));
    }

    /// Height and width of a baseline JPEG, from its start of frame
    fn jpeg_size(jpeg: &[u8]) -> (u32, u32) {
        let i = jpeg
            .windows(2)
            .position(|w| w == [0xff, 0xc0])
            .expect("SOF0");
        let at = |k: usize| u32::from(jpeg[k]) << 8 | u32::from(jpeg[k + 1]);
        (at(i + 7), at(i + 5))
    }

    #[test]
    fn frames_are_cached_and_never_upscaled() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("procon-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("clip.mp4");
        ffmpeg(
            Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i"])
                .arg("testsrc2=size=320x180:rate=30")
                .args(["-t", "4", "-pix_fmt", "yuv420p", "-y"])
                .arg(&video),
        )
        .unwrap();
        let source = FrameSource::open(&video, &dir.join("cache")).unwrap();
        assert_eq!(source.size, (320, 180));
        let times = [0.5, 1.0, 1.2, 3.9, 9.0];
        let frames = source.frames(&times, 720).unwrap();
        // Past the end: none; 180 lines, not 720
        assert_eq!(
            frames.iter().map(|f| f.t_s).collect::<Vec<_>>(),
            [0.5, 1.0, 1.2, 3.9]
        );
        assert_eq!(jpeg_size(&frames[0].jpeg), (320, 180));
        let file = frame_file(&source.dir, 1.0, 180);
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        // Asked again, at a height the video cannot give either: the same files
        let again = source.frames(&[1.0, 1.2], 480).unwrap();
        assert_eq!(again[0], frames[1]);
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            written
        );
        // Smaller than the video: scaled down, even width
        let small = source.frames(&[1.0], 90).unwrap();
        assert_eq!(jpeg_size(&small[0].jpeg), (160, 90));
        // The signal: one value per half second, cached
        let diffs = source.signal(0.0, 4.0).unwrap();
        assert_eq!(diffs.len(), 8);
        assert_eq!(source.signal(0.0, 4.0).unwrap(), diffs);
        assert_eq!(source.range_times(0.0, 4.0, 4).len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
