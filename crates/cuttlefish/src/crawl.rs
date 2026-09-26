//! Polite web fetching.
//!
//! [`Fetcher`] identifies itself ([`user_agent`]), obeys each site's
//! `robots.txt` (rules for `Cuttlefish` or `*`; a `robots.txt` that fails
//! with a server error blocks the site, as RFC 9309 asks) and waits between
//! requests to the same site: the given delay or the site's `Crawl-delay`,
//! whichever is longer. It also reads sitemaps and MediaWiki's API, which
//! returns an article's content without the page chrome.

use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use core::time::Duration;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;
use texting_robots::{Robot, get_robots_url};

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

/// Fetches pages politely
pub struct Fetcher {
    agent: ureq::Agent,
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
        Fetcher {
            agent: agent(Duration::from_secs(60)),
            delay,
            robots: HashMap::new(),
            last: HashMap::new(),
        }
    }

    /// Sleeps until the site may be asked again
    fn wait(&mut self, site: &str, extra: Option<f32>) {
        let delay = self.delay.max(Duration::from_secs_f32(
            extra.unwrap_or(0.0).clamp(0.0, 120.0),
        ));
        if let Some(last) = self.last.get(site) {
            let since = last.elapsed();
            if since < delay {
                std::thread::sleep(delay - since);
            }
        }
        self.last.insert(String::from(site), Instant::now());
    }

    /// GET with the politeness delay; (status, body)
    fn get_raw(
        &mut self,
        url: &str,
        site: &str,
        crawl_delay: Option<f32>,
    ) -> Result<(u16, String)> {
        self.wait(site, crawl_delay);
        let mut resp = self
            .agent
            .get(url)
            .call()
            .with_context(|| alloc::format!("GET {url}"))?;
        let status = resp.status().as_u16();
        let body = resp
            .body_mut()
            .with_config()
            .limit(50 * 1024 * 1024)
            .read_to_string()
            .unwrap_or_default();
        Ok((status, body))
    }

    /// Whether `robots.txt` lets us fetch `url`, and the site's crawl delay
    fn check_robots(&mut self, url: &str) -> Result<(String, bool, Option<f32>)> {
        let robots_url =
            get_robots_url(url).map_err(|e| anyhow::anyhow!("bad url {url}: {e:?}"))?;
        if !self.robots.contains_key(&robots_url) {
            let (status, body) = self.get_raw(&robots_url, &robots_url.clone(), None)?;
            let robot = match status {
                200..=299 => Some(Robot::new(ROBOTS_TOKEN, body.as_bytes())?),
                400..=499 => None,
                _ => bail!("{robots_url}: HTTP {status}; not crawling this site"),
            };
            self.robots.insert(robots_url.clone(), robot);
        }
        let robot = &self.robots[&robots_url];
        let allowed = robot.as_ref().is_none_or(|r| r.allowed(url));
        let delay = robot.as_ref().and_then(|r| r.delay);
        Ok((robots_url, allowed, delay))
    }

    /// Fetches a page as text; fails if `robots.txt` forbids it or the
    /// server does not answer 2xx
    pub fn get(&mut self, url: &str) -> Result<String> {
        let (site, allowed, delay) = self.check_robots(url)?;
        if !allowed {
            bail!("robots.txt disallows {url}");
        }
        let (status, body) = self.get_raw(url, &site, delay)?;
        if !(200..300).contains(&status) {
            bail!("GET {url}: HTTP {status}");
        }
        Ok(body)
    }
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

/// A MediaWiki site through its `api.php`
pub struct MediaWiki {
    /// `https://example.org/w/api.php`
    pub api: String,
}

/// What [`MediaWiki::site_info`] returns
pub struct SiteInfo {
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

/// One article from [`MediaWiki::page`]
pub struct WikiPage {
    /// Article title
    pub title: String,
    /// Rendered content HTML (no navigation or footer)
    pub html: String,
}

impl MediaWiki {
    fn query(&self, fetcher: &mut Fetcher, params: &str) -> Result<serde_json::Value> {
        let url = alloc::format!("{}?format=json&formatversion=2&{params}", self.api);
        let body = fetcher.get(&url)?;
        let v: serde_json::Value = serde_json::from_str(&body).context("MediaWiki answer")?;
        if let Some(err) = v.get("error") {
            bail!("MediaWiki: {err}");
        }
        Ok(v)
    }

    /// The site's content license and article url pattern (`siteinfo`)
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
            license,
            article_url: alloc::format!("{server}{path}"),
        })
    }

    /// Article titles in a category (`Category:Salmon Run Next Wave`), not
    /// descending into subcategories
    pub fn category(&self, fetcher: &mut Fetcher, category: &str) -> Result<Vec<String>> {
        let mut titles = Vec::new();
        let mut cont = String::new();
        loop {
            let params = alloc::format!(
                "action=query&list=categorymembers&cmtitle={}&cmnamespace=0&cmlimit=500{cont}",
                encode(category)
            );
            let v = self.query(fetcher, &params)?;
            for m in v["query"]["categorymembers"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(t) = m["title"].as_str() {
                    titles.push(String::from(t));
                }
            }
            match v["continue"]["cmcontinue"].as_str() {
                Some(c) => cont = alloc::format!("&cmcontinue={}", encode(c)),
                None => return Ok(titles),
            }
        }
    }

    /// One article's rendered content
    pub fn page(&self, fetcher: &mut Fetcher, title: &str) -> Result<WikiPage> {
        let params = alloc::format!(
            "action=parse&page={}&prop=text&redirects=1&disableeditsection=1",
            encode(title)
        );
        let v = self.query(fetcher, &params)?;
        Ok(WikiPage {
            title: String::from(v["parse"]["title"].as_str().unwrap_or(title)),
            html: String::from(v["parse"]["text"].as_str().unwrap_or_default()),
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
