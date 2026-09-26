//! Images and icons from the inbox, kept as a catalogue
//! (`<data>/assets.json`): where each is, its size, a name from its file
//! name and folder, and the glossary term it shows when its file name
//! names one (an icon `Wst_Shooter_Normal_00.png` is the weapon whose
//! imported key is `Shooter_Normal_00`, `steelhead.svg` is the Steelhead).
//!
//! Images are not embedded (an image embedder such as CLIP or SigLIP could
//! make them searchable by content later). Thumbnails are made on request
//! by ffmpeg and kept in the local cache, never in the synced folder;
//! small icons and SVGs are served as they are.

use crate::doc::doc_id;
use crate::glossary::Glossary;
use crate::store::write_atomic;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// Longest side of a thumbnail, in pixels
pub const THUMB: u32 = 160;

/// Largest image served as its own thumbnail, in bytes
const SMALL: u64 = 256 << 10;

/// One image or icon
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// Id: [`doc_id`] of `path`
    pub id: String,
    /// Path inside the inbox; for a file inside an archive, the archive's
    /// path, `/` and the path inside it
    pub path: String,
    /// The archive holding it, when it is inside one
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    /// Name from the file name (`Wst Shooter Normal 00`)
    pub name: String,
    /// Folder it is in (`icons/weapons`)
    pub folder: String,
    /// Format from the extension (`png`, `svg`, ...)
    pub format: String,
    /// Size of the file
    pub bytes: u64,
    /// Width and height in pixels, when the header says
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Content hash (thumbnails are cached by it)
    pub hash: String,
    /// Id of the glossary term it shows
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<String>,
}

impl Asset {
    /// Describes an image of the inbox from its path, the start of its
    /// content (enough for the header) and its size
    pub fn new(path: &str, archive: Option<&str>, head: &[u8], bytes: u64, hash: &str) -> Self {
        let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
        let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
        let format = ext.to_lowercase();
        let (width, height) = dimensions(head, &format).unzip();
        Asset {
            id: doc_id(path),
            path: String::from(path),
            archive: archive.map(String::from),
            name: words(stem, false),
            folder: String::from(folder),
            format,
            bytes,
            width,
            height,
            hash: String::from(hash),
            term: None,
        }
    }

    /// Media type for its format
    pub fn media_type(&self) -> &'static str {
        match self.format.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            "bmp" => "image/bmp",
            "ico" => "image/x-icon",
            "avif" => "image/avif",
            _ => "application/octet-stream",
        }
    }
}

/// Words of a name, split at anything but letters and digits; lowercase
/// when `lower`
fn words(text: &str, lower: bool) -> String {
    let text = if lower {
        text.to_lowercase()
    } else {
        String::from(text)
    };
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn u16_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn u16_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?) as u32)
}

fn u24_le(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
}

/// The number at the start of an SVG attribute (`24`, `24px`, `24.5`)
fn svg_number(tag: &str, attribute: &str) -> Option<f32> {
    let at = tag.find(&alloc::format!(" {attribute}=\""))? + attribute.len() + 3;
    let value: String = tag[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    value.parse().ok()
}

/// Width and height from an image's header: PNG, GIF, JPEG, WebP, BMP,
/// ICO, and SVG (its `width`/`height` or `viewBox`)
pub fn dimensions(head: &[u8], format: &str) -> Option<(u32, u32)> {
    match format {
        "png" if head.starts_with(b"\x89PNG") => Some((
            u32::from_be_bytes(head.get(16..20)?.try_into().ok()?),
            u32::from_be_bytes(head.get(20..24)?.try_into().ok()?),
        )),
        "gif" if head.starts_with(b"GIF8") => Some((u16_le(head, 6)?, u16_le(head, 8)?)),
        "jpg" | "jpeg" => {
            let mut at = 2;
            while at + 9 < head.len() {
                if head[at] != 0xff {
                    return None;
                }
                let marker = head[at + 1];
                let length = u16_be(head, at + 2)? as usize;
                // Start of frame, except DHT, JPG and DAC
                if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                    return Some((u16_be(head, at + 7)?, u16_be(head, at + 5)?));
                }
                at += 2 + length;
            }
            None
        }
        "webp" if head.get(8..12) == Some(b"WEBP") => match head.get(12..16)? {
            b"VP8 " => Some((u16_le(head, 26)? & 0x3fff, u16_le(head, 28)? & 0x3fff)),
            b"VP8L" => {
                let b = head.get(21..25)?;
                let (b0, b1, b2, b3) = (b[0] as u32, b[1] as u32, b[2] as u32, b[3] as u32);
                Some((
                    1 + (b0 | (b1 & 0x3f) << 8),
                    1 + (b1 >> 6 | b2 << 2 | (b3 & 0xf) << 10),
                ))
            }
            b"VP8X" => Some((1 + u24_le(head, 24)?, 1 + u24_le(head, 27)?)),
            _ => None,
        },
        "bmp" if head.starts_with(b"BM") => Some((
            i32::from_le_bytes(head.get(18..22)?.try_into().ok()?).unsigned_abs(),
            i32::from_le_bytes(head.get(22..26)?.try_into().ok()?).unsigned_abs(),
        )),
        "ico" => {
            let side = |b: u8| if b == 0 { 256 } else { b as u32 };
            Some((side(*head.get(6)?), side(*head.get(7)?)))
        }
        "svg" => {
            let text = String::from_utf8_lossy(head);
            let start = text.find("<svg")?;
            let tag = &text[start..start + text[start..].find('>')?];
            match (svg_number(tag, "width"), svg_number(tag, "height")) {
                (Some(w), Some(h)) => Some((w as u32, h as u32)),
                _ => {
                    let at = tag.find(" viewBox=\"")? + 10;
                    let view: Vec<f32> = tag[at..]
                        .split('"')
                        .next()?
                        .split([' ', ','])
                        .filter_map(|n| n.parse().ok())
                        .collect();
                    Some((*view.get(2)? as u32, *view.get(3)? as u32))
                }
            }
        }
        _ => None,
    }
}

/// Links each asset to the glossary term its file name names: the words of
/// the file name, or of its end (`Path Wst Shooter Normal 00` → `Shooter
/// Normal 00`), equal to a term's id, one of its names, or the last part of
/// an imported key. Returns how many are linked.
pub fn link(assets: &mut [Asset], glossary: &Glossary) -> usize {
    let mut names: BTreeMap<String, &str> = BTreeMap::new();
    for t in &glossary.terms {
        let keys = t
            .from
            .iter()
            .filter_map(|f| f.rsplit(['#', '/']).next())
            .chain(t.forms.values().flatten().map(String::as_str))
            .chain([t.id.as_str()]);
        for k in keys {
            names.entry(words(k, true)).or_insert(&t.id);
        }
    }
    let mut linked = 0;
    for a in assets.iter_mut() {
        let stem = a.path.rsplit('/').next().unwrap_or_default();
        let stem = stem.rsplit_once('.').map_or(stem, |(s, _)| s);
        let all = words(stem, true);
        let tokens: Vec<&str> = all.split(' ').collect();
        a.term = (0..tokens.len())
            .map(|i| tokens[i..].join(" "))
            .filter(|w| w.len() >= 4 && !w.chars().all(|c| c.is_ascii_digit() || c == ' '))
            .find_map(|w| names.get(&w).map(|id| String::from(*id)));
        linked += a.term.is_some() as usize;
    }
    linked
}

/// The catalogue of a data folder
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalogue {
    /// Images and icons, by path
    pub assets: Vec<Asset>,
}

impl Catalogue {
    fn path(root: &Path) -> PathBuf {
        root.join("assets.json")
    }

    /// The data folder's catalogue; empty without one, or with one that
    /// does not read (with a warning)
    pub fn load(root: &Path) -> Self {
        let path = Self::path(root);
        let Ok(bytes) = std::fs::read(&path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            log::warn!("{} does not read: {e}", path.display());
            Self::default()
        })
    }

    /// Writes the catalogue
    pub fn save(&self, root: &Path) -> Result<()> {
        write_atomic(&Self::path(root), &serde_json::to_vec(self)?)
    }

    /// Adds an asset, replacing the one of the same path
    pub fn upsert(&mut self, asset: Asset) {
        match self.assets.iter_mut().find(|a| a.path == asset.path) {
            Some(a) => *a = asset,
            None => self.assets.push(asset),
        }
        self.assets.sort_by(|a, b| a.path.cmp(&b.path));
    }

    /// Number of assets per folder
    pub fn folders(&self) -> BTreeMap<&str, usize> {
        let mut out = BTreeMap::new();
        for a in &self.assets {
            *out.entry(a.folder.as_str()).or_default() += 1;
        }
        out
    }
}

/// A path inside a folder, refused when it could leave it
fn inside(dir: &Path, rel: &str) -> Result<PathBuf> {
    let rel = Path::new(rel);
    ensure!(
        rel.components().all(|c| matches!(c, Component::Normal(_))),
        "bad path {}",
        rel.display()
    );
    Ok(dir.join(rel))
}

/// The bytes of an asset, read from the inbox (`inbox` is its folder) or
/// out of its archive
pub fn read(inbox: &Path, asset: &Asset) -> Result<Vec<u8>> {
    let Some(archive) = &asset.archive else {
        return Ok(std::fs::read(inside(inbox, &asset.path)?)?);
    };
    let file = inside(inbox, archive)?;
    ensure!(file.is_file(), "{archive} is inside another archive");
    let member = asset
        .path
        .strip_prefix(archive.as_str())
        .and_then(|m| m.strip_prefix('/'))
        .context("not in its archive")?;
    let out = Command::new("bsdtar")
        .arg("-xOf")
        .arg(&file)
        .arg(member)
        .stdin(Stdio::null())
        .output()
        .context("running bsdtar (install libarchive)")?;
    ensure!(
        out.status.success() && !out.stdout.is_empty(),
        "cannot read {member} from {archive}"
    );
    Ok(out.stdout)
}

/// A thumbnail of an asset and its media type: SVGs and small images as
/// they are, others scaled to fit [`THUMB`] pixels by ffmpeg, as PNG kept in
/// `cache` by content hash
pub fn thumbnail(inbox: &Path, cache: &Path, asset: &Asset) -> Result<(Vec<u8>, &'static str)> {
    let small = asset.width.is_some_and(|w| w <= 2 * THUMB)
        && asset.height.is_some_and(|h| h <= 2 * THUMB)
        && asset.bytes <= SMALL;
    if asset.format == "svg" || small {
        return Ok((read(inbox, asset)?, asset.media_type()));
    }
    let cached = cache
        .join("thumbs")
        .join(alloc::format!("{}.png", asset.hash));
    if let Ok(png) = std::fs::read(&cached) {
        return Ok((png, "image/png"));
    }
    let bytes = read(inbox, asset)?;
    let mut ffmpeg = Command::new("ffmpeg")
        .args(["-v", "error", "-i", "pipe:0", "-vf"])
        .arg(alloc::format!(
            "scale={THUMB}:{THUMB}:force_original_aspect_ratio=decrease"
        ))
        .args([
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ffmpeg")?;
    let mut stdin = ffmpeg.stdin.take().context("no stdin")?;
    let out = std::thread::scope(|s| {
        // A failed write shows as ffmpeg's error
        s.spawn(move || stdin.write_all(&bytes));
        ffmpeg.wait_with_output()
    })?;
    if !out.status.success() || out.stdout.is_empty() {
        bail!(
            "ffmpeg cannot read {}: {}",
            asset.path,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    write_atomic(&cached, &out.stdout)?;
    Ok((out.stdout, "image/png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_headers() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(64u32.to_be_bytes());
        png.extend(32u32.to_be_bytes());
        assert_eq!(dimensions(&png, "png"), Some((64, 32)));
        let gif = b"GIF89a\x10\x00\x08\x00";
        assert_eq!(dimensions(gif, "gif"), Some((16, 8)));
        let mut jpeg = alloc::vec![0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0];
        jpeg.extend([0xff, 0xc0, 0, 11, 8, 0, 120, 0, 200, 3, 0, 0]);
        assert_eq!(dimensions(&jpeg, "jpg"), Some((200, 120)));
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 24"><path/></svg>"#;
        assert_eq!(dimensions(svg, "svg"), Some((48, 24)));
        let sized = br#"<svg width="32px" height="16" viewBox="0 0 1 1">"#;
        assert_eq!(dimensions(sized, "svg"), Some((32, 16)));
        assert_eq!(dimensions(b"junk", "png"), None);
    }

    #[test]
    fn names_and_links_icons() {
        let glossary = Glossary::parse(
            r#"
            [[term]]
            id = "splattershot"
            forms = { en = ["Splattershot"], ja = ["スプラシューター"] }
            from = ["splat3/language/*.json#CommonMsg/Weapon/WeaponName_Main/Shooter_Normal_00"]
            [[term]]
            id = "steelhead"
            forms = { en = ["Steelhead"] }
            "#,
        )
        .unwrap();
        let mut assets = alloc::vec![
            Asset::new(
                "icons/weapon/Path_Wst_Shooter_Normal_00.png",
                None,
                b"",
                10,
                "a"
            ),
            Asset::new("bosses/steelhead.svg", None, b"", 10, "b"),
            Asset::new("misc/00.png", None, b"", 10, "c"),
            Asset::new("pack.zip/ui/logo.png", Some("pack.zip"), b"", 10, "d"),
        ];
        assert_eq!(link(&mut assets, &glossary), 2);
        assert_eq!(assets[0].term.as_deref(), Some("splattershot"));
        assert_eq!(assets[0].name, "Path Wst Shooter Normal 00");
        assert_eq!(assets[0].folder, "icons/weapon");
        assert_eq!(assets[1].term.as_deref(), Some("steelhead"));
        assert_eq!(assets[2].term, None);
        assert_eq!(assets[3].folder, "pack.zip/ui");
        let mut catalogue = Catalogue::default();
        for a in assets {
            catalogue.upsert(a);
        }
        assert_eq!(catalogue.folders()["icons/weapon"], 1);
        assert!(inside(Path::new("/x"), "../etc/passwd").is_err());
    }
}
