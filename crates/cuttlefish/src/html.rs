//! HTML to text with markdown headings.
//!
//! Keeps the article (MediaWiki content, `<main>` or `<article>` when the
//! page has one), turns `h1`-`h6` into `#` headings, list items into `- `
//! lines and table cells into `|`-separated rows, and drops scripts,
//! navigation, edit links, reference markers and tables of contents.

use alloc::string::String;
use scraper::{ElementRef, Html, Node, Selector};

/// A page converted to text
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Page {
    /// `<title>` (or the first `h1`)
    pub title: Option<String>,
    /// `<html lang>`, reduced to its primary code (`ja`)
    pub language: Option<String>,
    /// `<link rel="license">` target
    pub license: Option<String>,
    /// The site's name (`<meta property="og:site_name">`)
    pub site_name: Option<String>,
    /// Article text
    pub text: String,
}

/// Elements dropped with their content
const SKIP_TAGS: [&str; 10] = [
    "script", "style", "noscript", "nav", "footer", "header", "form", "button", "svg", "sup",
];
/// Classes dropped with their content (MediaWiki chrome)
const SKIP_CLASSES: [&str; 9] = [
    "toc",
    "navbox",
    "mw-editsection",
    "reference",
    "references",
    "noprint",
    "catlinks",
    "mw-jump-link",
    "printfooter",
];

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("valid selector")
}

/// Converts a whole page
pub fn convert(html: &str) -> Page {
    let doc = Html::parse_document(html);
    let first_text = |s: &str| {
        doc.select(&sel(s))
            .next()
            .map(|e| collapse(&e.text().collect::<String>()))
            .filter(|t| !t.is_empty())
    };
    let title = first_text("title").or_else(|| first_text("h1"));
    let language = doc
        .select(&sel("html"))
        .next()
        .and_then(|e| e.value().attr("lang"))
        .map(|l| String::from(l.split('-').next().unwrap_or(l)))
        .filter(|l| !l.is_empty());
    let license = doc
        .select(&sel("link[rel=license], a[rel=license]"))
        .next()
        .and_then(|e| e.value().attr("href"))
        .map(String::from);
    let site_name = doc
        .select(&sel("meta[property=\"og:site_name\"]"))
        .next()
        .and_then(|e| e.value().attr("content"))
        .map(collapse)
        .filter(|n| !n.is_empty());
    let root = [
        "#mw-content-text .mw-parser-output",
        "#mw-content-text",
        "main",
        "article",
        "body",
    ]
    .iter()
    .find_map(|s| doc.select(&sel(s)).next());
    let mut text = String::new();
    if let Some(root) = root {
        walk(root, &mut text);
    } else {
        walk(doc.root_element(), &mut text);
    }
    Page {
        title,
        language,
        license,
        site_name,
        text: tidy(&text),
    }
}

/// Phrases of pages that draw their content with JavaScript, lowercase
const SHELL_PHRASES: [&str; 8] = [
    "this browser version is no longer supported",
    "enable javascript",
    "javascript is required",
    "javascript is disabled",
    "requires javascript",
    "turn on javascript",
    "loading...",
    // "Loading..." in Japanese, where a script fills the page in
    "\u{8aad}\u{307f}\u{8fbc}\u{307f}\u{4e2d}...",
];

/// Text shorter than this may be a page shell, in characters
const SHELL_TEXT: usize = 2000;

/// Why a converted page is not worth keeping: no text, or the empty shell
/// of a page that draws its content with JavaScript (a known phrase in
/// little text, or almost no text in a large page). `html_bytes` is the
/// size of the page the text came from.
pub fn shell_reason(html_bytes: usize, text: &str) -> Option<&'static str> {
    let chars = text.chars().count();
    if chars < 40 {
        return Some("no text on the page");
    }
    if chars >= SHELL_TEXT {
        return None;
    }
    let lower = text.to_lowercase();
    if SHELL_PHRASES.iter().any(|p| lower.contains(p)) {
        return Some("the page draws its content with JavaScript; only its shell was received");
    }
    if html_bytes > 100 * text.len() {
        return Some(
            "almost no text in a large page: probably drawn with JavaScript; only its shell was received",
        );
    }
    None
}

/// Converts an HTML fragment (a MediaWiki `parse` result)
pub fn fragment_text(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let mut text = String::new();
    walk(doc.root_element(), &mut text);
    tidy(&text)
}

fn collapse(s: &str) -> String {
    s.split_whitespace()
        .collect::<alloc::vec::Vec<_>>()
        .join(" ")
}

/// Whether an element is hidden by its own style (`display: none`, as the
/// empty rows of a wiki's infobox)
fn hidden(el: ElementRef) -> bool {
    el.value().attr("style").is_some_and(|s| {
        s.split(';').any(|decl| {
            decl.split_once(':').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("display") && v.trim().starts_with("none")
            })
        })
    })
}

fn walk(el: ElementRef, out: &mut String) {
    let v = el.value();
    let name = v.name();
    if SKIP_TAGS.contains(&name) || v.classes().any(|c| SKIP_CLASSES.contains(&c)) || hidden(el) {
        return;
    }
    if let Some(level) = name
        .strip_prefix('h')
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| (1..=6).contains(n))
    {
        // Through `walk` so edit links and markers inside are dropped
        let mut inner = String::new();
        walk_children(el, &mut inner);
        let heading = collapse(&inner);
        if !heading.is_empty() {
            out.push_str("\n\n");
            out.push_str(&"#".repeat(level));
            out.push(' ');
            out.push_str(&heading);
            out.push_str("\n\n");
        }
        return;
    }
    match name {
        "p" | "div" | "section" | "table" | "ul" | "ol" | "dl" | "blockquote" | "pre" => {
            out.push_str("\n\n")
        }
        "li" => out.push_str("\n- "),
        "tr" | "dt" | "dd" | "br" => out.push('\n'),
        "td" | "th" => out.push_str(" | "),
        _ => {}
    }
    walk_children(el, out);
    if matches!(name, "p" | "div" | "table" | "ul" | "ol" | "blockquote") {
        out.push_str("\n\n");
    }
}

fn walk_children(el: ElementRef, out: &mut String) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => out.push_str(t),
            Node::Element(_) => {
                if let Some(e) = ElementRef::wrap(child) {
                    walk(e, out);
                }
            }
            _ => {}
        }
    }
}

/// Collapses spaces inside lines and runs of blank lines
fn tidy(s: &str) -> String {
    let mut out = String::new();
    let mut blank = true;
    for line in s.lines() {
        let line = collapse(line);
        let line = line.trim_start_matches("| ").trim_end_matches(" |");
        if line.is_empty() || line == "-" || line == "|" {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    String::from(out.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_wiki_page() {
        let html = r#"<html lang="en-US"><head><title>Steelhead - Wiki</title>
            <link rel="license" href="https://creativecommons.org/licenses/by-nc-sa/3.0/"></head>
            <body><nav>Menu</nav><div id="mw-content-text"><div class="mw-parser-output">
            <div class="toc">Contents</div>
            <p>A <b>boss</b> Salmonid.<sup class="reference">[1]</sup></p>
            <h2>Strategy<span class="mw-editsection">edit</span></h2>
            <ul><li>Shoot the bomb.</li><li>Stay near the basket.</li></ul>
            <table><tr><th>HP</th><td>1000</td></tr></table>
            <script>x()</script></div></div><footer>Foot</footer></body></html>"#;
        let page = convert(html);
        assert_eq!(page.title.as_deref(), Some("Steelhead - Wiki"));
        assert_eq!(page.language.as_deref(), Some("en"));
        assert!(page.license.unwrap().contains("by-nc-sa"));
        assert_eq!(
            page.text,
            "A boss Salmonid.\n\n## Strategy\n\n- Shoot the bomb.\n- Stay near the basket.\n\nHP | 1000"
        );
    }

    #[test]
    fn detects_page_shells() {
        // What a Google Doc gives without JavaScript
        let shell = "This browser version is no longer supported. Please upgrade to a supported browser.\n\nSalmon Run Next Wave: Overfishing Fundamentals\n\nShare\n\nFile\n\nEdit";
        assert!(shell_reason(300_000, shell).is_some());
        assert!(
            shell_reason(
                5_000,
                "Please enable JavaScript to continue using this site."
            )
            .is_some()
        );
        assert_eq!(shell_reason(100, "tiny"), Some("no text on the page"));
        // A schedule a script fills in, still loading
        let loading = "## Schedule (latest 5)\n\nLatest 5 / all\n\n\u{8aad}\u{307f}\u{8fbc}\u{307f}\u{4e2d}...\n\nVer.0.4.9";
        assert!(shell_reason(6_000, loading).is_some());
        // Little text in a huge page
        let short = "Salmon Run schedule and rotation for this week, updated daily.";
        assert!(shell_reason(2_000_000, short).is_some());
        assert_eq!(shell_reason(4_000, short), None);
        // A real article mentioning JavaScript is kept
        let long =
            "Keep the eggs moving toward the basket. ".repeat(60) + "Enable JavaScript for maps.";
        assert_eq!(shell_reason(900_000, &long), None);
    }

    #[test]
    fn converts_fragments() {
        let t = fragment_text("<div><h3>Tips</h3><p>Low tide.</p></div>");
        assert_eq!(t, "### Tips\n\nLow tide.");
        // An infobox's hidden rows are left out
        let t = fragment_text(
            "<table><tr style=\"display: none;\"><td>Full name</td><td></td></tr><tr style=\"padding: 5px\"><td>HP</td><td>1,200</td></tr></table>",
        );
        assert_eq!(t, "HP | 1,200");
    }
}
