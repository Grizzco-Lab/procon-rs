//! The attachments of an archived Discord channel: the VODs uploaded to
//! #vod-review as files, and the images.
//!
//! The messages `cuttlefish fetch discord` keeps carry their attachments
//! as Discord's CDN links (`cdn.discordapp.com`, `media.discordapp.net`),
//! which are signed and expire after about a day (`ex`, `is` and `hm` in
//! the query). So the fetcher downloads what is wanted right after the
//! messages ([`crate::discord_fetch`]), and re-reads the page of a message
//! whose link expired for a fresh one. This module is the part that needs
//! no network: which attachments a message has and which are wanted
//! ([`Attachment`], [`Attachments`]), the scope check on a link
//! ([`in_scope`]), its expiry ([`expired`]), where a file goes and the
//! manifest of what was downloaded ([`Manifest`], `media.jsonl`), and the
//! listing a run prints ([`Listing`]).
//!
//! Files go to `<media>/<guild>/<channel>/<message id>/<file name>` with
//! the manifest `media.jsonl` in the channel's folder, `<media>` being
//! `<knowledge>/media/discord`. The inbox reads the manifest to point each
//! message's `video_local` at the file ([`crate::discord::link_media`]).

use crate::discord::VIDEO_EXTENSIONS;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result};
use core::fmt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Hosts of attachment links; nothing else is downloaded
pub const CDN_HOSTS: [&str; 2] = ["cdn.discordapp.com", "media.discordapp.net"];
/// Extensions of image attachments
pub const IMAGE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "avif", "bmp"];
/// The manifest in a channel's media folder
pub const MANIFEST: &str = "media.jsonl";
/// The media folder in the knowledge folder; Discord's files are in its
/// `discord/<guild>/<channel>/`
pub const MEDIA: &str = "media";
/// A link this close to its expiry is treated as expired: the download
/// may start later than the check
const EXPIRY_MARGIN_S: i64 = 120;

/// What to do with attachments (`--attachments`)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Attachments {
    /// Nothing, not even a count
    None,
    /// Count them and add up their sizes; download nothing
    #[default]
    List,
    /// Download the videos
    Videos,
    /// Download the videos and the images
    Media,
}

/// What kind of file an attachment is
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Video,
    Image,
    Other,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Video => write!(f, "video"),
            Kind::Image => write!(f, "image"),
            Kind::Other => write!(f, "other"),
        }
    }
}

/// An attachment of a message, as the API describes it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// The message it is on
    pub message_id: String,
    /// The channel or thread that message is in (its messages file)
    pub conversation: String,
    /// The attachment's id, unique for good
    pub id: String,
    pub filename: String,
    pub size: u64,
    pub content_type: Option<String>,
    /// The signed CDN link
    pub url: String,
}

/// The extension of a file name, lower case, without the query of a link
fn extension(name: &str) -> String {
    let name = name.split(['?', '#']).next().unwrap_or_default();
    name.rsplit_once('.')
        .map(|(_, e)| e.to_lowercase())
        .unwrap_or_default()
}

impl Attachment {
    /// The attachments of an API message object
    pub fn from_message(m: &Value, conversation: &str) -> Vec<Attachment> {
        let Some(mid) = m["id"].as_str() else {
            return Vec::new();
        };
        m["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                Some(Attachment {
                    message_id: String::from(mid),
                    conversation: String::from(conversation),
                    id: String::from(a["id"].as_str()?),
                    filename: String::from(a["filename"].as_str().unwrap_or_default()),
                    size: a["size"].as_u64().unwrap_or_default(),
                    content_type: a["content_type"].as_str().map(String::from),
                    url: String::from(a["url"].as_str()?),
                })
            })
            .collect()
    }

    /// Video or image by the content type, else by the file name
    pub fn kind(&self) -> Kind {
        if let Some(t) = &self.content_type {
            if t.starts_with("video/") {
                return Kind::Video;
            }
            if t.starts_with("image/") {
                return Kind::Image;
            }
        }
        let ext = extension(&self.filename);
        if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            Kind::Video
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            Kind::Image
        } else {
            Kind::Other
        }
    }

    /// Whether `which` asks for this attachment
    pub fn wanted(&self, which: Attachments) -> bool {
        match which {
            Attachments::None | Attachments::List => false,
            Attachments::Videos => self.kind() == Kind::Video,
            Attachments::Media => self.kind() != Kind::Other,
        }
    }

    /// The file name on disk: the attachment's, without folders or control
    /// characters; the id when nothing is left
    pub fn safe_name(&self) -> String {
        let name: String = self
            .filename
            .chars()
            .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '\0'))
            .collect();
        let name = name.trim().trim_start_matches('.');
        if name.is_empty() {
            self.id.clone()
        } else {
            String::from(name)
        }
    }

    /// Where the file goes below a channel's media folder
    pub fn relative_path(&self) -> String {
        alloc::format!("{}/{}", self.message_id, self.safe_name())
    }
}

/// Whether a link may be downloaded: HTTPS to one of the CDN hosts, an
/// attachment path (`/attachments/<channel>/<id>/<name>`). Which messages
/// the links come from is the fetcher's scope; this keeps a link in a
/// message's text or embeds from being taken for one of its files.
pub fn in_scope(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    CDN_HOSTS.contains(&host.to_lowercase().as_str())
        && path.starts_with("attachments/")
        && path
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .matches('/')
            .count()
            == 3
}

/// When a signed link expires (Unix seconds): its `ex` parameter, hex
pub fn expires(url: &str) -> Option<i64> {
    let query = url.split_once('?')?.1;
    let ex = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("ex="))?
        .split('#')
        .next()?;
    i64::from_str_radix(ex, 16).ok()
}

/// Whether a link has expired, or nearly, at `now` (Unix seconds); a link
/// without an expiry has not
pub fn expired(url: &str, now: i64) -> bool {
    expires(url).is_some_and(|ex| ex - EXPIRY_MARGIN_S <= now)
}

/// A downloaded file, a line of `media.jsonl`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub message_id: String,
    pub attachment_id: String,
    pub filename: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Below the manifest's folder: `<message id>/<file name>`
    pub path: String,
    pub sha256: String,
}

/// What a channel's media folder holds, by attachment id
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    dir: PathBuf,
    entries: BTreeMap<String, Entry>,
}

impl Manifest {
    /// The manifest of the media folder `dir`, or an empty one
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST);
        let mut entries = BTreeMap::new();
        if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| alloc::format!("reading {}", path.display()))?;
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(e) = serde_json::from_str::<Entry>(line) {
                    entries.insert(e.attachment_id.clone(), e);
                }
            }
        }
        Ok(Manifest {
            dir: dir.to_path_buf(),
            entries,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the attachment's file is here and whole (the manifest names
    /// it, and the file has its size)
    pub fn has(&self, a: &Attachment) -> bool {
        self.entries.get(&a.id).is_some_and(|e| {
            std::fs::metadata(self.dir.join(&e.path)).is_ok_and(|m| m.len() == e.size)
        })
    }

    /// The entry of an attachment id
    pub fn get(&self, attachment_id: &str) -> Option<&Entry> {
        self.entries.get(attachment_id)
    }

    /// Bytes of the files listed
    pub fn bytes(&self) -> u64 {
        self.entries.values().map(|e| e.size).sum()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records a downloaded file: a line appended to the manifest
    pub fn add(&mut self, entry: Entry) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(MANIFEST);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| alloc::format!("opening {}", path.display()))?;
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        self.entries.insert(entry.attachment_id.clone(), entry);
        Ok(())
    }
}

/// SHA-256 of a file, hex
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| alloc::format!("reading {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = alloc::vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(alloc::format!("{:x}", hasher.finalize()))
}

/// A count of files and their bytes
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub files: usize,
    pub bytes: u64,
}

impl Tally {
    pub fn add(&mut self, bytes: u64) {
        self.files += 1;
        self.bytes += bytes;
    }
}

/// Gigabytes with one decimal (`412.5 GB`), megabytes below one
pub fn size(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        alloc::format!("{gb:.1} GB")
    } else {
        alloc::format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// What a channel's messages carry, by kind; `over_cap` counts the wanted
/// files larger than `--max-file-mb`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Listing {
    pub videos: Tally,
    pub images: Tally,
    pub other: Tally,
    pub over_cap: Tally,
}

impl Listing {
    /// Counts `attachments` (each id once), the cap in bytes when there is
    /// one and `which` says what is wanted
    pub fn of(attachments: &[Attachment], which: Attachments, cap: Option<u64>) -> Listing {
        let mut l = Listing::default();
        for a in attachments {
            match a.kind() {
                Kind::Video => l.videos.add(a.size),
                Kind::Image => l.images.add(a.size),
                Kind::Other => l.other.add(a.size),
            }
            if a.wanted(which) && cap.is_some_and(|c| a.size > c) {
                l.over_cap.add(a.size);
            }
        }
        l
    }

    /// Adds another channel's listing
    pub fn extend(&mut self, other: &Listing) {
        for (mine, theirs) in [
            (&mut self.videos, other.videos),
            (&mut self.images, other.images),
            (&mut self.other, other.other),
            (&mut self.over_cap, other.over_cap),
        ] {
            mine.files += theirs.files;
            mine.bytes += theirs.bytes;
        }
    }
}

impl fmt::Display for Listing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} videos ({}), {} images ({}), {} other files ({})",
            self.videos.files,
            size(self.videos.bytes),
            self.images.files,
            size(self.images.bytes),
            self.other.files,
            size(self.other.bytes)
        )
    }
}

/// The attachments in a messages file (`<id>.messages.jsonl`), each
/// attachment id once with the link of its last line (the freshest)
pub fn in_messages_file(path: &Path) -> Result<Vec<Attachment>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| alloc::format!("reading {}", path.display()))?;
    let conversation = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".messages.jsonl"))
        .unwrap_or_default();
    let mut by_id: BTreeMap<String, Attachment> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(m) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for a in Attachment::from_message(&m, conversation) {
            by_id.insert(a.id.clone(), a);
        }
    }
    Ok(by_id.into_values().collect())
}

/// The attachments of a channel's archive folder: its messages file and
/// its threads' (`threads/*.messages.jsonl`)
pub fn in_channel_folder(folder: &Path, channel: &str) -> Result<Vec<Attachment>> {
    let mut files = alloc::vec![folder.join(alloc::format!("{channel}.messages.jsonl"))];
    if let Ok(entries) = std::fs::read_dir(folder.join("threads")) {
        let mut threads: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".messages.jsonl"))
            })
            .collect();
        threads.sort();
        files.extend(threads);
    }
    let mut out = Vec::new();
    for f in files.iter().filter(|f| f.is_file()) {
        out.extend(in_messages_file(f)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn attachment(name: &str, content_type: Option<&str>, size: u64) -> Attachment {
        Attachment {
            message_id: String::from("100"),
            conversation: String::from("2"),
            id: String::from("7"),
            filename: String::from(name),
            size,
            content_type: content_type.map(String::from),
            url: String::from("https://cdn.discordapp.com/attachments/2/7/x?ex=1"),
        }
    }

    #[test]
    fn kinds_and_wants() {
        assert_eq!(attachment("run.MP4", None, 1).kind(), Kind::Video);
        assert_eq!(attachment("x", Some("video/webm"), 1).kind(), Kind::Video);
        assert_eq!(attachment("shot.PNG", None, 1).kind(), Kind::Image);
        assert_eq!(
            attachment("x.bin", Some("image/png"), 1).kind(),
            Kind::Image
        );
        assert_eq!(attachment("notes.txt", None, 1).kind(), Kind::Other);
        let video = attachment("run.mp4", None, 1);
        let image = attachment("shot.png", None, 1);
        assert!(!video.wanted(Attachments::None) && !video.wanted(Attachments::List));
        assert!(video.wanted(Attachments::Videos) && !image.wanted(Attachments::Videos));
        assert!(video.wanted(Attachments::Media) && image.wanted(Attachments::Media));
        assert!(!attachment("a.txt", None, 1).wanted(Attachments::Media));
        assert_eq!(Attachments::default(), Attachments::List);

        assert_eq!(
            attachment("../../etc/passwd", None, 1).safe_name(),
            "etc/passwd".replace('/', "")
        );
        assert_eq!(attachment("a\\b:c.mp4", None, 1).safe_name(), "abc.mp4");
        assert_eq!(attachment("", None, 1).safe_name(), "7");
        assert_eq!(
            attachment("Run 3.mp4", None, 1).relative_path(),
            "100/Run 3.mp4"
        );
    }

    #[test]
    fn scope_and_expiry() {
        assert!(in_scope(
            "https://cdn.discordapp.com/attachments/2/7/run.mp4?ex=66f0a1b2&is=66ef5032&hm=abc"
        ));
        assert!(in_scope(
            "https://media.discordapp.net/attachments/2/7/shot.png"
        ));
        assert!(in_scope(
            "https://CDN.discordapp.com/attachments/2/7/shot.png"
        ));
        assert!(!in_scope(
            "http://cdn.discordapp.com/attachments/2/7/run.mp4"
        ));
        assert!(!in_scope("https://cdn.discordapp.com/avatars/5/abc.png"));
        assert!(!in_scope("https://cdn.discordapp.com/attachments/2/7"));
        assert!(!in_scope("https://cdn.discordapp.com/attachments/2/7/a/b"));
        assert!(!in_scope("https://youtu.be/x"));
        assert!(!in_scope(
            "https://cdn.discordapp.com.evil.org/attachments/2/7/run.mp4"
        ));

        let url =
            "https://cdn.discordapp.com/attachments/2/7/run.mp4?ex=66f0a1b2&is=66ef5032&hm=abc";
        assert_eq!(expires(url), Some(0x66f0_a1b2));
        assert!(!expired(url, 0x66f0_a1b2 - 3600));
        assert!(expired(url, 0x66f0_a1b2 - 60));
        assert!(expired(url, 0x66f0_a1b2 + 1));
        assert_eq!(
            expires("https://cdn.discordapp.com/attachments/2/7/run.mp4"),
            None
        );
        assert!(!expired(
            "https://cdn.discordapp.com/attachments/2/7/run.mp4",
            i64::MAX
        ));
    }

    #[test]
    fn manifest_and_listing() {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-media-manifest-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mut m = Manifest::load(&dir).unwrap();
        assert!(m.is_empty());
        let a = attachment("run.mp4", Some("video/mp4"), 5);
        assert!(!m.has(&a));
        std::fs::create_dir_all(dir.join("100")).unwrap();
        std::fs::write(dir.join("100/run.mp4"), b"12345").unwrap();
        m.add(Entry {
            message_id: a.message_id.clone(),
            attachment_id: a.id.clone(),
            filename: a.filename.clone(),
            size: a.size,
            content_type: a.content_type.clone(),
            path: a.relative_path(),
            sha256: sha256_file(&dir.join("100/run.mp4")).unwrap(),
        })
        .unwrap();
        assert!(m.has(&a));
        assert_eq!(m.bytes(), 5);
        // Read back; a file of the wrong size is not "there"
        let again = Manifest::load(&dir).unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(
            again.get("7").unwrap().sha256,
            "5994471abb01112afcc18159f6cc74b4f511b99806da59b3caf5a9c173cacfc5"
        );
        std::fs::write(dir.join("100/run.mp4"), b"123").unwrap();
        assert!(!again.has(&a));
        std::fs::remove_dir_all(&dir).unwrap();

        let list = Listing::of(
            &[
                attachment("a.mp4", None, 3_000_000_000),
                attachment("b.mp4", None, 1_000_000_000),
                attachment("c.png", None, 2_000_000),
                attachment("d.txt", None, 10),
            ],
            Attachments::Videos,
            Some(2_000_000_000),
        );
        assert_eq!(
            list.videos,
            Tally {
                files: 2,
                bytes: 4_000_000_000
            }
        );
        assert_eq!(
            list.images,
            Tally {
                files: 1,
                bytes: 2_000_000
            }
        );
        assert_eq!(
            list.other,
            Tally {
                files: 1,
                bytes: 10
            }
        );
        assert_eq!(
            list.over_cap,
            Tally {
                files: 1,
                bytes: 3_000_000_000
            }
        );
        assert_eq!(
            list.to_string(),
            "2 videos (4.0 GB), 1 images (2 MB), 1 other files (0 MB)"
        );
    }

    #[test]
    fn attachments_of_messages_files() {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-media-files-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("threads")).unwrap();
        let line = |id: &str, att: Value| {
            alloc::format!(
                "{}\n",
                json!({"id": id, "channel_id": "2", "content": "", "attachments": att})
            )
        };
        let att = |id: &str, url: &str| json!({"id": id, "filename": "run.mp4", "size": 10, "content_type": "video/mp4", "url": url});
        // The same message twice: the last link counts
        let mut text = line(
            "100",
            json!([att(
                "7",
                "https://cdn.discordapp.com/attachments/2/7/run.mp4?ex=1"
            )]),
        );
        text.push_str(&line("101", json!([])));
        text.push_str("not json\n");
        text.push_str(&line(
            "100",
            json!([att(
                "7",
                "https://cdn.discordapp.com/attachments/2/7/run.mp4?ex=2"
            )]),
        ));
        std::fs::write(dir.join("2.messages.jsonl"), text).unwrap();
        std::fs::write(
            dir.join("threads/30.messages.jsonl"),
            line(
                "300",
                json!([att(
                    "8",
                    "https://cdn.discordapp.com/attachments/30/8/run.mp4"
                )]),
            ),
        )
        .unwrap();
        let all = in_channel_folder(&dir, "2").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "7");
        assert_eq!(all[0].conversation, "2");
        assert!(all[0].url.ends_with("ex=2"));
        assert_eq!(all[1].id, "8");
        assert_eq!(all[1].conversation, "30");
        assert_eq!(all[1].message_id, "300");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
