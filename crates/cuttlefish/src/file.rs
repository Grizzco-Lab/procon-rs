//! Local files: markdown and other plain text, HTML, PDF (through
//! `pdftotext` from poppler), Word `.docx` (through `bsdtar` from
//! libarchive) and subtitles (`.srt`, `.vtt`).

use crate::doc::{Document, SourceKind};
use crate::{html, youtube};
use alloc::string::String;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Runs a tool and returns its output as text
fn run(command: &mut Command, what: &str) -> Result<String> {
    let out = command
        .output()
        .with_context(|| alloc::format!("running {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from(String::from_utf8_lossy(&out.stdout)))
}

/// Replaces the five XML entities
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of a Word document's `word/document.xml`: one paragraph per
/// `<w:p>`, headings (styles `Title`, `Heading1`...) as markdown `#`
pub fn docx_text(xml: &str) -> String {
    let mut out = String::new();
    for paragraph in xml.split("</w:p>") {
        let style = paragraph
            .split_once("<w:pStyle w:val=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(style, _)| style)
            .unwrap_or_default();
        let level = match style.strip_prefix("Heading") {
            _ if style == "Title" => 1,
            Some(n) => n.parse::<usize>().unwrap_or(0).min(6),
            None => 0,
        };
        let mut text = String::new();
        let mut rest = paragraph;
        while let Some(open) = rest.find('<') {
            let Some(close) = rest[open..].find('>') else {
                break;
            };
            let tag = &rest[open + 1..open + close];
            rest = &rest[open + close + 1..];
            if tag == "w:t" || tag.starts_with("w:t ") {
                let end = rest.find("</w:t>").unwrap_or(rest.len());
                text.push_str(&unescape_xml(&rest[..end]));
                rest = &rest[end..];
            } else if tag.starts_with("w:tab") {
                text.push('\t');
            } else if tag.starts_with("w:br") {
                text.push('\n');
            }
        }
        let text = text.trim();
        if !text.is_empty() {
            if level > 0 {
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            out.push_str(text);
            out.push_str("\n\n");
        }
    }
    String::from(out.trim_end())
}

/// A file's title when it names one, its text and its license (HTML may
/// say)
pub fn read(path: &Path) -> Result<(Option<String>, String, Option<String>)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    Ok(match ext.as_str() {
        "pdf" => {
            let text = run(
                Command::new("pdftotext")
                    .arg("-enc")
                    .arg("UTF-8")
                    .arg(path)
                    .arg("-"),
                "pdftotext (install poppler)",
            )?;
            // Pages are separated by form feeds
            (None, text.replace('\u{c}', "\n\n"), None)
        }
        "docx" => {
            let xml = run(
                Command::new("bsdtar")
                    .arg("-xOf")
                    .arg(path)
                    .arg("word/document.xml"),
                "bsdtar (install libarchive)",
            )?;
            (None, docx_text(&xml), None)
        }
        "html" | "htm" | "xhtml" => {
            let page = html::convert(&String::from_utf8_lossy(&std::fs::read(path)?));
            (page.title, page.text, page.license)
        }
        "srt" | "vtt" => (
            None,
            youtube::vtt_to_text(&String::from_utf8_lossy(&std::fs::read(path)?)),
            None,
        ),
        _ => (
            None,
            String::from(String::from_utf8_lossy(&std::fs::read(path)?)),
            None,
        ),
    })
}

/// Reads a file into a document stored under `key`; the title is the
/// first `#` heading, the HTML title or the file name
pub fn load_keyed(path: &Path, key: &str, source: SourceKind) -> Result<Document> {
    let (title, text, license) = read(path)?;
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let title = title
        .or_else(|| {
            text.lines()
                .find_map(|l| l.strip_prefix("# "))
                .map(|t| String::from(t.trim()))
        })
        .unwrap_or(stem);
    let mut doc = Document::new(source, key, title, text);
    doc.license = license;
    Ok(doc)
}

/// Reads a file into a document stored under its full path
pub fn load(path: &Path, source: SourceKind) -> Result<Document> {
    let key = std::fs::canonicalize(path)?.to_string_lossy().into_owned();
    load_keyed(path, &key, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_markdown_with_its_title() {
        let path =
            std::env::temp_dir().join(alloc::format!("cuttlefish-{}.md", std::process::id()));
        std::fs::write(&path, "intro\n\n# Egg flow\n\nKeep eggs moving.").unwrap();
        let doc = load(&path, SourceKind::Guide).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(doc.title, "Egg flow");
        assert_eq!(doc.source, SourceKind::Guide);
        assert_eq!(doc.weight, SourceKind::Guide.default_weight());
    }

    #[test]
    fn word_paragraphs_and_headings() {
        let xml = r#"<w:body><w:p><w:pPr><w:pStyle w:val="Heading2"/></w:pPr><w:r><w:t>Tides</w:t></w:r></w:p><w:p><w:r><w:t xml:space="preserve">Low tide </w:t></w:r><w:r><w:t>&amp; fog</w:t></w:r><w:tbl/></w:p><w:p></w:p></w:body>"#;
        assert_eq!(docx_text(xml), "## Tides\n\nLow tide & fog");
    }

    #[test]
    fn subtitles_lose_their_timing() {
        let path =
            std::env::temp_dir().join(alloc::format!("cuttlefish-{}.srt", std::process::id()));
        std::fs::write(
            &path,
            "1\n00:00:01,000 --> 00:00:02,000\nKill the Steelhead\n\n2\n00:01:05,000 --> 00:01:06,000\nthen the Flyfish\n",
        )
        .unwrap();
        let (_, text, _) = read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(text, "[0:01] Kill the Steelhead\n\n[1:05] then the Flyfish");
    }
}
