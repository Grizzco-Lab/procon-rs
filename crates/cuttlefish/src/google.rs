//! Google Docs, Sheets and Slides shared by link.
//!
//! Their pages are drawn with JavaScript, so fetching the address gives a
//! shell ("This browser version is no longer supported... File Edit View").
//! [`recognise`] finds the file in the address and [`GoogleFile::exports`]
//! gives its export addresses, tried in order: a document as Markdown
//! (headings kept), else plain text, else Word; a sheet as CSV (the sheet
//! the address names with `gid`, else the first); slides as plain text.
//!
//! Exports work only for files shared as "Anyone with the link can view";
//! others answer with Google's sign-in page or 401/403 ([`access_error`]).

use alloc::string::String;
use alloc::vec::Vec;

/// What the user sees when a file is not shared publicly
pub const NOT_SHARED: &str = "the doc isn't shared publicly (Anyone with the link can view): in Google Docs, Share > General access > Anyone with the link, or File > Download and import the file";

/// A Google file named by an address
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoogleFile {
    /// `docs.google.com/document/d/<id>`
    Doc(String),
    /// `docs.google.com/spreadsheets/d/<id>`, with the sheet's `gid` when
    /// the address names one
    Sheet { id: String, gid: Option<String> },
    /// `docs.google.com/presentation/d/<id>`
    Slides(String),
}

/// How an export is read
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Docx,
    Csv,
}

impl Format {
    /// The file extension the export is kept under
    pub fn ext(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Text => "txt",
            Format::Docx => "docx",
            Format::Csv => "csv",
        }
    }
}

/// The Google file an address names, if it is one on `docs.google.com`
/// (`/document/d/<id>/edit`, `/u/1/...`, `/a/<domain>/...` too). Published
/// copies (`/d/e/<id>/pub`) are plain pages and not recognised.
pub fn recognise(url: &str) -> Option<GoogleFile> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    if !host.eq_ignore_ascii_case("docs.google.com") {
        return None;
    }
    let (path, query) = match path.find(['?', '#']) {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, ""),
    };
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let kind = parts
        .iter()
        .position(|p| matches!(*p, "document" | "spreadsheets" | "presentation"))?;
    let tail = &parts[kind + 1..];
    // Skip `u/<n>` (the signed-in account)
    let tail = match tail {
        ["u", _, rest @ ..] => rest,
        _ => tail,
    };
    let id = match tail {
        ["d", id, ..] if *id != "e" && is_id(id) => String::from(*id),
        _ => return None,
    };
    Some(match parts[kind] {
        "document" => GoogleFile::Doc(id),
        "spreadsheets" => {
            let gid = query
                .split(['&', '#', '?'])
                .find_map(|p| p.strip_prefix("gid="))
                .filter(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()))
                .map(String::from);
            GoogleFile::Sheet { id, gid }
        }
        _ => GoogleFile::Slides(id),
    })
}

/// Whether a path segment can be a file id
fn is_id(s: &str) -> bool {
    s.len() >= 20
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl GoogleFile {
    /// Export addresses to try, in order, with how to read each
    pub fn exports(&self) -> Vec<(String, Format)> {
        const BASE: &str = "https://docs.google.com";
        match self {
            GoogleFile::Doc(id) => [
                ("md", Format::Markdown),
                ("txt", Format::Text),
                ("docx", Format::Docx),
            ]
            .iter()
            .map(|(f, format)| {
                (
                    alloc::format!("{BASE}/document/d/{id}/export?format={f}"),
                    *format,
                )
            })
            .collect(),
            GoogleFile::Sheet { id, gid } => {
                let gid = gid
                    .as_ref()
                    .map(|g| alloc::format!("&gid={g}"))
                    .unwrap_or_default();
                alloc::vec![(
                    alloc::format!("{BASE}/spreadsheets/d/{id}/export?format=csv{gid}"),
                    Format::Csv,
                )]
            }
            GoogleFile::Slides(id) => alloc::vec![(
                alloc::format!("{BASE}/presentation/d/{id}/export/txt"),
                Format::Text,
            )],
        }
    }

    /// What it is, in words
    pub fn kind(&self) -> &'static str {
        match self {
            GoogleFile::Doc(_) => "Google Doc",
            GoogleFile::Sheet { .. } => "Google Sheet",
            GoogleFile::Slides(_) => "Google Slides",
        }
    }
}

/// Why an export answer is not the file: not shared publicly (401, 403,
/// Google's sign-in page, or HTML where a file was asked for), or not
/// found. `None` when it may be the file.
pub fn access_error(status: u16, final_url: &str, is_html: bool) -> Option<String> {
    let host = final_url
        .split_once("://")
        .map_or(final_url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default();
    if matches!(status, 401 | 403) || host.eq_ignore_ascii_case("accounts.google.com") {
        return Some(String::from(NOT_SHARED));
    }
    if status == 404 {
        return Some(alloc::format!(
            "not found: check the address; if it is right, {NOT_SHARED}"
        ));
    }
    if (200..300).contains(&status) && is_html {
        return Some(String::from(NOT_SHARED));
    }
    None
}

/// The title a file name from `Content-Disposition` gives: without its
/// extension
pub fn title_of(file_name: &str) -> String {
    let stem = file_name
        .rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && ext.len() <= 5)
        .map_or(file_name, |(stem, _)| stem);
    String::from(stem.trim())
}

/// A Markdown export made fit to chunk: embedded images (`![][image1]` and
/// their base64 definitions) and heading anchors (`{#tides}`) dropped, the
/// table of contents (lines that only link inside the document) dropped,
/// other links inside the document kept as their text, escapes (`\-`)
/// undone
pub fn clean_markdown(md: &str) -> String {
    let mut out = String::new();
    let mut blank = true;
    for line in md.lines() {
        let trimmed = line.trim();
        let is_definition = trimmed.starts_with('[')
            && trimmed.split_once("]: ").is_some_and(|(_, target)| {
                target.starts_with("<data:") || target.starts_with("data:")
            });
        if is_definition || is_toc_line(trimmed) {
            continue;
        }
        let mut text = unlink(&drop_images(line));
        if trimmed.starts_with('#') {
            if let Some(i) = text.rfind(" {#").filter(|_| text.trim_end().ends_with('}')) {
                text.truncate(i);
            }
            // An empty heading
            if text.trim().trim_start_matches('#').trim().is_empty() {
                continue;
            }
        }
        let text = unescape(text.trim_end());
        if text.trim().is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(&text);
            out.push('\n');
            blank = false;
        }
    }
    String::from(out.trim())
}

/// Whether a line is only links inside the document (`[Tides 4](#tides)`),
/// as in a table of contents
fn is_toc_line(line: &str) -> bool {
    let mut rest = line;
    let mut any = false;
    while !rest.is_empty() {
        let Some(after) = rest.strip_prefix('[') else {
            return false;
        };
        let Some((_, after)) = after.split_once("](#") else {
            return false;
        };
        let Some((_, after)) = after.split_once(')') else {
            return false;
        };
        rest = after.trim_start();
        any = true;
    }
    any
}

/// Removes images: `![alt][ref]` and `![alt](target)`
fn drop_images(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find(']').and_then(|close| {
            let next = &after[close + 1..];
            let shut = match next.chars().next() {
                Some('[') => ']',
                Some('(') => ')',
                _ => return None,
            };
            next.find(shut).map(|e| close + 1 + e + 1)
        });
        match end {
            Some(end) => rest = &after[end..],
            None => {
                out.push_str("![");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Links inside the document (`[text](#anchor)`) as their text
fn unlink(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let after = &rest[start + 1..];
        let found = after.find("](#").and_then(|mid| {
            after[mid + 3..]
                .find(')')
                .map(|close| (mid, mid + 3 + close + 1))
        });
        match found {
            Some((mid, end)) if !after[..mid].contains('[') => {
                out.push_str(&rest[..start]);
                out.push_str(&after[..mid]);
                rest = &after[end..];
            }
            _ => {
                out.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Undoes Markdown escapes of ASCII punctuation (`\-` → `-`)
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some_and(char::is_ascii_punctuation) {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "1iK_dcrno5-cpR0ol4CBKLTDz46HMeM6AFPE0mw_sRBk";

    #[test]
    fn recognises_docs_and_rewrites_them() {
        let url = alloc::format!("https://docs.google.com/document/d/{ID}/edit?tab=t.0");
        let file = recognise(&url).unwrap();
        assert_eq!(file, GoogleFile::Doc(String::from(ID)));
        let exports = file.exports();
        assert_eq!(
            exports[0],
            (
                alloc::format!("https://docs.google.com/document/d/{ID}/export?format=md"),
                Format::Markdown
            )
        );
        assert_eq!(exports[1].1, Format::Text);
        assert_eq!(exports[2].1, Format::Docx);
        let other = alloc::format!("https://docs.google.com/document/u/1/d/{ID}/view#heading=h.x");
        assert_eq!(recognise(&other), Some(file));
        assert_eq!(
            recognise(&alloc::format!(
                "https://docs.google.com/a/example.org/document/d/{ID}"
            )),
            Some(GoogleFile::Doc(String::from(ID)))
        );
    }

    #[test]
    fn sheets_keep_their_gid_and_slides_export_text() {
        let sheet = recognise(&alloc::format!(
            "https://docs.google.com/spreadsheets/d/{ID}/edit#gid=123456"
        ))
        .unwrap();
        assert_eq!(
            sheet.exports()[0].0,
            alloc::format!(
                "https://docs.google.com/spreadsheets/d/{ID}/export?format=csv&gid=123456"
            )
        );
        let first = recognise(&alloc::format!(
            "https://docs.google.com/spreadsheets/d/{ID}/edit?usp=sharing"
        ))
        .unwrap();
        assert_eq!(
            first.exports(),
            [(
                alloc::format!("https://docs.google.com/spreadsheets/d/{ID}/export?format=csv"),
                Format::Csv
            )]
        );
        let slides = recognise(&alloc::format!(
            "https://docs.google.com/presentation/d/{ID}/edit#slide=id.p"
        ))
        .unwrap();
        assert_eq!(
            slides.exports()[0].0,
            alloc::format!("https://docs.google.com/presentation/d/{ID}/export/txt")
        );
    }

    #[test]
    fn leaves_other_addresses_alone() {
        for url in [
            "https://example.org/document/d/1iK_dcrno5-cpR0ol4CBKLTDz46HMeM6AFPE0mw_sRBk",
            "https://docs.google.com/document/d/e/2PACX-1vQ-published-copy-of-a-doc/pub",
            "https://docs.google.com/forms/d/1iK_dcrno5-cpR0ol4CBKLTDz46HMeM6AFPE0mw_sRBk",
            "https://docs.google.com/document/",
            "docs.google.com/document/d/1iK_dcrno5-cpR0ol4CBKLTDz46HMeM6AFPE0mw_sRBk",
        ] {
            assert_eq!(recognise(url), None, "{url}");
        }
    }

    #[test]
    fn tells_access_errors() {
        assert_eq!(
            access_error(403, "https://docs.google.com/x", false).as_deref(),
            Some(NOT_SHARED)
        );
        assert_eq!(
            access_error(
                200,
                "https://accounts.google.com/ServiceLogin?continue=x",
                true
            )
            .as_deref(),
            Some(NOT_SHARED)
        );
        assert_eq!(
            access_error(200, "https://docs.google.com/x", true).as_deref(),
            Some(NOT_SHARED)
        );
        assert!(
            access_error(404, "https://docs.google.com/x", true)
                .unwrap()
                .starts_with("not found")
        );
        assert_eq!(
            access_error(200, "https://doc-0k.googleusercontent.com/x", false),
            None
        );
        assert_eq!(access_error(500, "https://docs.google.com/x", true), None);
    }

    #[test]
    fn titles_from_file_names() {
        assert_eq!(title_of("Salmon Run: Tides.md"), "Salmon Run: Tides");
        assert_eq!(title_of("Names - Sheet1.csv"), "Names - Sheet1");
        assert_eq!(title_of("v1.2 notes"), "v1.2 notes");
    }

    #[test]
    fn cleans_markdown_exports() {
        let md = "# Overfishing Fundamentals\n\n## \n\n[**Introduction\t4**](#introduction)\n\n[Tides\t5](#tides) [Fog\t6](#fog)\n\n# Introduction {#introduction}\n\nSee [Tides](#tides) and [the wiki](https://example.org).\n\n![][image1]![][image2]\n\n## Egg Throwing {#egg-throwing}\n\nThrow \\- then roll\\. Aim at 1\\) the basket.\n\n[image1]: <data:image/png;base64,iVBORw0KGgo=>\n";
        assert_eq!(
            clean_markdown(md),
            "# Overfishing Fundamentals\n\n# Introduction\n\nSee Tides and [the wiki](https://example.org).\n\n## Egg Throwing\n\nThrow - then roll. Aim at 1) the basket."
        );
    }
}
