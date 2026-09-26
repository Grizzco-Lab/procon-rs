//! Video transcripts through `yt-dlp` (subtitles and metadata only; no
//! video is downloaded).
//!
//! Works for YouTube videos, playlists and channels. Uploaded subtitles are
//! preferred over automatic captions. (Twitch VODs have no subtitles; they
//! would need speech-to-text first.)

use crate::doc::{Document, SourceKind};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Runs yt-dlp; stdout on success
fn yt_dlp(args: &[&str]) -> Result<String> {
    let out = Command::new("yt-dlp")
        .args(args)
        .output()
        .context("running yt-dlp (is it installed?)")?;
    if !out.status.success() {
        bail!(
            "yt-dlp failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Video ids behind a video, playlist or channel url
pub fn list_ids(url: &str) -> Result<Vec<String>> {
    let out = yt_dlp(&["--flat-playlist", "--print", "%(id)s", url])?;
    Ok(out
        .lines()
        .map(|l| String::from(l.trim()))
        .filter(|l| !l.is_empty())
        .collect())
}

/// Downloads one video's metadata and subtitles into `raw_dir` and turns
/// them into a document. `langs` is yt-dlp's `--sub-langs` list.
pub fn fetch(id: &str, raw_dir: &Path, langs: &str) -> Result<Document> {
    std::fs::create_dir_all(raw_dir)?;
    let template = raw_dir.join("%(id)s.%(ext)s");
    let url = alloc::format!("https://www.youtube.com/watch?v={id}");
    yt_dlp(&[
        "--skip-download",
        "--write-info-json",
        "--write-subs",
        "--write-auto-subs",
        "--sub-format",
        "vtt",
        "--sub-langs",
        langs,
        "--sleep-requests",
        "1",
        "--no-playlist",
        "-o",
        &template.to_string_lossy(),
        &url,
    ])?;
    let info_path = raw_dir.join(alloc::format!("{id}.info.json"));
    let info: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&info_path)
            .with_context(|| alloc::format!("reading {}", info_path.display()))?,
    )?;
    let manual: Vec<String> = info["subtitles"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    // (language, path, uploaded by the author)
    let mut subs: Vec<(String, std::path::PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(raw_dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Some(lang) = name
            .strip_prefix(&alloc::format!("{id}."))
            .and_then(|r| r.strip_suffix(".vtt"))
        {
            let lang = String::from(lang);
            let uploaded = manual.contains(&lang);
            subs.push((lang, path, uploaded));
        }
    }
    // Uploaded subtitles first, then the original-language captions
    subs.sort_by_key(|(lang, _, uploaded)| (!uploaded, !lang.ends_with("-orig"), lang.clone()));
    let Some((lang, path, _)) = subs.first() else {
        bail!("{id}: no subtitles in {langs}");
    };
    let transcript = vtt_to_text(&std::fs::read_to_string(path)?);
    let title = String::from(info["title"].as_str().unwrap_or(id));
    let channel = info["channel"]
        .as_str()
        .or(info["uploader"].as_str())
        .map(String::from);
    let description = info["description"].as_str().unwrap_or_default();
    let text = alloc::format!("# Description\n\n{description}\n\n# Transcript\n\n{transcript}");
    let page_url = String::from(info["webpage_url"].as_str().unwrap_or(&url));
    let mut doc = Document::new(SourceKind::Video, &page_url, title, text);
    doc.url = Some(page_url);
    doc.language = Some(String::from(lang.trim_end_matches("-orig")));
    doc.attribution = channel;
    doc.license =
        Some(String::from(info["license"].as_str().unwrap_or(
            "YouTube Standard License; transcript kept for personal study",
        )));
    Ok(doc)
}

/// WebVTT to text: one paragraph per minute, each starting with its
/// `[m:ss]` time, with the repeated lines of rolling captions removed
pub fn vtt_to_text(vtt: &str) -> String {
    let mut out = String::new();
    let mut last_line = String::new();
    let mut minute: Option<u64> = None;
    let mut cue_start = 0u64;
    for line in vtt.lines() {
        let line = line.trim();
        if let Some((start, _)) = line.split_once("-->") {
            cue_start = parse_time(start.trim()).unwrap_or(cue_start);
            continue;
        }
        if line.is_empty()
            || line == "WEBVTT"
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.chars().all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let text = strip_tags(line);
        let text = text.trim();
        if text.is_empty() || text == last_line {
            continue;
        }
        last_line = String::from(text);
        let m = cue_start / 60;
        if minute != Some(m) {
            minute = Some(m);
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&alloc::format!(
                "[{}:{:02}] ",
                cue_start / 60,
                cue_start % 60
            ));
        } else {
            out.push(' ');
        }
        out.push_str(text);
    }
    out
}

/// `hh:mm:ss.mmm` or `mm:ss.mmm` to whole seconds
fn parse_time(s: &str) -> Option<u64> {
    let s = s.split_whitespace().next()?;
    let mut secs = 0f64;
    for part in s.split(':') {
        secs = secs * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(secs as u64)
}

/// Removes `<...>` tags (timing and styling inside cues)
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&gt;", ">")
        .replace("&lt;", "<")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rolling_captions() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
            00:00:01.000 --> 00:00:03.000 align:start position:0%\n\
            kill<00:00:01.500><c> the</c> steelhead\n\n\
            00:00:03.000 --> 00:00:04.000\n\
            kill the steelhead\n\
            then the flyfish\n\n\
            00:01:05.000 --> 00:01:06.000\n\
            low tide &amp; fog\n";
        assert_eq!(
            vtt_to_text(vtt),
            "[0:01] kill the steelhead then the flyfish\n\n[1:05] low tide & fog"
        );
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_time("01:02:03.500"), Some(3723));
        assert_eq!(parse_time("02:03.000"), Some(123));
    }
}
