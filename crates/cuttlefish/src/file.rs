//! Local files: markdown, text, HTML and PDF (through `pdftotext` from
//! poppler).

use crate::doc::{Document, SourceKind};
use crate::html;
use alloc::string::String;
use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// Reads a file into a document; the title is the first `#` heading, the
/// HTML title or the file name
pub fn load(path: &Path, source: SourceKind) -> Result<Document> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let (title, text, license) = match ext.as_str() {
        "pdf" => {
            let out = Command::new("pdftotext")
                .arg("-enc")
                .arg("UTF-8")
                .arg(path)
                .arg("-")
                .output()
                .context("running pdftotext (install poppler)")?;
            if !out.status.success() {
                bail!("pdftotext failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            // Pages are separated by form feeds
            let text = String::from_utf8_lossy(&out.stdout).replace('\u{c}', "\n\n");
            (None, text, None)
        }
        "html" | "htm" => {
            let page = html::convert(&std::fs::read_to_string(path)?);
            (page.title, page.text, page.license)
        }
        _ => (None, std::fs::read_to_string(path)?, None),
    };
    let title = title
        .or_else(|| {
            text.lines()
                .find_map(|l| l.strip_prefix("# "))
                .map(|t| String::from(t.trim()))
        })
        .unwrap_or(stem);
    let key = std::fs::canonicalize(path)?.to_string_lossy().into_owned();
    let mut doc = Document::new(source, &key, title, text);
    doc.license = license;
    Ok(doc)
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
}
