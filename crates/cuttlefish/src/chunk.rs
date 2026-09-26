//! Splitting documents into chunks for embedding and retrieval.
//!
//! Text is split into sections at markdown headings and into paragraphs at
//! blank lines. Paragraphs are packed into chunks of about
//! [`ChunkConfig::target_tokens`]; a new heading starts a new chunk once the
//! current one is reasonably full. Paragraphs longer than
//! [`ChunkConfig::max_tokens`] are split at sentence ends. A chunk that is cut
//! for size starts with the last sentences of the previous one
//! ([`ChunkConfig::overlap_tokens`]) so an idea on the boundary is found in
//! both.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Chunk sizes, in estimated tokens ([`estimate_tokens`])
#[derive(Clone, Copy, Debug)]
pub struct ChunkConfig {
    /// Size a chunk is packed up to
    pub target_tokens: usize,
    /// Paragraphs longer than this are split at sentence ends
    pub max_tokens: usize,
    /// Tail of the previous chunk repeated at the start of the next one
    pub overlap_tokens: usize,
}

impl Default for ChunkConfig {
    /// 300-800 tokens is the usual range; the embedding model reads at most
    /// 512 of its own tokens, so chunks stay near the lower end
    fn default() -> Self {
        ChunkConfig {
            target_tokens: 400,
            max_tokens: 600,
            overlap_tokens: 60,
        }
    }
}

/// One piece of a document
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// Position in the document, from 0
    pub ordinal: u32,
    /// Heading path of the chunk's first paragraph (`Steelhead > Strategy`),
    /// empty before the first heading
    pub heading: String,
    /// The chunk's text
    pub text: String,
}

/// Rough token count: one per CJK character, one per four other characters.
/// Close enough for sizing chunks without loading a tokenizer.
pub fn estimate_tokens(s: &str) -> usize {
    let (mut cjk, mut other) = (0usize, 0usize);
    for c in s.chars() {
        if is_cjk(c) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x9fff | 0xac00..=0xd7af | 0xff00..=0xffef)
}

/// A paragraph with its heading path
struct Para {
    heading: String,
    text: String,
}

/// Splits `text` into paragraphs under their heading paths
fn paragraphs(text: &str) -> Vec<Para> {
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, path: &[(usize, String)], out: &mut Vec<Para>| {
        let t = cur.trim();
        if !t.is_empty() {
            let heading = path
                .iter()
                .map(|(_, h)| h.as_str())
                .collect::<Vec<_>>()
                .join(" > ");
            out.push(Para {
                heading,
                text: String::from(t),
            });
        }
        cur.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with(' ') {
            flush(&mut cur, &path, &mut out);
            path.retain(|(l, _)| *l < level);
            path.push((level, String::from(trimmed[level..].trim())));
        } else if line.trim().is_empty() {
            flush(&mut cur, &path, &mut out);
        } else {
            if !cur.is_empty() {
                cur.push('\n');
            }
            cur.push_str(line.trim_end());
        }
    }
    flush(&mut cur, &path, &mut out);
    out
}

/// Splits after sentence ends (`. ! ? 。 ！ ？` and newlines)
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let end = i + c.len_utf8();
        let boundary = match c {
            '。' | '！' | '？' | '\n' => true,
            '.' | '!' | '?' => chars.peek().is_none_or(|(_, n)| n.is_whitespace()),
            _ => false,
        };
        if boundary {
            out.push(&text[start..end]);
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out.into_iter().filter(|s| !s.trim().is_empty()).collect()
}

/// Cuts text without sentence ends into pieces of at most `max` tokens
fn hard_split(text: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if estimate_tokens(&cur) >= max && (c.is_whitespace() || is_cjk(c)) {
            out.push(core::mem::take(&mut cur));
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Pieces of at most `max` tokens: the paragraph itself, or its sentences
fn pieces(text: &str, max: usize) -> Vec<String> {
    if estimate_tokens(text) <= max {
        return alloc::vec![String::from(text)];
    }
    let mut out = Vec::new();
    for s in sentences(text) {
        if estimate_tokens(s) > max {
            out.extend(hard_split(s, max));
        } else {
            out.push(String::from(s.trim()));
        }
    }
    out
}

/// Last sentences of `text` totalling at most `budget` tokens
fn tail(text: &str, budget: usize) -> String {
    let mut picked: Vec<&str> = Vec::new();
    let mut used = 0;
    for s in sentences(text).into_iter().rev() {
        let t = estimate_tokens(s);
        if used + t > budget {
            break;
        }
        used += t;
        picked.push(s.trim());
    }
    picked.reverse();
    picked.join(" ")
}

/// Splits a document's text into chunks
pub fn chunk_text(text: &str, cfg: &ChunkConfig) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    let push = |heading: String, body: String, chunks: &mut Vec<Chunk>| {
        chunks.push(Chunk {
            ordinal: chunks.len() as u32,
            heading,
            text: body,
        });
    };
    for para in paragraphs(text) {
        for piece in pieces(&para.text, cfg.max_tokens) {
            let piece_tokens = estimate_tokens(&piece);
            if let Some((heading, body)) = cur.take() {
                let body_tokens = estimate_tokens(&body);
                let new_section = heading != para.heading && body_tokens * 3 >= cfg.target_tokens;
                if new_section {
                    push(heading, body, &mut chunks);
                    cur = Some((para.heading.clone(), piece));
                } else if body_tokens + piece_tokens > cfg.target_tokens {
                    let overlap = tail(&body, cfg.overlap_tokens);
                    let same_section = heading == para.heading;
                    push(heading, body, &mut chunks);
                    let start = if overlap.is_empty() || !same_section {
                        piece
                    } else {
                        alloc::format!("{overlap}\n\n{piece}")
                    };
                    cur = Some((para.heading.clone(), start));
                } else {
                    cur = Some((heading, alloc::format!("{body}\n\n{piece}")));
                }
            } else {
                cur = Some((para.heading.clone(), piece));
            }
        }
    }
    if let Some((heading, body)) = cur {
        push(heading, body, &mut chunks);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ChunkConfig {
        ChunkConfig {
            target_tokens: 40,
            max_tokens: 60,
            overlap_tokens: 10,
        }
    }

    #[test]
    fn estimates_tokens() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("金イクラ"), 4);
    }

    #[test]
    fn keeps_heading_paths() {
        let text = "# Bosses\n\nIntro.\n\n## Steelhead\n\nShoot the bomb.\n\n# Tides\n\nLow tide.";
        let paras = paragraphs(text);
        let headings: Vec<_> = paras.iter().map(|p| p.heading.as_str()).collect();
        assert_eq!(headings, ["Bosses", "Bosses > Steelhead", "Tides"]);
    }

    #[test]
    fn small_text_is_one_chunk() {
        let chunks = chunk_text("# A\n\nOne.\n\nTwo.", &cfg());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "A");
        assert_eq!(chunks[0].text, "One.\n\nTwo.");
    }

    #[test]
    fn packs_to_target_with_overlap() {
        // Ten paragraphs of about 13 tokens each
        let para = "Egg flow keeps the basket fed. Stay close to it.";
        let text = alloc::vec![para; 10].join("\n\n");
        let chunks = chunk_text(&text, &cfg());
        assert!(chunks.len() >= 3, "{}", chunks.len());
        for (i, c) in chunks.iter().enumerate() {
            assert_eq!(c.ordinal as usize, i);
            assert!(estimate_tokens(&c.text) <= 40 + 10 + 13, "{}", c.text);
        }
        // Later chunks start with the previous chunk's last sentence
        assert!(chunks[1].text.starts_with("Stay close to it.\n\n"));
    }

    #[test]
    fn splits_long_paragraphs_at_sentences() {
        let long = "Kill the Steelhead before it throws. ".repeat(30);
        let chunks = chunk_text(&long, &cfg());
        assert!(chunks.len() > 2);
        for c in &chunks {
            assert!(estimate_tokens(&c.text) <= 60 + 10, "{}", c.text);
        }
    }

    #[test]
    fn splits_japanese_sentences() {
        let s = sentences("バクダンを倒す。カタパッドも倒す。");
        assert_eq!(s, ["バクダンを倒す。", "カタパッドも倒す。"]);
    }

    #[test]
    fn new_heading_starts_new_chunk_when_full() {
        let body = "Word ".repeat(60);
        let text = alloc::format!("# A\n\n{body}\n\n# B\n\nShort.");
        let chunks = chunk_text(&text, &cfg());
        assert_eq!(chunks.last().unwrap().heading, "B");
        assert_eq!(chunks.last().unwrap().text, "Short.");
    }
}
