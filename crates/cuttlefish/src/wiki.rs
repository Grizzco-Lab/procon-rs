//! Whole-topic imports: a topic of a MediaWiki wiki through its API, and a
//! whole site crawled page by page on its own host.
//!
//! [`Wiki`]: start pages and categories, as titles or addresses. A category
//! brings its articles and, down to a depth, its subcategories' (listed with
//! their latest revisions); a start page brings itself and, when words are
//! given, the pages it links to whose titles contain one of them. Each page
//! is fetched rendered (`action=parse`) and turned into text with headings
//! ([`html::fragment_text`]); its document records the url, the revision
//! and the wiki's license, credited to the wiki's contributors. A re-run
//! fetches only the pages whose revision changed, and the page cap counts
//! only pages fetched, so a run stopped early or capped continues.
//!
//! [`Site`]: from a start address, the pages on the same host that its
//! links and its sitemaps reach, up to a cap of pages fetched. Assets
//! (images, scripts, styles, fonts, feeds) and the path prefixes given are
//! skipped, and a page drawn with JavaScript is noted and not kept
//! ([`html::shell_reason`]). Every page fetched is kept raw; a re-run reads
//! a page with a raw copy from it (links, and the document when it is not
//! stored yet) instead of fetching it again, so it continues where the last
//! one stopped.
//!
//! Both are polite ([`crate::crawl::Fetcher`]): one request at a time, the
//! given delay or the site's `Crawl-delay` when longer, every `robots.txt`
//! rule (for the API and for each page's address). A dry run tells what is
//! in scope, what is new or changed and how long fetching it would take,
//! and stores nothing. Pages go to the data folder's `raw/wiki/` or
//! `raw/web/`, documents to `docs/`.

use crate::crawl::{CATEGORY_NS, Fetcher, Listed, MediaWiki, sitemap_locs};
use crate::doc::{Document, SourceKind, doc_id, guess_language};
use crate::html;
use crate::ingest::{Meta, Sink, add, check, save_raw};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use core::time::Duration;
use scraper::{Html, Selector};
use serde::Deserialize;

/// Titles listed per line of a dry run's notes
const TITLES_PER_LINE: usize = 8;

/// Sitemap files read at most for one site
const MAX_SITEMAPS: usize = 20;

/// A MediaWiki topic to import
#[derive(Clone, Debug, PartialEq, Deserialize, clap::Args)]
#[serde(default)]
pub struct Wiki {
    /// Start pages or categories: titles (`Salmon Run`,
    /// `Category:Salmon Run`) or their addresses
    /// (`https://splatoonwiki.org/wiki/Salmon_Run`)
    #[arg(required = true)]
    pub start: Vec<String>,
    /// The wiki's api.php; by default `/w/api.php` on the host of the first
    /// start address
    #[arg(long)]
    pub api: Option<String>,
    /// Levels of subcategories followed below a start category (0: only
    /// its own pages)
    #[arg(long, default_value_t = 2)]
    pub depth: usize,
    /// A category left out with its pages, as a subcategory too
    /// (`Category:Mechanics`; repeatable)
    #[arg(long, value_name = "CATEGORY")]
    pub exclude: Vec<String>,
    /// Also the pages a start page links to whose titles contain this word
    /// (any case; repeatable)
    #[arg(long = "link-match", value_name = "WORD")]
    pub link_match: Vec<String>,
    /// At most this many pages fetched a run; stored pages whose revision
    /// is unchanged do not count, so the next run continues with the rest
    #[arg(long, default_value_t = 500)]
    pub max_pages: usize,
    /// Seconds between requests (the wiki's Crawl-delay when longer); at
    /// least 1
    #[arg(long, default_value_t = 2.0)]
    pub delay_s: f32,
    /// Only tell what is in scope and how long fetching it would take;
    /// fetch no page and store nothing
    #[arg(long)]
    pub dry_run: bool,
}

impl Default for Wiki {
    fn default() -> Self {
        Wiki {
            start: Vec::new(),
            api: None,
            depth: 2,
            exclude: Vec::new(),
            link_match: Vec::new(),
            max_pages: 500,
            delay_s: 2.0,
            dry_run: false,
        }
    }
}

/// A site to crawl
#[derive(Clone, Debug, PartialEq, Deserialize, clap::Args)]
#[serde(default)]
pub struct Site {
    /// Start address; only pages on its host are crawled
    #[arg(required = true)]
    pub start: String,
    /// Path prefix skipped besides assets (`/map/`; repeatable)
    #[arg(long, value_name = "PREFIX")]
    pub skip: Vec<String>,
    /// At most this many pages fetched a run; pages read from their raw
    /// copies do not count, so the next run continues past them
    #[arg(long, default_value_t = 100)]
    pub max_pages: usize,
    /// Seconds between requests (the site's Crawl-delay when longer); at
    /// least 1
    #[arg(long, default_value_t = 2.0)]
    pub delay_s: f32,
    /// Only tell what the site holds (sections, pages); store nothing.
    /// Without a sitemap the pages are still fetched to find their links.
    #[arg(long)]
    pub dry_run: bool,
}

impl Default for Site {
    fn default() -> Self {
        Site {
            start: String::new(),
            skip: Vec::new(),
            max_pages: 100,
            delay_s: 2.0,
            dry_run: false,
        }
    }
}

/// The politeness delay asked for, at least a second
fn delay(seconds: f32) -> Duration {
    Duration::from_secs_f32(if seconds.is_finite() {
        seconds.max(1.0)
    } else {
        1.0
    })
}

/// Seconds as minutes and hours (`2 h 5 min`, `40 s`)
fn duration_text(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => alloc::format!("{s} s"),
        60..3600 => alloc::format!("{} min", s.div_ceil(60)),
        _ => alloc::format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}

/// `scheme://host` of an http(s) address
pub fn origin(url: &str) -> Option<&str> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    (end > 0).then(|| &url[..scheme.len() + 3 + end])
}

/// A start of a [`Wiki`] as a title: an address `.../wiki/<Title>` gives
/// its title (underscores as spaces), anything else is a title already
fn start_title(start: &str) -> Result<String> {
    let start = start.trim();
    if origin(start).is_none() {
        return Ok(String::from(start));
    }
    let path = start.split(['?', '#']).next().unwrap_or(start);
    let Some((_, title)) = path.split_once("/wiki/") else {
        bail!("{start}: give the page's /wiki/ address or its title");
    };
    let title = crate::crawl::decode(title).unwrap_or_else(|| String::from(title));
    Ok(title.replace('_', " "))
}

/// Whether a title names a category
fn is_category(title: &str) -> bool {
    title
        .get(..9)
        .is_some_and(|p| p.eq_ignore_ascii_case("category:"))
}

/// Whether two titles name the same page: any case, underscores as spaces,
/// `Category:` optional
fn same_category(a: &str, b: &str) -> bool {
    let key = |t: &str| {
        let t = t.trim().replace('_', " ").to_lowercase();
        match t.strip_prefix("category:") {
            Some(rest) => String::from(rest.trim()),
            None => t,
        }
    };
    key(a) == key(b)
}

/// Whether a title contains one of the words (any case)
fn matches_any(title: &str, words: &[String]) -> bool {
    let title = title.to_lowercase();
    words
        .iter()
        .map(|w| w.trim().to_lowercase())
        .any(|w| !w.is_empty() && title.contains(&w))
}

/// The pages of a topic: the start categories' articles, down to `depth`
/// levels of subcategories, then the start pages and the pages they link
/// to that match; each once, in that order. Also answers the number of
/// categories read.
fn scope(
    sink: &mut dyn Sink,
    fetcher: &mut Fetcher,
    wiki: &MediaWiki,
    spec: &Wiki,
    starts: &[String],
) -> Result<(Vec<Listed>, usize)> {
    let mut pages: Vec<Listed> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut categories: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(String, usize)> = starts
        .iter()
        .filter(|t| is_category(t))
        .map(|t| (t.clone(), 0))
        .collect();
    while let Some((category, depth)) = queue.pop_front() {
        check(sink)?;
        if spec.exclude.iter().any(|e| same_category(e, &category))
            || !categories.insert(category.clone())
        {
            continue;
        }
        let members = wiki
            .members(fetcher, &category)
            .with_context(|| alloc::format!("listing {category}"))?;
        let (subs, articles): (Vec<Listed>, Vec<Listed>) =
            members.into_iter().partition(|m| m.ns == CATEGORY_NS);
        sink.note(&alloc::format!(
            "{category}: {} pages, {} subcategories{}",
            articles.len(),
            subs.len(),
            if depth < spec.depth || subs.is_empty() {
                ""
            } else {
                " (not followed: depth limit)"
            }
        ));
        if depth < spec.depth {
            queue.extend(subs.into_iter().map(|s| (s.title, depth + 1)));
        }
        for a in articles {
            if seen.insert(a.title.clone()) {
                pages.push(a);
            }
        }
    }
    let plain: Vec<String> = starts.iter().filter(|t| !is_category(t)).cloned().collect();
    let mut more = plain.clone();
    if !spec.link_match.is_empty() {
        for page in &plain {
            check(sink)?;
            let links = wiki.links(fetcher, page)?;
            let total = links.len();
            let kept: Vec<String> = links
                .into_iter()
                .filter(|t| matches_any(t, &spec.link_match))
                .collect();
            sink.note(&alloc::format!(
                "{page}: {} of its {total} links match",
                kept.len()
            ));
            more.extend(kept);
        }
    }
    more.retain(|t| !seen.contains(t));
    more.dedup();
    for l in wiki.latest(fetcher, &more)? {
        if l.ns == 0 && seen.insert(l.title.clone()) {
            pages.push(l);
        }
    }
    Ok((pages, categories.len()))
}

/// Notes titles a few per line, after a label
fn note_titles(sink: &mut dyn Sink, label: &str, titles: &[&str]) {
    for (i, group) in titles.chunks(TITLES_PER_LINE).enumerate() {
        let head = if i == 0 { label } else { "" };
        sink.note(&alloc::format!("{head:>10} {}", group.join(" · ")));
    }
}

/// Import a MediaWiki topic; returns the number of documents added
pub fn wiki(sink: &mut dyn Sink, spec: &Wiki, meta: &Meta) -> Result<usize> {
    let mut fetcher = Fetcher::new(delay(spec.delay_s));
    wiki_with(sink, &mut fetcher, spec, meta)
}

/// [`wiki`] through a given fetcher
pub fn wiki_with(
    sink: &mut dyn Sink,
    fetcher: &mut Fetcher,
    spec: &Wiki,
    meta: &Meta,
) -> Result<usize> {
    let starts = spec
        .start
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(start_title)
        .collect::<Result<Vec<_>>>()?;
    if starts.is_empty() {
        bail!("give a start page or category");
    }
    let api = match &spec.api {
        Some(api) if !api.trim().is_empty() => String::from(api.trim()),
        _ => {
            let Some(origin) = spec.start.iter().find_map(|s| origin(s.trim())) else {
                bail!("give the wiki's api.php, or a start page's address");
            };
            alloc::format!("{origin}/w/api.php")
        }
    };
    let wiki = MediaWiki { api: api.clone() };
    let info = wiki.site_info(fetcher)?;
    sink.note(&alloc::format!(
        "{} ({api}), license: {}",
        info.name,
        info.license.as_deref().unwrap_or("not given")
    ));
    let (pages, categories) = scope(sink, fetcher, &wiki, spec, &starts)?;
    let mut refused = Vec::new();
    let mut todo = Vec::new();
    let (mut new, mut changed, mut unchanged) = (Vec::new(), Vec::new(), 0);
    for p in &pages {
        let url = info.page_url(&p.title);
        if !fetcher.allowed(&url)? {
            refused.push(p.title.as_str());
            continue;
        }
        // A document stored without a revision (imported before) counts
        // as current
        let stored = sink.revision(&url);
        let has = stored.is_some() || sink.has(&url);
        let current = stored.map_or(has, |r| r == p.revision);
        if current && !meta.refresh {
            unchanged += 1;
            continue;
        }
        if has {
            changed.push(p.title.as_str());
        } else {
            new.push(p.title.as_str());
        }
        todo.push((p, url));
    }
    if !refused.is_empty() {
        note_titles(sink, "robots.txt disallows", &refused);
    }
    // The limit is on pages fetched: stored pages do not count, so the
    // next run takes the next ones
    let waiting = todo.len();
    if waiting > spec.max_pages {
        sink.note(&alloc::format!(
            "{waiting} pages to fetch; the first {} this run, the next run continues with the rest",
            spec.max_pages
        ));
        todo.truncate(spec.max_pages);
    }
    let pace = fetcher.pace(&api)?;
    if spec.dry_run {
        note_titles(sink, "new", &new);
        note_titles(sink, "changed", &changed);
    }
    let summary = alloc::format!(
        "{} pages in scope ({categories} categories): {} new, {} changed, {unchanged} unchanged; fetching {} takes about {} (one request every {:.1} s)",
        pages.len() - refused.len(),
        new.len(),
        changed.len(),
        todo.len(),
        duration_text(pace * todo.len() as u32),
        pace.as_secs_f32()
    );
    sink.note(&summary);
    if spec.dry_run {
        return Ok(0);
    }
    let raw = sink.raw_dir("wiki");
    let mut added = 0;
    for (i, (listed, url)) in todo.iter().enumerate() {
        check(sink)?;
        sink.progress(i, todo.len());
        let page = match wiki.page(fetcher, &listed.title) {
            Ok(p) => p,
            Err(e) => {
                sink.note(&alloc::format!("skipped {}: {e:#}", listed.title));
                continue;
            }
        };
        save_raw(&raw, url, "html", page.html.as_bytes())?;
        let text = html::fragment_text(&page.html);
        if text.chars().count() < 40 {
            sink.note(&alloc::format!("skipped {}: no text", listed.title));
            continue;
        }
        let mut doc = Document::new(SourceKind::Wiki, url, page.title, text);
        doc.url = Some(url.clone());
        doc.license = info.license.clone();
        doc.attribution = Some(alloc::format!("{} contributors", info.name));
        doc.language = info.language.clone().or(doc.language);
        doc.revision = Some(if page.revision > 0 {
            page.revision
        } else {
            listed.revision
        });
        add(sink, doc, meta)?;
        added += 1;
    }
    Ok(added)
}

/// Extensions of files that are not pages
const ASSETS: [&str; 22] = [
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "bmp", "avif", "css", "js", "mjs", "json",
    "xml", "woff", "woff2", "ttf", "otf", "mp4", "webm", "mp3", "zip",
];

/// The path of an address (`/a/b`, without query), `/` when it has none
fn path_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    rest.find('/').map_or("/", |i| &rest[i..])
}

/// Whether an address is a page worth visiting: not an asset, not under a
/// skipped prefix
fn wanted(url: &str, skip: &[String]) -> bool {
    let path = path_of(url);
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase());
    if ext.is_some_and(|e| ASSETS.contains(&e.as_str())) {
        return false;
    }
    !skip
        .iter()
        .map(|s| s.trim())
        .any(|s| !s.is_empty() && path.starts_with(s))
}

/// Removes `.` and `..` segments of a path
fn normalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if joined.starts_with('/') {
        joined
    } else {
        alloc::format!("/{joined}")
    }
}

/// The address a link on page `base` points to, without its fragment;
/// `None` for links that are not http(s) or only point within the page
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let href = href.split('#').next().unwrap_or_default();
    if href.is_empty() {
        return None;
    }
    let origin = origin(base)?;
    if let Some((scheme, _)) = href.split_once(':')
        && !scheme.contains(['/', '?'])
    {
        return (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
            .then(|| String::from(href));
    }
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = origin.split("://").next().unwrap_or("https");
        return Some(alloc::format!("{scheme}://{rest}"));
    }
    let (href_path, query) = match href.find('?') {
        Some(i) => (&href[..i], &href[i..]),
        None => (href, ""),
    };
    let base_path = path_of(base);
    let path = if href_path.is_empty() {
        String::from(base_path)
    } else if href_path.starts_with('/') {
        normalize_path(href_path)
    } else {
        let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
        normalize_path(&alloc::format!("{dir}{href_path}"))
    };
    Some(alloc::format!("{origin}{path}{query}"))
}

/// The addresses a page links to (`<a href>`), resolved against `base`
pub fn links_in(page: &str, base: &str) -> Vec<String> {
    let doc = Html::parse_document(page);
    let a = Selector::parse("a[href]").expect("valid selector");
    doc.select(&a)
        .filter_map(|e| resolve(base, e.value().attr("href")?))
        .collect()
}

/// A crawled address, normalised: an empty path as `/`
fn normalize(url: &str) -> Option<String> {
    let url = url.split('#').next()?;
    let origin = origin(url)?;
    let rest = &url[origin.len()..];
    Some(if rest.is_empty() || rest.starts_with('?') {
        alloc::format!("{origin}/{rest}")
    } else {
        String::from(url)
    })
}

/// The section of an address: its first path segment (`/schedule/`), or
/// `/` for pages at the top
fn section(url: &str) -> String {
    let path = path_of(url);
    match path[1..].find('/') {
        Some(i) => String::from(&path[..i + 2]),
        None => String::from("/"),
    }
}

/// The page addresses of a site's sitemaps (those `robots.txt` names, else
/// `/sitemap.xml` when there is one)
fn sitemap_pages(fetcher: &mut Fetcher, start: &str, origin: &str) -> Result<Vec<String>> {
    let mut queue = fetcher.sitemaps(start)?;
    if queue.is_empty() {
        let guess = alloc::format!("{origin}/sitemap.xml");
        if fetcher.allowed(&guess)? {
            let got = fetcher.fetch(&guess)?;
            if got.ok() && got.text().contains("<loc>") {
                queue.push(guess);
            }
        }
    }
    let mut pages = Vec::new();
    let mut read = 0;
    while let Some(s) = queue.pop() {
        if read >= MAX_SITEMAPS {
            break;
        }
        read += 1;
        let Ok(xml) = fetcher.get(&s) else { continue };
        let (p, nested) = sitemap_locs(&xml);
        pages.extend(p);
        queue.extend(nested);
    }
    Ok(pages)
}

/// Crawl a site; returns the number of documents added
pub fn site(sink: &mut dyn Sink, spec: &Site, meta: &Meta) -> Result<usize> {
    let mut fetcher = Fetcher::new(delay(spec.delay_s));
    site_with(sink, &mut fetcher, spec, meta)
}

/// [`site`] through a given fetcher
pub fn site_with(
    sink: &mut dyn Sink,
    fetcher: &mut Fetcher,
    spec: &Site,
    meta: &Meta,
) -> Result<usize> {
    let start = normalize(spec.start.trim()).context("give the site's http(s) address")?;
    let home = String::from(origin(&start).unwrap_or_default());
    if !fetcher.allowed(&start)? {
        bail!("robots.txt disallows {start}");
    }
    let pace = fetcher.pace(&start)?;
    let from_sitemaps: Vec<String> = sitemap_pages(fetcher, &start, &home)?
        .iter()
        .filter_map(|u| normalize(u))
        .filter(|u| u.starts_with(&alloc::format!("{home}/")) && wanted(u, &spec.skip))
        .collect();
    sink.note(&alloc::format!(
        "{home}: one request every {:.1} s; {}",
        pace.as_secs_f32(),
        if from_sitemaps.is_empty() {
            String::from("no sitemap, pages found through links")
        } else {
            alloc::format!("{} pages in its sitemaps", from_sitemaps.len())
        }
    ));
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for u in core::iter::once(start.clone()).chain(from_sitemaps.iter().cloned()) {
        if seen.insert(u.clone()) {
            queue.push_back(u);
        }
    }
    // With a sitemap a dry run needs no page
    if spec.dry_run && !from_sitemaps.is_empty() {
        let mut sections: BTreeMap<String, usize> = BTreeMap::new();
        for u in &queue {
            *sections.entry(section(u)).or_default() += 1;
        }
        let pages = queue.len().min(spec.max_pages);
        note_sections(sink, &sections);
        sink.note(&alloc::format!(
            "{} pages in the sitemaps; fetching {pages} takes about {}",
            queue.len(),
            duration_text(pace * pages as u32)
        ));
        return Ok(0);
    }
    let raw = sink.raw_dir("web");
    let mut sections: BTreeMap<String, usize> = BTreeMap::new();
    let (mut visited, mut added, mut kept, mut fetched, mut refused) = (0, 0, 0, 0, 0);
    while let Some(url) = queue.pop_front() {
        check(sink)?;
        if !fetcher.allowed(&url)? {
            refused += 1;
            continue;
        }
        let stored = !meta.refresh && sink.has(&url);
        // A page fetched before is read from its raw copy, stored or not
        // (the shell of an app, or a run stopped before storing it)
        let copy = raw.join(alloc::format!("{}.html", doc_id(&url)));
        let kept_copy = (!meta.refresh)
            .then(|| std::fs::read_to_string(&copy).ok())
            .flatten();
        if kept_copy.is_none() && fetched >= spec.max_pages {
            queue.push_front(url);
            break;
        }
        sink.progress(visited, visited + queue.len() + 1);
        visited += 1;
        let (page, base) = match kept_copy {
            Some(page) => (page, url.clone()),
            None => {
                fetched += 1;
                let got = match fetcher.fetch(&url) {
                    Ok(got) => got,
                    Err(e) => {
                        sink.note(&alloc::format!("skipped {url}: {e:#}"));
                        continue;
                    }
                };
                if !got.ok() {
                    sink.note(&alloc::format!("skipped {url}: HTTP {}", got.status));
                    continue;
                }
                if !got.is_html() {
                    sink.note(&alloc::format!("skipped {url}: not a page"));
                    continue;
                }
                if origin(&got.final_url) != Some(home.as_str()) {
                    sink.note(&alloc::format!(
                        "skipped {url}: leads to another site ({})",
                        got.final_url
                    ));
                    continue;
                }
                let page = got.text();
                if !spec.dry_run {
                    save_raw(&raw, &url, "html", page.as_bytes())?;
                }
                (page, got.final_url)
            }
        };
        *sections.entry(section(&url)).or_default() += 1;
        for link in links_in(&page, &base) {
            let Some(link) = normalize(&link) else {
                continue;
            };
            if link.starts_with(&alloc::format!("{home}/"))
                && wanted(&link, &spec.skip)
                && seen.insert(link.clone())
            {
                // robots.txt is read already: this asks nothing
                if fetcher.allowed(&link)? {
                    queue.push_back(link);
                } else {
                    refused += 1;
                }
            }
        }
        if stored {
            kept += 1;
            continue;
        }
        let converted = html::convert(&page);
        if let Some(why) = html::shell_reason(page.len(), &converted.text) {
            sink.note(&alloc::format!("not kept {url}: {why}"));
            continue;
        }
        if spec.dry_run {
            sink.note(&alloc::format!(
                "{url}: {} ({} characters)",
                converted.title.as_deref().unwrap_or("untitled"),
                converted.text.chars().count()
            ));
            continue;
        }
        let title = converted.title.clone().unwrap_or_else(|| url.clone());
        let mut doc = Document::new(SourceKind::Web, &url, title, converted.text);
        doc.url = Some(url.clone());
        doc.license = converted.license;
        doc.attribution = converted
            .site_name
            .or_else(|| Some(String::from(home.split("://").nth(1).unwrap_or(&home))));
        doc.language = converted
            .language
            .or_else(|| guess_language(&doc.text).map(String::from));
        add(sink, doc, meta)?;
        added += 1;
    }
    note_sections(sink, &sections);
    let left = queue.len();
    sink.note(&alloc::format!(
        "{visited} pages visited ({fetched} fetched, {kept} already stored){}{}",
        if refused > 0 {
            alloc::format!("; {refused} links robots.txt disallows")
        } else {
            String::new()
        },
        if left > 0 {
            alloc::format!("; {left} more found, not visited (page limit)")
        } else {
            String::new()
        }
    ));
    Ok(added)
}

/// Notes the pages per section
fn note_sections(sink: &mut dyn Sink, sections: &BTreeMap<String, usize>) {
    let list: Vec<String> = sections
        .iter()
        .map(|(s, n)| alloc::format!("{s} {n}"))
        .collect();
    sink.note(&alloc::format!("sections: {}", list.join(", ")));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crawl::{Fetched, Http};
    use alloc::rc::Rc;
    use core::cell::RefCell;
    use std::path::PathBuf;

    /// What the fake server saw: urls asked and waits
    #[derive(Default)]
    struct Seen {
        urls: Vec<String>,
        sleeps: Vec<Duration>,
    }

    /// A fake server answering from a function of the url
    struct Fake {
        answer: fn(&str) -> (u16, &'static str, String),
        seen: Rc<RefCell<Seen>>,
    }

    impl Http for Fake {
        fn get(&mut self, url: &str) -> Result<Fetched> {
            self.seen.borrow_mut().urls.push(String::from(url));
            let (status, content_type, body) = (self.answer)(url);
            Ok(Fetched {
                status,
                content_type: String::from(content_type),
                file_name: None,
                etag: None,
                final_url: String::from(url),
                body: body.into_bytes(),
            })
        }
        fn sleep(&mut self, d: Duration) {
            self.seen.borrow_mut().sleeps.push(d);
        }
    }

    fn fetcher(answer: fn(&str) -> (u16, &'static str, String)) -> (Fetcher, Rc<RefCell<Seen>>) {
        let seen = Rc::new(RefCell::new(Seen::default()));
        let fake = Fake {
            answer,
            seen: Rc::clone(&seen),
        };
        (
            Fetcher::with_http(Box::new(fake), Duration::from_secs(1)),
            seen,
        )
    }

    /// Keeps documents in memory
    #[derive(Default)]
    struct Memory {
        docs: Vec<Document>,
        notes: Vec<String>,
        root: PathBuf,
    }

    impl Sink for Memory {
        fn has(&self, key: &str) -> bool {
            self.docs.iter().any(|d| d.id == doc_id(key))
        }
        fn revision(&self, key: &str) -> Option<u64> {
            self.docs
                .iter()
                .find(|d| d.id == doc_id(key))
                .and_then(|d| d.revision)
        }
        fn add(&mut self, doc: &Document) -> Result<usize> {
            self.docs.retain(|d| d.id != doc.id);
            self.docs.push(doc.clone());
            Ok(1)
        }
        fn raw_dir(&self, kind: &str) -> PathBuf {
            self.root.join(kind)
        }
        fn note(&mut self, line: &str) {
            self.notes.push(String::from(line));
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-wiki-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn json(v: serde_json::Value) -> (u16, &'static str, String) {
        (200, "application/json", v.to_string())
    }

    /// A value of a query parameter, decoded
    fn param(url: &str, key: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        query.split('&').find_map(|p| {
            let (k, v) = p.split_once('=')?;
            (k == key).then(|| crate::crawl::decode(v).unwrap_or_default())
        })
    }

    /// Lagging once, for the first page fetched
    static LAGGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    /// A database error once, for Steelhead
    static BUSY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

    /// Tides' latest revision
    static TIDES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(11);

    fn revision_of(title: &str) -> u64 {
        match title {
            "Tides" => TIDES.load(core::sync::atomic::Ordering::Relaxed),
            _ => title.len() as u64,
        }
    }

    fn page(title: &str, ns: i64) -> serde_json::Value {
        serde_json::json!({"title": title, "ns": ns, "lastrevid": revision_of(title)})
    }

    /// A small wiki: Category:Salmon Run holds Eggs, Tides (on a second
    /// page of the listing) and Category:Bosses, which holds Steelhead and
    /// Category:Deep, which holds Deep page; the Salmon Run article links
    /// to Grizzco, Splatfest and Secret plans, which robots.txt forbids
    fn wiki_server(url: &str) -> (u16, &'static str, String) {
        if url.ends_with("/robots.txt") {
            return (
                200,
                "text/plain",
                String::from(
                    "User-agent: ClaudeBot\nDisallow: /\n\nUser-agent: *\nDisallow: /wiki/Secret\nCrawl-delay: 3\n",
                ),
            );
        }
        assert!(url.starts_with("https://w.test/w/api.php?"), "{url}");
        assert_eq!(param(url, "maxlag").as_deref(), Some("5"));
        let action = param(url, "action").unwrap_or_default();
        if action == "parse" {
            let title = param(url, "page").unwrap();
            if title == "Eggs" && !LAGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
                return json(serde_json::json!({"error": {"code": "maxlag", "lag": 7.0}}));
            }
            if title == "Steelhead" && !BUSY.swap(true, core::sync::atomic::Ordering::Relaxed) {
                return json(
                    serde_json::json!({"error": {"code": "internal_api_error_DBConnectionError"}}),
                );
            }
            return json(serde_json::json!({"parse": {
                "title": title,
                "revid": revision_of(&title),
                "text": format!("<div class=\"mw-parser-output\"><p>{title} is about Salmon Run and the golden eggs.</p><h2>Tips<span class=\"mw-editsection\">edit</span></h2><p>Bank early.</p></div>")
            }}));
        }
        if param(url, "meta").is_some() {
            return json(serde_json::json!({"query": {
                "general": {"sitename": "Inkipedia", "lang": "en", "server": "https://w.test", "articlepath": "/wiki/$1"},
                "rightsinfo": {"text": "CC BY-NC-SA 4.0", "url": "https://creativecommons.org/licenses/by-nc-sa/4.0/"}
            }}));
        }
        if let Some(category) = param(url, "gcmtitle") {
            let pages = match (category.as_str(), param(url, "gcmcontinue")) {
                ("Category:Salmon Run", None) => {
                    return json(serde_json::json!({
                        "continue": {"gcmcontinue": "page|54494445|1", "continue": "gcmcontinue||"},
                        "query": {"pages": [page("Eggs", 0), page("Category:Bosses", 14)]}
                    }));
                }
                ("Category:Salmon Run", Some(c)) => {
                    assert_eq!(c, "page|54494445|1");
                    assert_eq!(param(url, "continue").as_deref(), Some("gcmcontinue||"));
                    serde_json::json!([page("Tides", 0), page("Eggs", 0)])
                }
                ("Category:Bosses", None) => {
                    serde_json::json!([page("Steelhead", 0), page("Category:Deep", 14)])
                }
                other => panic!("not asked for: {other:?}"),
            };
            return json(serde_json::json!({"query": {"pages": pages}}));
        }
        if param(url, "prop").as_deref() == Some("links") {
            if param(url, "titles").as_deref() != Some("Salmon Run") {
                return json(serde_json::json!({"query": {"pages": []}}));
            }
            return json(
                serde_json::json!({"query": {"pages": [{"title": "Salmon Run", "links": [
                    {"ns": 0, "title": "Grizzco"}, {"ns": 0, "title": "Splatfest"}, {"ns": 0, "title": "Secret plans"}
                ]}]}}),
            );
        }
        if param(url, "prop").as_deref() == Some("info") {
            let titles = param(url, "titles").unwrap();
            let pages: Vec<serde_json::Value> = titles
                .split('|')
                .map(|t| {
                    if t == "Nowhere" {
                        serde_json::json!({"title": t, "ns": 0, "missing": true})
                    } else {
                        page(t, 0)
                    }
                })
                .collect();
            return json(serde_json::json!({"query": {"pages": pages}}));
        }
        panic!("unexpected request {url}")
    }

    fn spec() -> Wiki {
        Wiki {
            start: alloc::vec![
                String::from("Category:Salmon Run"),
                String::from("https://w.test/wiki/Salmon_Run"),
                String::from("Nowhere"),
            ],
            depth: 1,
            link_match: alloc::vec![String::from("grizzco"), String::from("Secret")],
            ..Wiki::default()
        }
    }

    fn parsed(seen: &Rc<RefCell<Seen>>) -> Vec<String> {
        seen.borrow()
            .urls
            .iter()
            .filter(|u| param(u, "action").as_deref() == Some("parse"))
            .map(|u| param(u, "page").unwrap())
            .collect()
    }

    #[test]
    fn a_capped_topic_continues_on_the_next_run() {
        let mut sink = Memory {
            root: temp("topic-capped"),
            ..Default::default()
        };
        // The start page and the page it links to (not the pages the
        // other test's server fails once)
        let one = Wiki {
            start: alloc::vec![String::from("https://w.test/wiki/Salmon_Run")],
            link_match: alloc::vec![String::from("grizzco")],
            max_pages: 1,
            ..Wiki::default()
        };
        let (mut f, first) = fetcher(wiki_server);
        assert_eq!(
            wiki_with(&mut sink, &mut f, &one, &Meta::default()).unwrap(),
            1
        );
        assert!(sink.notes.iter().any(|n| n.starts_with(
            "2 pages to fetch; the first 1 this run, the next run continues with the rest"
        )));
        let (mut f, second) = fetcher(wiki_server);
        assert_eq!(
            wiki_with(&mut sink, &mut f, &one, &Meta::default()).unwrap(),
            1
        );
        assert!(
            sink.notes
                .iter()
                .any(|n| n.contains("1 new, 0 changed, 1 unchanged")),
            "{:#?}",
            sink.notes
        );
        let (first, second) = (parsed(&first), parsed(&second));
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_ne!(first, second);
        std::fs::remove_dir_all(&sink.root).unwrap();
    }

    #[test]
    fn imports_a_wiki_topic_and_then_only_what_changed() {
        let mut sink = Memory {
            root: temp("topic"),
            ..Default::default()
        };
        // A dry run lists and counts, and fetches no page
        let (mut f, seen) = fetcher(wiki_server);
        let dry = Wiki {
            dry_run: true,
            ..spec()
        };
        assert_eq!(
            wiki_with(&mut sink, &mut f, &dry, &Meta::default()).unwrap(),
            0
        );
        assert!(parsed(&seen).is_empty());
        let summary = sink.notes.last().unwrap();
        assert!(
            summary.starts_with("5 pages in scope (2 categories): 5 new, 0 changed, 0 unchanged; fetching 5 takes about 15 s"),
            "{summary}"
        );
        assert!(
            sink.notes
                .iter()
                .any(|n| n.contains("robots.txt disallows") && n.contains("Secret plans"))
        );
        assert!(
            sink.notes
                .iter()
                .any(|n| n.contains("Category:Bosses: 1 pages, 1 subcategories (not followed"))
        );
        // Category:Deep is below the depth limit: never listed
        assert!(!seen.borrow().urls.iter().any(|u| u.contains("Deep")));
        // Every wait is the wiki's Crawl-delay, never less
        assert!(seen.borrow().sleeps.iter().all(|d| d.as_secs_f32() > 2.5));
        assert!(sink.docs.is_empty());
        // A category left out is not listed
        let (mut f, seen) = fetcher(wiki_server);
        let without = Wiki {
            exclude: alloc::vec![String::from("category:bosses")],
            ..dry.clone()
        };
        wiki_with(&mut sink, &mut f, &without, &Meta::default()).unwrap();
        assert!(!seen.borrow().urls.iter().any(|u| u.contains("Bosses")));
        assert!(
            sink.notes
                .last()
                .unwrap()
                .starts_with("4 pages in scope (1 categories)")
        );

        let (mut f, seen) = fetcher(wiki_server);
        let n = wiki_with(&mut sink, &mut f, &spec(), &Meta::default()).unwrap();
        assert_eq!(n, 5);
        assert_eq!(
            parsed(&seen),
            [
                "Eggs",
                "Eggs",
                "Tides",
                "Steelhead",
                "Steelhead",
                "Salmon Run",
                "Grizzco"
            ]
        );
        // The wiki lagged once, and its database failed once: waited (as
        // it asked, or half a minute), then asked again
        assert!(seen.borrow().sleeps.contains(&Duration::from_secs(7)));
        assert!(seen.borrow().sleeps.contains(&Duration::from_secs(30)));
        let eggs = sink.docs.iter().find(|d| d.title == "Eggs").unwrap();
        assert_eq!(eggs.url.as_deref(), Some("https://w.test/wiki/Eggs"));
        assert_eq!(eggs.source, SourceKind::Wiki);
        assert_eq!(eggs.revision, Some(4));
        assert_eq!(eggs.language.as_deref(), Some("en"));
        assert_eq!(eggs.attribution.as_deref(), Some("Inkipedia contributors"));
        assert!(
            eggs.license
                .as_deref()
                .unwrap()
                .starts_with("CC BY-NC-SA 4.0")
        );
        assert!(
            eggs.text.contains("\n## Tips\n\nBank early."),
            "{}",
            eggs.text
        );
        assert!(!eggs.text.contains("edit"));
        let run = sink.docs.iter().find(|d| d.title == "Salmon Run").unwrap();
        assert_eq!(run.url.as_deref(), Some("https://w.test/wiki/Salmon_Run"));
        assert!(
            sink.root
                .join("wiki")
                .join(format!("{}.html", eggs.id))
                .is_file()
        );

        // Nothing changed: nothing fetched
        let (mut f, seen) = fetcher(wiki_server);
        assert_eq!(
            wiki_with(&mut sink, &mut f, &spec(), &Meta::default()).unwrap(),
            0
        );
        assert!(parsed(&seen).is_empty());
        // Tides was edited: only Tides again
        TIDES.store(12, core::sync::atomic::Ordering::Relaxed);
        let (mut f, seen) = fetcher(wiki_server);
        assert_eq!(
            wiki_with(&mut sink, &mut f, &spec(), &Meta::default()).unwrap(),
            1
        );
        assert_eq!(parsed(&seen), ["Tides"]);
        assert!(
            sink.notes
                .iter()
                .any(|n| n.contains("1 changed, 4 unchanged"))
        );
        let tides = sink.docs.iter().find(|d| d.title == "Tides").unwrap();
        assert_eq!(tides.revision, Some(12));
        assert_eq!(sink.docs.len(), 5);
        std::fs::remove_dir_all(&sink.root).unwrap();
    }

    #[test]
    fn a_failing_robots_txt_stops_the_import() {
        let (mut f, seen) = fetcher(|url| {
            if url.ends_with("/robots.txt") {
                (503, "text/plain", String::new())
            } else {
                panic!("asked {url} without robots.txt")
            }
        });
        let mut sink = Memory::default();
        let err = wiki_with(&mut sink, &mut f, &spec(), &Meta::default()).unwrap_err();
        assert!(format!("{err:#}").contains("HTTP 503"), "{err:#}");
        assert_eq!(seen.borrow().urls.len(), 1);
    }

    #[test]
    fn starts_are_titles_or_addresses() {
        assert_eq!(
            start_title("https://splatoonwiki.org/wiki/Salmon_Run_Next_Wave#Bosses").unwrap(),
            "Salmon Run Next Wave"
        );
        assert_eq!(
            start_title("https://w.test/wiki/Category:Salmon_Run%3F").unwrap(),
            "Category:Salmon Run?"
        );
        assert_eq!(start_title(" Category:Bosses ").unwrap(), "Category:Bosses");
        assert!(start_title("https://w.test/index.php?title=X").is_err());
        assert!(is_category("category:x") && !is_category("Cat"));
    }

    #[test]
    fn resolves_links() {
        let base = "https://s.test/dir/page/?q=1";
        assert_eq!(resolve(base, "/a").as_deref(), Some("https://s.test/a"));
        assert_eq!(
            resolve(base, "b/c#x").as_deref(),
            Some("https://s.test/dir/page/b/c")
        );
        assert_eq!(
            resolve(base, "../up?x=2").as_deref(),
            Some("https://s.test/dir/up?x=2")
        );
        assert_eq!(
            resolve(base, "?p=2").as_deref(),
            Some("https://s.test/dir/page/?p=2")
        );
        assert_eq!(
            resolve(base, "//cdn.test/x.js").as_deref(),
            Some("https://cdn.test/x.js")
        );
        assert_eq!(
            resolve(base, "https://o.test/").as_deref(),
            Some("https://o.test/")
        );
        assert_eq!(resolve(base, "#top"), None);
        assert_eq!(resolve(base, "javascript:void(0);"), None);
        assert_eq!(resolve(base, "mailto:a@b"), None);
        assert_eq!(
            normalize("https://s.test").as_deref(),
            Some("https://s.test/")
        );
        assert_eq!(section("https://s.test/schedule/all/"), "/schedule/");
        assert_eq!(section("https://s.test/"), "/");
        assert!(!wanted("https://s.test/a/b.PNG", &[]));
        assert!(!wanted(
            "https://s.test/map/?stage=1",
            &[String::from("/map/")]
        ));
        assert!(wanted("https://s.test/maplist/", &[String::from("/map/")]));
        assert_eq!(duration_text(Duration::from_secs(7500)), "2 h 5 min");
    }

    /// A small site: the home page links to pages, assets, a skipped
    /// section, a page robots.txt forbids, another site and a script shell
    fn site_server(url: &str) -> (u16, &'static str, String) {
        let html = |body: &str| {
            (
                200,
                "text/html; charset=utf-8",
                format!(
                    "<html lang=ja><head><title>{}</title><meta property=og:site_name content=\"Salmon Learn\"></head><body><main>{body}</main></body></html>",
                    url.rsplit('/').nth(1).unwrap_or("home")
                ),
            )
        };
        let text = "Golden eggs and the tides, told at length for new players. ".repeat(3);
        match url {
            "https://s.test/robots.txt" => (
                200,
                "text/plain",
                String::from("User-agent: *\nDisallow: /private\n"),
            ),
            "https://s.test/sitemap.xml" => (404, "text/html", String::new()),
            "https://s.test/" => html(&format!(
                "<p>{text}</p><a href=/guide/>Guide</a> <a href=\"guide/eggs/\">Eggs</a> <a href=/private/x>x</a> <a href=/map/?stage=1>map</a> <a href=/img/a.png>a</a> <a href=https://other.test/>o</a> <a href=/app/>app</a> <a href=#top>top</a>"
            )),
            "https://s.test/guide/" => html(&format!(
                "<h2>Guide</h2><p>{text}</p><a href=../>home</a> <a href=eggs/>eggs</a>"
            )),
            "https://s.test/guide/eggs/" => html(&format!("<h2>Eggs</h2><p>{text}</p>")),
            "https://s.test/app/" => html("<p>Please enable JavaScript to use this app.</p>"),
            other => panic!("unexpected request {other}"),
        }
    }

    #[test]
    fn crawls_a_site_politely_and_again_from_raw_copies() {
        let mut sink = Memory {
            root: temp("site"),
            ..Default::default()
        };
        let spec = Site {
            start: String::from("https://s.test"),
            skip: alloc::vec![String::from("/map/")],
            max_pages: 10,
            ..Site::default()
        };
        let (mut f, seen) = fetcher(site_server);
        let n = site_with(&mut sink, &mut f, &spec, &Meta::default()).unwrap();
        assert_eq!(n, 3, "{:#?}", sink.notes);
        let asked = seen.borrow().urls.clone();
        assert_eq!(
            asked,
            [
                "https://s.test/robots.txt",
                "https://s.test/sitemap.xml",
                "https://s.test/",
                "https://s.test/guide/",
                "https://s.test/guide/eggs/",
                "https://s.test/app/"
            ]
        );
        let eggs = sink.docs.iter().find(|d| d.title == "eggs").unwrap();
        assert_eq!(eggs.url.as_deref(), Some("https://s.test/guide/eggs/"));
        assert_eq!(eggs.source, SourceKind::Web);
        assert_eq!(eggs.attribution.as_deref(), Some("Salmon Learn"));
        assert_eq!(eggs.language.as_deref(), Some("ja"));
        assert!(eggs.text.starts_with("## Eggs"));
        assert!(
            sink.notes
                .iter()
                .any(|n| n.starts_with("not kept https://s.test/app/"))
        );
        assert!(
            sink.notes
                .iter()
                .any(|n| n == "sections: / 1, /app/ 1, /guide/ 2")
        );
        assert_eq!(seen.borrow().sleeps.len(), 5);

        // Again: every page is read from its raw copy, the one not kept too
        let (mut f, seen) = fetcher(site_server);
        assert_eq!(
            site_with(&mut sink, &mut f, &spec, &Meta::default()).unwrap(),
            0
        );
        let asked = seen.borrow().urls.clone();
        assert_eq!(
            asked,
            ["https://s.test/robots.txt", "https://s.test/sitemap.xml"]
        );
        assert!(
            sink.notes
                .last()
                .unwrap()
                .contains("4 pages visited (0 fetched, 3 already stored)")
        );

        // A capped crawl: the next run goes on past what the last fetched
        let mut capped = Memory {
            root: temp("site-capped"),
            ..Default::default()
        };
        let two = Site {
            max_pages: 2,
            ..spec.clone()
        };
        let (mut f, _) = fetcher(site_server);
        assert_eq!(
            site_with(&mut capped, &mut f, &two, &Meta::default()).unwrap(),
            2
        );
        let (mut f, seen) = fetcher(site_server);
        assert_eq!(
            site_with(&mut capped, &mut f, &two, &Meta::default()).unwrap(),
            1
        );
        assert_eq!(
            seen.borrow().urls[2..],
            ["https://s.test/guide/eggs/", "https://s.test/app/"]
        );
        assert_eq!(capped.docs.len(), 3);
        std::fs::remove_dir_all(&capped.root).unwrap();

        // A dry run on a fresh store keeps nothing; the page cap holds
        let mut fresh = Memory {
            root: temp("site-dry"),
            ..Default::default()
        };
        let (mut f, _) = fetcher(site_server);
        let dry = Site {
            dry_run: true,
            max_pages: 2,
            ..spec
        };
        assert_eq!(
            site_with(&mut fresh, &mut f, &dry, &Meta::default()).unwrap(),
            0
        );
        assert!(fresh.docs.is_empty() && !fresh.root.exists());
        let last = fresh.notes.last().unwrap();
        assert!(
            last.starts_with("2 pages visited (2 fetched, 0 already stored)"),
            "{last}"
        );
        assert!(
            last.ends_with("; 2 more found, not visited (page limit)"),
            "{last}"
        );
        std::fs::remove_dir_all(&sink.root).unwrap();
    }
}
