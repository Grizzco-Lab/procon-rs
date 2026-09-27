//! Polite web fetching.
//!
//! [`Fetcher`] identifies itself ([`user_agent`]), obeys each site's
//! `robots.txt` (rules for `Cuttlefish` or `*`; a `robots.txt` that fails
//! with a server error blocks the site, as RFC 9309 asks) and waits between
//! requests to the same site: the given delay or the site's `Crawl-delay`,
//! whichever is longer. It also reads sitemaps and MediaWiki's API, which
//! lists categories and links with each page's latest revision and returns
//! an article's content without the page chrome. Requests go out through
//! [`Http`], which tests replace with a fake server.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use core::time::Duration;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use texting_robots::{Robot, get_robots_url};
use ureq::ResponseExt;

/// Product token matched against `robots.txt` user-agent lines
pub const ROBOTS_TOKEN: &str = "Cuttlefish";

/// The User-Agent header: product, purpose, and a contact from the
/// `CUTTLEFISH_CONTACT` environment variable when set (a url or an email,
/// so site admins can reach whoever runs the crawl)
pub fn user_agent() -> String {
    let contact = std::env::var("CUTTLEFISH_CONTACT").unwrap_or_default();
    let contact = if contact.is_empty() {
        String::new()
    } else {
        alloc::format!("; contact: {contact}")
    };
    alloc::format!(
        "{ROBOTS_TOKEN}/{} (personal, non-commercial Salmon Run study notes{contact})",
        env!("CARGO_PKG_VERSION")
    )
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(user_agent())
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Downloads a file (streamed, any size) to `dest`
pub fn download(url: &str, dest: &Path) -> Result<()> {
    let mut resp = agent(Duration::from_secs(3600))
        .get(url)
        .call()
        .with_context(|| alloc::format!("GET {url}"))?;
    if !resp.status().is_success() {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp)?;
    std::io::copy(&mut resp.body_mut().as_reader(), &mut file)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// How requests go out: over the network ([`Network`]), or through a fake
/// in tests
pub trait Http {
    /// GET a url, redirects followed; fails only when no answer came
    fn get(&mut self, url: &str) -> Result<Fetched>;
    /// GET a url with `If-None-Match: <etag>`, so a server that still has
    /// the same file answers 304 without a body; a fake may ignore the tag
    fn get_if_none_match(&mut self, url: &str, _etag: &str) -> Result<Fetched> {
        self.get(url)
    }
    /// Waits `d` (a fake only records it)
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Requests over the network, as [`user_agent`]
pub struct Network(ureq::Agent);

impl Network {
    fn call(&self, url: &str, etag: Option<&str>) -> Result<Fetched> {
        let mut req = self.0.get(url);
        if let Some(etag) = etag {
            req = req.header("If-None-Match", etag);
        }
        let mut resp = req.call().with_context(|| alloc::format!("GET {url}"))?;
        let header = |name: &str| {
            resp.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(String::from)
        };
        let content_type = header("content-type").unwrap_or_default();
        let file_name = header("content-disposition").and_then(|d| disposition_name(&d));
        let etag = header("etag");
        let final_url = resp.get_uri().to_string();
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(50 * 1024 * 1024)
            .read_to_vec()
            .unwrap_or_default();
        Ok(Fetched {
            status,
            content_type,
            file_name,
            etag,
            final_url,
            body,
        })
    }
}

impl Http for Network {
    fn get(&mut self, url: &str) -> Result<Fetched> {
        self.call(url, None)
    }

    fn get_if_none_match(&mut self, url: &str, etag: &str) -> Result<Fetched> {
        self.call(url, Some(etag))
    }
}

/// Fetches pages politely
pub struct Fetcher {
    http: Box<dyn Http>,
    delay: Duration,
    /// Per robots.txt url (one per site): its rules, `None` when the site
    /// has none
    robots: HashMap<String, Option<Robot>>,
    /// Per site: when the last request went out
    last: HashMap<String, Instant>,
}

impl Fetcher {
    /// A fetcher waiting at least `delay` between requests to one site
    pub fn new(delay: Duration) -> Self {
        Self::with_http(Box::new(Network(agent(Duration::from_secs(60)))), delay)
    }

    /// A fetcher sending its requests through `http`
    pub fn with_http(http: Box<dyn Http>, delay: Duration) -> Self {
        Fetcher {
            http,
            delay,
            robots: HashMap::new(),
            last: HashMap::new(),
        }
    }

    /// The pause between two requests to a site: the given delay, or the
    /// site's `Crawl-delay` (at most two minutes) when longer
    fn pause(&self, crawl_delay: Option<f32>) -> Duration {
        self.delay.max(Duration::from_secs_f32(
            crawl_delay.unwrap_or(0.0).clamp(0.0, 120.0),
        ))
    }

    /// Sleeps until the site may be asked again
    fn wait(&mut self, site: &str, crawl_delay: Option<f32>) {
        let delay = self.pause(crawl_delay);
        if let Some(last) = self.last.get(site) {
            let since = last.elapsed();
            if since < delay {
                self.http.sleep(delay - since);
            }
        }
        self.last.insert(String::from(site), Instant::now());
    }

    /// Waits `d` (a server asked us to come back later)
    pub fn sleep(&mut self, d: Duration) {
        self.http.sleep(d);
    }

    /// GET with the politeness delay, conditional on `etag` when given
    fn get_raw(
        &mut self,
        url: &str,
        site: &str,
        crawl_delay: Option<f32>,
        etag: Option<&str>,
    ) -> Result<Fetched> {
        self.wait(site, crawl_delay);
        match etag {
            Some(etag) => self.http.get_if_none_match(url, etag),
            None => self.http.get(url),
        }
    }

    /// `url`'s site's `robots.txt` url and rules (`None`: it has none)
    fn robot(&mut self, url: &str) -> Result<(String, Option<&Robot>)> {
        let robots_url =
            get_robots_url(url).map_err(|e| anyhow::anyhow!("bad url {url}: {e:?}"))?;
        if !self.robots.contains_key(&robots_url) {
            let got = self.get_raw(&robots_url, &robots_url.clone(), None, None)?;
            let robot = match got.status {
                200..=299 => Some(Robot::new(ROBOTS_TOKEN, &got.body)?),
                400..=499 => None,
                status => bail!("{robots_url}: HTTP {status}; not crawling this site"),
            };
            self.robots.insert(robots_url.clone(), robot);
        }
        let robot = self.robots[&robots_url].as_ref();
        Ok((robots_url, robot))
    }

    /// Whether `robots.txt` lets us fetch `url`, and the site's crawl delay
    fn check_robots(&mut self, url: &str) -> Result<(String, bool, Option<f32>)> {
        let (site, robot) = self.robot(url)?;
        let allowed = robot.is_none_or(|r| r.allowed(url));
        let delay = robot.and_then(|r| r.delay);
        Ok((site, allowed, delay))
    }

    /// Whether `robots.txt` lets us fetch `url`
    pub fn allowed(&mut self, url: &str) -> Result<bool> {
        Ok(self.check_robots(url)?.1)
    }

    /// The pause between two requests to `url`'s site
    pub fn pace(&mut self, url: &str) -> Result<Duration> {
        let (_, _, delay) = self.check_robots(url)?;
        Ok(self.pause(delay))
    }

    /// The sitemaps `url`'s site names in its `robots.txt`
    pub fn sitemaps(&mut self, url: &str) -> Result<Vec<String>> {
        let (_, robot) = self.robot(url)?;
        Ok(robot.map(|r| r.sitemaps.clone()).unwrap_or_default())
    }

    /// Fetches a url whatever the server answers; fails only if
    /// `robots.txt` forbids it or the request does not complete
    pub fn fetch(&mut self, url: &str) -> Result<Fetched> {
        self.fetch_if_changed(url, None)
    }

    /// [`Fetcher::fetch`] with `If-None-Match: <etag>` when an ETag of the
    /// last copy is given: an unchanged file answers 304 with no body
    pub fn fetch_if_changed(&mut self, url: &str, etag: Option<&str>) -> Result<Fetched> {
        let (site, allowed, delay) = self.check_robots(url)?;
        if !allowed {
            bail!("robots.txt disallows {url}");
        }
        self.get_raw(url, &site, delay, etag)
    }

    /// Fetches a page as text; fails if `robots.txt` forbids it or the
    /// server does not answer 2xx
    pub fn get(&mut self, url: &str) -> Result<String> {
        let got = self.fetch(url)?;
        if !got.ok() {
            bail!("GET {url}: HTTP {}", got.status);
        }
        Ok(got.text())
    }
}

/// A server's answer, redirects followed
pub struct Fetched {
    /// HTTP status
    pub status: u16,
    /// `Content-Type`, empty when not given
    pub content_type: String,
    /// The file name `Content-Disposition` gives, if any
    pub file_name: Option<String>,
    /// The `ETag` header, if any: names this copy for
    /// [`Fetcher::fetch_if_changed`]
    pub etag: Option<String>,
    /// The address that answered, after redirects
    pub final_url: String,
    /// The body (at most 50 MB)
    pub body: Vec<u8>,
}

impl Fetched {
    /// Whether the status is 2xx
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether the server answered 304: the copy named by the ETag sent is
    /// still current
    pub fn unchanged(&self) -> bool {
        self.status == 304
    }

    /// The body as text
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Whether the body is an HTML page
    pub fn is_html(&self) -> bool {
        self.content_type.starts_with("text/html")
    }
}

/// The file name of a `Content-Disposition` header: `filename*=UTF-8''...`
/// (percent-decoded) before `filename="..."`
pub fn disposition_name(header: &str) -> Option<String> {
    let param = |name: &str| {
        header.split(';').find_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().trim_matches('"'))
        })
    };
    if let Some(v) = param("filename*") {
        let encoded = v.split_once("''").map_or(v, |(_, rest)| rest);
        if let Some(name) = decode(encoded).filter(|n| !n.is_empty()) {
            return Some(name);
        }
    }
    param("filename")
        .filter(|n| !n.is_empty())
        .map(String::from)
}

/// Percent-decodes UTF-8 text; `None` when it is not valid
pub fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Page urls in a sitemap, and nested sitemaps of a sitemap index (both are
/// `<loc>` elements; nested ones end in `.xml`)
pub fn sitemap_locs(xml: &str) -> (Vec<String>, Vec<String>) {
    let mut pages = Vec::new();
    let mut nested = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<loc>") {
        rest = &rest[start + 5..];
        let Some(end) = rest.find("</loc>") else {
            break;
        };
        let loc = rest[..end].trim().replace("&amp;", "&");
        rest = &rest[end..];
        if loc.ends_with(".xml") || loc.ends_with(".xml.gz") {
            nested.push(loc);
        } else {
            pages.push(loc);
        }
    }
    (pages, nested)
}

/// Percent-encodes a url query value or path segment
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&alloc::format!("%{b:02X}"));
        }
    }
    out
}

/// A MediaWiki site through its `api.php`. Every request carries `maxlag`
/// ([`MAXLAG_S`]): while the wiki's database lags more than that, it
/// answers with an error instead, and the request is sent again after the
/// wait it asks for; likewise after a database error, a read-only wiki or
/// an HTTP 503, half a minute later.
pub struct MediaWiki {
    /// `https://example.org/w/api.php`
    pub api: String,
}

/// Seconds of database lag above which the wiki is left alone for a while
pub const MAXLAG_S: u32 = 5;

/// Times a request is sent while the wiki lags or is busy
const MAXLAG_TRIES: usize = 5;

/// Wait after the wiki's database failed or was read-only (or an HTTP 503)
const BUSY_WAIT: Duration = Duration::from_secs(30);

/// Namespace of categories
pub const CATEGORY_NS: i64 = 14;

/// Titles asked about in one request (the API's limit)
const TITLES_PER_REQUEST: usize = 50;

/// What [`MediaWiki::site_info`] returns
pub struct SiteInfo {
    /// The wiki's name (`Inkipedia`)
    pub name: String,
    /// Language code of its content (`en`)
    pub language: Option<String>,
    /// Content license, for example `CC BY-NC-SA 3.0 (https://...)`
    pub license: Option<String>,
    /// Article url with `$1` for the title
    pub article_url: String,
}

impl SiteInfo {
    /// The reading url of an article
    pub fn page_url(&self, title: &str) -> String {
        let title = encode(&title.replace(' ', "_")).replace("%2F", "/");
        self.article_url.replace("$1", &title)
    }
}

/// A page as a listing gives it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    /// Title, with its namespace (`Category:Salmon Run`)
    pub title: String,
    /// Namespace number (0: articles, [`CATEGORY_NS`])
    pub ns: i64,
    /// Id of its latest revision
    pub revision: u64,
}

/// One article from [`MediaWiki::page`]
pub struct WikiPage {
    /// Article title
    pub title: String,
    /// Rendered content HTML (no navigation or footer)
    pub html: String,
    /// Id of the revision rendered
    pub revision: u64,
}

/// A value of a `continue` object as a query parameter
fn continue_value(v: &serde_json::Value) -> String {
    match v.as_str() {
        Some(s) => encode(s),
        None => encode(&v.to_string()),
    }
}

impl MediaWiki {
    fn query(&self, fetcher: &mut Fetcher, params: &str) -> Result<serde_json::Value> {
        let url = alloc::format!(
            "{}?format=json&formatversion=2&maxlag={MAXLAG_S}&{params}",
            self.api
        );
        let mut last = String::new();
        for _ in 0..MAXLAG_TRIES {
            let got = fetcher.fetch(&url)?;
            let v: serde_json::Value = match serde_json::from_slice(&got.body) {
                Ok(v) => v,
                Err(_) if got.status == 503 => {
                    last = String::from("HTTP 503");
                    fetcher.sleep(BUSY_WAIT);
                    continue;
                }
                Err(_) if !got.ok() => bail!("GET {url}: HTTP {}", got.status),
                Err(e) => return Err(e).context("MediaWiki answer"),
            };
            let code = v["error"]["code"].as_str().unwrap_or_default();
            if code == "maxlag" {
                let lag = v["error"]["lag"].as_f64().unwrap_or(0.0);
                log::info!("the wiki lags {lag:.0} s; waiting");
                last = alloc::format!("its database lags {lag:.0} s");
                fetcher.sleep(Duration::from_secs_f64(lag.clamp(5.0, 60.0)));
            } else if code == "readonly" || code.starts_with("internal_api_error_DB") {
                log::info!("the wiki's database is busy ({code}); waiting");
                last = String::from(code);
                fetcher.sleep(BUSY_WAIT);
            } else if !code.is_empty() {
                bail!("MediaWiki: {}", v["error"]);
            } else if !got.ok() {
                bail!("GET {url}: HTTP {}", got.status);
            } else {
                return Ok(v);
            }
        }
        bail!("the wiki is busy ({last}); try again later")
    }

    /// Every answer of a query, following `continue`
    fn query_all(
        &self,
        fetcher: &mut Fetcher,
        params: &str,
        each: &mut dyn FnMut(&serde_json::Value),
    ) -> Result<()> {
        let mut cont = String::new();
        loop {
            let v = self.query(fetcher, &alloc::format!("{params}{cont}"))?;
            each(&v);
            let Some(next) = v.get("continue").and_then(|c| c.as_object()) else {
                return Ok(());
            };
            cont = next
                .iter()
                .map(|(k, v)| alloc::format!("&{k}={}", continue_value(v)))
                .collect();
        }
    }

    /// The site's name, language, content license and article url pattern
    /// (`siteinfo`)
    pub fn site_info(&self, fetcher: &mut Fetcher) -> Result<SiteInfo> {
        let v = self.query(
            fetcher,
            "action=query&meta=siteinfo&siprop=general%7Crightsinfo",
        )?;
        let info = &v["query"]["rightsinfo"];
        let text = info["text"].as_str().unwrap_or_default();
        let url = info["url"].as_str().unwrap_or_default();
        let license = match (text.is_empty(), url.is_empty()) {
            (true, true) => None,
            (false, true) => Some(String::from(text)),
            (true, false) => Some(String::from(url)),
            (false, false) => Some(alloc::format!("{text} ({url})")),
        };
        let general = &v["query"]["general"];
        let server = general["server"].as_str().unwrap_or_default();
        let server = match server.strip_prefix("//") {
            Some(rest) => alloc::format!("https://{rest}"),
            None => String::from(server),
        };
        let path = general["articlepath"].as_str().unwrap_or("/wiki/$1");
        Ok(SiteInfo {
            name: String::from(general["sitename"].as_str().unwrap_or("the wiki")),
            language: general["lang"].as_str().map(String::from),
            license,
            article_url: alloc::format!("{server}{path}"),
        })
    }

    /// Pages from one `pages` answer
    fn listed(v: &serde_json::Value, out: &mut Vec<Listed>) {
        for p in v["query"]["pages"].as_array().into_iter().flatten() {
            if p.get("missing").is_some() || p.get("invalid").is_some() {
                continue;
            }
            if let Some(title) = p["title"].as_str() {
                out.push(Listed {
                    title: String::from(title),
                    ns: p["ns"].as_i64().unwrap_or(0),
                    revision: p["lastrevid"].as_u64().unwrap_or(0),
                });
            }
        }
    }

    /// The articles and subcategories of a category (`Category:Salmon Run`),
    /// with their latest revisions
    pub fn members(&self, fetcher: &mut Fetcher, category: &str) -> Result<Vec<Listed>> {
        let params = alloc::format!(
            "action=query&generator=categorymembers&gcmtitle={}&gcmnamespace=0%7C{CATEGORY_NS}&gcmlimit=500&prop=info",
            encode(category)
        );
        let mut out = Vec::new();
        self.query_all(fetcher, &params, &mut |v| Self::listed(v, &mut out))?;
        out.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(out)
    }

    /// Titles of the articles a page links to
    pub fn links(&self, fetcher: &mut Fetcher, title: &str) -> Result<Vec<String>> {
        let params = alloc::format!(
            "action=query&prop=links&titles={}&plnamespace=0&pllimit=max&redirects=1",
            encode(title)
        );
        let mut out = Vec::new();
        self.query_all(fetcher, &params, &mut |v| {
            for p in v["query"]["pages"].as_array().into_iter().flatten() {
                for l in p["links"].as_array().into_iter().flatten() {
                    if let Some(t) = l["title"].as_str() {
                        out.push(String::from(t));
                    }
                }
            }
        })?;
        Ok(out)
    }

    /// The latest revisions of pages by title (redirects followed; pages
    /// that do not exist are left out)
    pub fn latest(&self, fetcher: &mut Fetcher, titles: &[String]) -> Result<Vec<Listed>> {
        let mut out = Vec::new();
        for group in titles.chunks(TITLES_PER_REQUEST) {
            let titles: Vec<String> = group.iter().map(|t| encode(t)).collect();
            let params = alloc::format!(
                "action=query&prop=info&redirects=1&titles={}",
                titles.join("%7C")
            );
            let v = self.query(fetcher, &params)?;
            Self::listed(&v, &mut out);
        }
        Ok(out)
    }

    /// One article's rendered content
    pub fn page(&self, fetcher: &mut Fetcher, title: &str) -> Result<WikiPage> {
        let params = alloc::format!(
            "action=parse&page={}&prop=text%7Crevid&redirects=1&disableeditsection=1",
            encode(title)
        );
        let v = self.query(fetcher, &params)?;
        Ok(WikiPage {
            title: String::from(v["parse"]["title"].as_str().unwrap_or(title)),
            html: String::from(v["parse"]["text"].as_str().unwrap_or_default()),
            revision: v["parse"]["revid"].as_u64().unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sitemaps() {
        let xml = "<urlset><url><loc> https://a.org/x?a=1&amp;b=2 </loc></url>\
                   <url><loc>https://a.org/y</loc></url></urlset>\
                   <sitemapindex><sitemap><loc>https://a.org/s2.xml</loc></sitemap></sitemapindex>";
        let (pages, nested) = sitemap_locs(xml);
        assert_eq!(pages, ["https://a.org/x?a=1&b=2", "https://a.org/y"]);
        assert_eq!(nested, ["https://a.org/s2.xml"]);
    }

    #[test]
    fn reads_file_names() {
        let h =
            "attachment; filename=\"SalmonRun.md\"; filename*=UTF-8''Salmon%20Run%3A%20Tides.md";
        assert_eq!(disposition_name(h).as_deref(), Some("Salmon Run: Tides.md"));
        assert_eq!(
            disposition_name("attachment; filename=\"a b.csv\"").as_deref(),
            Some("a b.csv")
        );
        assert_eq!(disposition_name("inline"), None);
    }

    #[test]
    fn encodes() {
        assert_eq!(encode("Category:Salmon Run"), "Category%3ASalmon%20Run");
        assert_eq!(encode("バ"), "%E3%83%90");
    }

    #[test]
    fn robots_rules_apply_to_our_token() {
        let txt = b"User-agent: Cuttlefish\nDisallow: /private\n\nUser-agent: *\nDisallow: /\n";
        let r = Robot::new(ROBOTS_TOKEN, txt).unwrap();
        assert!(r.allowed("https://a.org/wiki/x"));
        assert!(!r.allowed("https://a.org/private/x"));
    }

    #[test]
    fn user_agent_names_the_tool() {
        assert!(user_agent().starts_with("Cuttlefish/"));
    }
}
