//! Video frames of a session segment, decoded by an ffmpeg subprocess.
//!
//! Frames come out as packed RGB at a fixed size and are numbered as in
//! the segment: frame `n` is shown at `n / fps` seconds. Only constant-rate
//! sessions (those with `video.fps` in `session.json`) are supported.

use anyhow::{Context, Result, bail};
use gameplay_data::session::SessionInfo;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

/// Width frames are decoded at, the dataset's 360p
pub const WIDTH: usize = 640;
/// Height frames are decoded at
pub const HEIGHT: usize = 360;

/// One segment of a session
#[derive(Clone, Debug)]
pub struct Segment {
    /// Session folder name, such as `2026-09-25_11-26-22`
    pub session: String,
    /// Video file name in the session folder, such as `video-01.mkv`
    pub file: String,
    /// Path to the video file
    pub video: PathBuf,
    /// Frames per second
    pub fps: f64,
}

impl Segment {
    /// Segment `index` (0-based) of the session folder `dir`
    pub fn open(dir: &Path, index: usize) -> Result<Self> {
        let info = SessionInfo::read(dir)?;
        let Some(fps) = info.video.fps else {
            bail!(
                "{} has variable-rate video (no video.fps), which is not supported",
                dir.display()
            );
        };
        let Some(segment) = info.video.segments.get(index) else {
            bail!(
                "{} has {} segments, no segment {}",
                dir.display(),
                info.video.segments.len(),
                index + 1
            );
        };
        let session = dir
            .file_name()
            .context("session path has no folder name")?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            session,
            file: segment.file.clone(),
            video: dir.join(&segment.file),
            fps: f64::from(fps),
        })
    }

    /// The video file name without its extension, such as `video-01`
    pub fn stem(&self) -> &str {
        self.file
            .rsplit_once('.')
            .map_or(self.file.as_str(), |(stem, _)| stem)
    }

    /// Seconds to seek to for frame `n`: half a frame early, so rounded
    /// container timestamps still land on it
    pub fn seek_s(&self, n: u64) -> f64 {
        ((n as f64 - 0.5) / self.fps).max(0.0)
    }
}

/// Which frames to read: `first`, then every `step`-th, at most `count`
#[derive(Clone, Copy, Debug)]
pub struct FrameRange {
    /// First frame number
    pub first: u64,
    /// Frames between two read frames; 1 reads all
    pub step: u64,
    /// Most frames to read; `None` reads to the end
    pub count: Option<u64>,
}

/// A decoded frame
pub struct Frame {
    /// Frame number in the segment
    pub number: u64,
    /// Packed RGB, [`WIDTH`] x [`HEIGHT`] x 3 bytes
    pub rgb: Vec<u8>,
}

/// Frames read from a running ffmpeg
pub struct FrameReader {
    child: Child,
    stdout: ChildStdout,
    range: FrameRange,
    read: u64,
}

impl FrameReader {
    /// Start decoding `range` of `segment`
    pub fn start(segment: &Segment, range: FrameRange) -> Result<Self> {
        let step = range.step.max(1);
        let mut filter = format!("scale={WIDTH}:{HEIGHT}");
        if step > 1 {
            // After the seek, `n` counts from the first frame read
            filter = format!("select=not(mod(n\\,{step})),{filter}");
        }
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-v", "error", "-nostdin", "-ss"])
            .arg(format!("{:.4}", segment.seek_s(range.first)))
            .arg("-i")
            .arg(&segment.video)
            .args(["-an", "-vf", &filter, "-fps_mode", "passthrough"]);
        if let Some(count) = range.count {
            cmd.args(["-frames:v", &count.to_string()]);
        }
        cmd.args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let mut child = cmd.spawn().context("cannot run ffmpeg")?;
        let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
        Ok(Self {
            child,
            stdout,
            range: FrameRange { step, ..range },
            read: 0,
        })
    }
}

impl Iterator for FrameReader {
    type Item = Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut rgb = vec![0u8; WIDTH * HEIGHT * 3];
        match self.stdout.read_exact(&mut rgb) {
            Ok(()) => {
                let number = self.range.first + self.read * self.range.step;
                self.read += 1;
                Some(Ok(Frame { number, rgb }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => None,
            Err(e) => Some(Err(e.into())),
        }
    }
}

impl Drop for FrameReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
