//! Discord conversations, from an export file, the official bot API or an
//! archive made by `cuttlefish fetch discord`.
//!
//! Three ways in:
//!
//! 1. An export file: DiscordChatExporter's JSON format, made by someone with
//!    access to the channel (and the server's permission) and handed over.
//! 2. The bot API: a server admin adds a bot (with the Message Content
//!    intent, and View Channel + Read Message History on the channel) and
//!    gives its token through the `DISCORD_BOT_TOKEN` environment variable.
//! 3. An archive of raw API messages fetched slowly with the user's own
//!    account ([`crate::discord_fetch`]; against Discord's terms, at the
//!    user's own risk), read with [`read_archive`].
//!
//! Messages become one document per conversation: a forum post or thread,
//! or a run of channel messages without a gap longer than
//! [`CONVERSATION_GAP_S`]. Attachments (VOD clips) are kept as links, and
//! the moments the messages point at (video links with a time, times and
//! waves in the text, [`crate::moments`]) as the document's `moments`.

use crate::doc::{Document, SourceKind};
use crate::moments;
use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use core::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// A pause longer than this starts a new conversation
pub const CONVERSATION_GAP_S: i64 = 2 * 3600;
/// Terms recorded with Discord documents
pub const LICENSE: &str = "Discord messages by their authors; shared in a private community: study use only, do not republish";

/// One message
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Snowflake id
    pub id: String,
    /// When it was posted
    pub timestamp: DateTime<Utc>,
    /// Display name of the author
    pub author: String,
    /// Text
    pub content: String,
    /// Attachment and embed links (`name: url`)
    pub links: Vec<String>,
}

/// Where the messages were posted
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    /// Server id (for message links)
    pub guild_id: String,
    /// Server name
    pub guild: String,
    /// Channel id
    pub id: String,
    /// Channel (or thread) name
    pub name: String,
    /// Parent channel name of a thread
    pub parent: Option<String>,
    /// A thread or forum post: one conversation
    #[serde(default)]
    pub thread: bool,
}

/// Channel types of threads (announcement, public, private)
const THREAD_TYPES: [u64; 3] = [10, 11, 12];
/// Channel types without messages of their own: forum and media channels,
/// whose posts are threads
pub const FORUM_TYPES: [u64; 2] = [15, 16];

impl Channel {
    /// True for the #vod-review channel and its threads
    pub fn is_vod_review(&self) -> bool {
        let is = |n: &str| n.to_lowercase().contains("vod-review");
        is(&self.name) || self.parent.as_deref().is_some_and(is)
    }

    /// From a channel object of the API, with the parent channel's name
    /// for a thread; the server's name is not in it
    pub fn from_api(c: &Value, parent: Option<String>) -> Channel {
        let text = |key: &str| String::from(c[key].as_str().unwrap_or_default());
        Channel {
            guild_id: text("guild_id"),
            guild: String::new(),
            id: text("id"),
            name: text("name"),
            parent,
            thread: c["type"]
                .as_u64()
                .is_some_and(|t| THREAD_TYPES.contains(&t)),
        }
    }
}

#[derive(Deserialize)]
struct ExportFile {
    guild: ExportGuild,
    channel: ExportChannel,
    messages: Vec<ExportMessage>,
}

#[derive(Deserialize)]
struct ExportGuild {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct ExportChannel {
    id: String,
    name: String,
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportMessage {
    id: String,
    timestamp: DateTime<Utc>,
    #[serde(default)]
    content: String,
    author: ExportAuthor,
    #[serde(default)]
    attachments: Vec<ExportAttachment>,
    #[serde(default)]
    embeds: Vec<Embed>,
}

#[derive(Deserialize)]
struct ExportAuthor {
    name: String,
    #[serde(default)]
    nickname: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportAttachment {
    url: String,
    file_name: String,
}

#[derive(Deserialize)]
struct Embed {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

fn embed_links(embeds: &[Embed]) -> impl Iterator<Item = String> + '_ {
    embeds.iter().filter_map(|e| {
        let url = e.url.as_deref()?;
        Some(alloc::format!(
            "{}: {url}",
            e.title.as_deref().unwrap_or("link")
        ))
    })
}

/// Parses a DiscordChatExporter JSON export
pub fn parse_export(json: &str) -> Result<(Channel, Vec<Message>)> {
    let f: ExportFile =
        serde_json::from_str(json).context("not a DiscordChatExporter JSON export")?;
    let channel = Channel {
        guild_id: f.guild.id,
        guild: f.guild.name,
        id: f.channel.id,
        name: f.channel.name,
        parent: f.channel.category,
        thread: f.channel.r#type.contains("Thread"),
    };
    let messages = f
        .messages
        .into_iter()
        .map(|m| {
            let mut links: Vec<String> = m
                .attachments
                .iter()
                .map(|a| alloc::format!("{}: {}", a.file_name, a.url))
                .collect();
            links.extend(embed_links(&m.embeds));
            Message {
                id: m.id,
                timestamp: m.timestamp,
                author: m.author.nickname.unwrap_or(m.author.name),
                content: m.content,
                links,
            }
        })
        .collect();
    Ok((channel, messages))
}

/// Splits messages (oldest first) into conversations and makes a document of
/// each. `whole` keeps them as one conversation (a thread or forum post).
pub fn to_documents(channel: &Channel, messages: &[Message], whole: bool) -> Vec<Document> {
    let mut groups: Vec<&[Message]> = Vec::new();
    let mut start = 0;
    for i in 1..messages.len() {
        let gap = (messages[i].timestamp - messages[i - 1].timestamp).num_seconds();
        if !whole && gap > CONVERSATION_GAP_S {
            groups.push(&messages[start..i]);
            start = i;
        }
    }
    if start < messages.len() {
        groups.push(&messages[start..]);
    }
    let source = if channel.is_vod_review() {
        SourceKind::DiscordVodReview
    } else {
        SourceKind::Discord
    };
    groups
        .into_iter()
        .map(|g| {
            let first = &g[0];
            let url = alloc::format!(
                "https://discord.com/channels/{}/{}/{}",
                channel.guild_id,
                channel.id,
                first.id
            );
            let place = match &channel.parent {
                Some(p) => alloc::format!("#{p} > {}", channel.name),
                None => alloc::format!("#{}", channel.name),
            };
            let title = alloc::format!("{place}, {}", first.timestamp.format("%Y-%m-%d"));
            let mut text = String::new();
            let mut authors: Vec<&str> = Vec::new();
            let mut found = Vec::new();
            for m in g {
                if !authors.contains(&m.author.as_str()) {
                    authors.push(&m.author);
                }
                text.push_str(&alloc::format!(
                    "[{}] {}: {}\n",
                    m.timestamp.format("%Y-%m-%d %H:%M UTC"),
                    m.author,
                    m.content.trim()
                ));
                for l in &m.links {
                    text.push_str(&alloc::format!("  [{l}]\n"));
                }
                text.push('\n');
                // Moments in the text and in the links (embeds of a video)
                let mut with_links = m.content.clone();
                for l in &m.links {
                    with_links.push('\n');
                    with_links.push_str(l);
                }
                for mut moment in moments::extract(&with_links) {
                    moment.message_id = Some(m.id.clone());
                    moment.author = Some(m.author.clone());
                    moment.at = Some(m.timestamp);
                    found.push(moment);
                }
            }
            let mut doc = Document::new(source, &url, title, String::from(text.trim_end()));
            doc.url = Some(url);
            doc.attribution = Some(if channel.guild.is_empty() {
                authors.join(", ")
            } else {
                alloc::format!("{} ({})", authors.join(", "), channel.guild)
            });
            doc.license = Some(String::from(LICENSE));
            doc.moments = found;
            doc
        })
        .collect()
}

/// The Discord REST API with a bot token
pub struct Bot {
    agent: ureq::Agent,
    token: String,
}

/// The REST API
pub const API: &str = "https://discord.com/api/v10";

/// A message object of the API
#[derive(Deserialize)]
struct ApiMessage {
    id: String,
    timestamp: DateTime<Utc>,
    #[serde(default)]
    content: String,
    author: ApiUser,
    #[serde(default)]
    attachments: Vec<ApiAttachment>,
    #[serde(default)]
    embeds: Vec<Embed>,
}

#[derive(Deserialize)]
struct ApiUser {
    username: String,
    #[serde(default)]
    global_name: Option<String>,
}

#[derive(Deserialize)]
struct ApiAttachment {
    url: String,
    filename: String,
}

impl From<ApiMessage> for Message {
    fn from(m: ApiMessage) -> Self {
        let mut links: Vec<String> = m
            .attachments
            .iter()
            .map(|a| alloc::format!("{}: {}", a.filename, a.url))
            .collect();
        links.extend(embed_links(&m.embeds));
        Message {
            id: m.id,
            timestamp: m.timestamp,
            author: m.author.global_name.unwrap_or(m.author.username),
            content: m.content,
            links,
        }
    }
}

/// Numeric value of a snowflake id, for ordering
pub fn snowflake(id: &str) -> u64 {
    id.parse().unwrap_or_default()
}

/// Messages from JSON lines of API message objects (a `.messages.jsonl`
/// of `cuttlefish fetch discord`), oldest first, each id once; lines that
/// are not messages are skipped
pub fn parse_api_messages(jsonl: &str) -> Vec<Message> {
    let mut messages: Vec<Message> = jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ApiMessage>(l).ok())
        .map(Message::from)
        .collect();
    messages.sort_by_key(|m| snowflake(&m.id));
    messages.dedup_by(|a, b| a.id == b.id);
    messages
}

/// Reads a channel or thread of a `cuttlefish fetch discord` archive: the
/// messages file (`<id>.messages.jsonl`) with the channel object beside it
/// (`<id>.channel.json`) and, for a thread, the parent's channel object
/// beside it or one folder up (threads are kept in `threads/`)
pub fn read_archive(messages_path: &Path) -> Result<(Channel, Vec<Message>)> {
    let name = messages_path
        .file_name()
        .context("not a file")?
        .to_string_lossy()
        .into_owned();
    let id = name
        .strip_suffix(".messages.jsonl")
        .with_context(|| alloc::format!("{name}: not a <id>.messages.jsonl file"))?;
    let dir = messages_path.parent().context("no folder")?;
    let read_channel = |dir: &Path, id: &str| -> Option<Value> {
        let bytes = std::fs::read(dir.join(alloc::format!("{id}.channel.json"))).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    let info = read_channel(dir, id).unwrap_or_else(|| serde_json::json!({ "id": id }));
    let parent = info["parent_id"].as_str().and_then(|p| {
        read_channel(dir, p)
            .or_else(|| read_channel(dir.parent()?, p))
            .and_then(|c| c["name"].as_str().map(String::from))
    });
    let mut channel = Channel::from_api(&info, parent);
    if channel.name.is_empty() {
        channel.name = String::from(id);
    }
    let text = std::fs::read_to_string(messages_path)
        .with_context(|| alloc::format!("reading {}", messages_path.display()))?;
    Ok((channel, parse_api_messages(&text)))
}

impl Bot {
    /// A client with the token from `DISCORD_BOT_TOKEN`
    pub fn from_env() -> Result<Self> {
        let token = std::env::var("DISCORD_BOT_TOKEN")
            .ok()
            .filter(|t| !t.is_empty())
            .context(
                "DISCORD_BOT_TOKEN is not set; a server admin has to add a bot and share its token",
            )?;
        let agent = ureq::Agent::config_builder()
            .user_agent(alloc::format!(
                "DiscordBot (https://github.com/crazyboycjr/procon-rs, {})",
                env!("CARGO_PKG_VERSION")
            ))
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Bot { agent, token })
    }

    /// GET an API path, waiting out rate limits
    fn get(&self, path: &str) -> Result<serde_json::Value> {
        loop {
            let mut resp = self
                .agent
                .get(alloc::format!("{API}{path}"))
                .header("Authorization", alloc::format!("Bot {}", self.token))
                .call()?;
            let header = |name: &str| {
                resp.headers()
                    .get(name)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
            };
            let remaining = header("x-ratelimit-remaining");
            let reset_after = header("x-ratelimit-reset-after");
            let status = resp.status().as_u16();
            let body: serde_json::Value = resp.body_mut().read_json().unwrap_or_default();
            if status == 429 {
                let wait = body["retry_after"].as_f64().unwrap_or(1.0);
                std::thread::sleep(Duration::from_secs_f64(wait.clamp(0.1, 60.0)));
                continue;
            }
            if !(200..300).contains(&status) {
                bail!("Discord API {path}: HTTP {status}: {body}");
            }
            if remaining == Some(0.0) {
                std::thread::sleep(Duration::from_secs_f64(
                    reset_after.unwrap_or(1.0).clamp(0.0, 60.0),
                ));
            }
            return Ok(body);
        }
    }

    /// A channel or thread, with its server and parent names
    pub fn channel(&self, id: &str) -> Result<Channel> {
        let c = self.get(&alloc::format!("/channels/{id}"))?;
        let guild_id = String::from(c["guild_id"].as_str().unwrap_or_default());
        let guild = self.get(&alloc::format!("/guilds/{guild_id}"))?;
        let parent = match c["parent_id"].as_str() {
            Some(p) => self.get(&alloc::format!("/channels/{p}"))?["name"]
                .as_str()
                .map(String::from),
            None => None,
        };
        let mut channel = Channel::from_api(&c, parent);
        channel.guild_id = guild_id;
        channel.guild = String::from(guild["name"].as_str().unwrap_or_default());
        channel.id = String::from(id);
        Ok(channel)
    }

    /// Every message of a channel or thread, oldest first
    pub fn messages(&self, channel_id: &str) -> Result<Vec<Message>> {
        let mut out: Vec<Message> = Vec::new();
        let mut after = String::from("0");
        loop {
            let page = self.get(&alloc::format!(
                "/channels/{channel_id}/messages?limit=100&after={after}"
            ))?;
            let mut batch: Vec<ApiMessage> = serde_json::from_value(page)?;
            if batch.is_empty() {
                return Ok(out);
            }
            // Newest first within a page
            batch.reverse();
            after = batch.last().map(|m| m.id.clone()).unwrap_or_default();
            out.extend(batch.into_iter().map(Message::from));
        }
    }

    /// Ids of a channel's threads (forum posts): active and archived public
    pub fn threads(&self, channel: &Channel) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let active = self.get(&alloc::format!(
            "/guilds/{}/threads/active",
            channel.guild_id
        ))?;
        for t in active["threads"].as_array().into_iter().flatten() {
            if t["parent_id"].as_str() == Some(channel.id.as_str()) {
                ids.extend(t["id"].as_str().map(String::from));
            }
        }
        let mut before: Option<String> = None;
        loop {
            let q = before
                .as_deref()
                .map(|b| alloc::format!("&before={}", crate::crawl::encode(b)))
                .unwrap_or_default();
            let page = self.get(&alloc::format!(
                "/channels/{}/threads/archived/public?limit=100{q}",
                channel.id
            ))?;
            let threads = page["threads"].as_array().cloned().unwrap_or_default();
            for t in &threads {
                ids.extend(t["id"].as_str().map(String::from));
            }
            before = threads
                .last()
                .and_then(|t| t["thread_metadata"]["archive_timestamp"].as_str())
                .map(String::from);
            if !page["has_more"].as_bool().unwrap_or(false) || before.is_none() {
                return Ok(ids);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"{
      "guild": {"id": "1", "name": "Overfishing", "iconUrl": ""},
      "channel": {"id": "2", "type": "GuildTextChat", "categoryId": "9",
                  "category": "Salmon Run", "name": "vod-review", "topic": null},
      "messages": [
        {"id": "10", "type": "Default", "timestamp": "2024-05-01T10:00:00+00:00",
         "content": "Wave 2 here: why did the basket starve?",
         "author": {"id": "5", "name": "alice", "nickname": "Alice"},
         "attachments": [{"id": "7", "url": "https://cdn.example/clip.mp4", "fileName": "clip.mp4"}],
         "embeds": []},
        {"id": "11", "timestamp": "2024-05-01T10:05:00+00:00",
         "content": "Two players chased Steelheads on the far side.",
         "author": {"id": "6", "name": "bob"}},
        {"id": "12", "timestamp": "2024-05-02T09:00:00+00:00",
         "content": "Next day: new clip",
         "author": {"id": "5", "name": "alice", "nickname": "Alice"},
         "embeds": [{"title": "Run 3", "url": "https://youtu.be/x"}]}
      ]
    }"#;

    #[test]
    fn parses_exports() {
        let (ch, msgs) = parse_export(EXPORT).unwrap();
        assert_eq!(ch.name, "vod-review");
        assert!(ch.is_vod_review());
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].author, "Alice");
        assert_eq!(msgs[1].author, "bob");
        assert_eq!(msgs[0].links, ["clip.mp4: https://cdn.example/clip.mp4"]);
        assert_eq!(msgs[2].links, ["Run 3: https://youtu.be/x"]);
    }

    #[test]
    fn splits_conversations_at_gaps() {
        let (ch, msgs) = parse_export(EXPORT).unwrap();
        let docs = to_documents(&ch, &msgs, false);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].source, SourceKind::DiscordVodReview);
        assert_eq!(docs[0].title, "#Salmon Run > vod-review, 2024-05-01");
        assert_eq!(
            docs[0].url.as_deref(),
            Some("https://discord.com/channels/1/2/10")
        );
        assert!(
            docs[0]
                .text
                .contains("[2024-05-01 10:05 UTC] bob: Two players chased")
        );
        assert!(
            docs[0]
                .text
                .contains("[clip.mp4: https://cdn.example/clip.mp4]")
        );
        assert_eq!(
            docs[0].attribution.as_deref(),
            Some("Alice, bob (Overfishing)")
        );
        assert_eq!(to_documents(&ch, &msgs, true).len(), 1);
    }

    const LINES: &str = r#"{"id": "12", "channel_id": "3", "timestamp": "2024-05-02T09:00:00+00:00", "content": "W1 :50 see https://youtu.be/x?t=83", "author": {"id": "5", "username": "alice", "global_name": "Alice"}, "attachments": [{"id": "7", "url": "https://cdn.example/clip.mp4", "filename": "clip.mp4"}]}
{"id": "10", "channel_id": "3", "timestamp": "2024-05-01T10:00:00+00:00", "content": "first", "author": {"id": "6", "username": "bob"}}
{"id": "12", "channel_id": "3", "timestamp": "2024-05-02T09:00:00+00:00", "content": "W1 :50 see https://youtu.be/x?t=83", "author": {"id": "5", "username": "alice", "global_name": "Alice"}}
not json
"#;

    #[test]
    fn reads_archives_with_moments() {
        let dir = std::env::temp_dir().join(alloc::format!(
            "cuttlefish-discord-archive-{}",
            std::process::id()
        ));
        let threads = dir.join("threads");
        std::fs::create_dir_all(&threads).unwrap();
        std::fs::write(
            dir.join("2.channel.json"),
            r#"{"id": "2", "type": 15, "guild_id": "1", "name": "vod-review"}"#,
        )
        .unwrap();
        std::fs::write(
            threads.join("3.channel.json"),
            r#"{"id": "3", "type": 11, "guild_id": "1", "parent_id": "2", "name": "Run 3 wipe"}"#,
        )
        .unwrap();
        std::fs::write(threads.join("3.messages.jsonl"), LINES).unwrap();
        let (ch, msgs) = read_archive(&threads.join("3.messages.jsonl")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(ch.thread);
        assert_eq!(ch.parent.as_deref(), Some("vod-review"));
        assert!(ch.is_vod_review());
        // Sorted, once each, the bad line dropped
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].author, "bob");
        assert_eq!(msgs[1].author, "Alice");
        assert_eq!(msgs[1].links, ["clip.mp4: https://cdn.example/clip.mp4"]);
        let docs = to_documents(&ch, &msgs, ch.thread);
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].source, SourceKind::DiscordVodReview);
        assert_eq!(docs[0].title, "#vod-review > Run 3 wipe, 2024-05-01");
        assert_eq!(docs[0].attribution.as_deref(), Some("bob, Alice"));
        let m = &docs[0].moments;
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].raw, "W1 :50");
        assert_eq!(m[0].message_id.as_deref(), Some("12"));
        assert_eq!(m[0].author.as_deref(), Some("Alice"));
        assert_eq!(m[1].url.as_deref(), Some("https://youtu.be/x?t=83"));
        assert_eq!(m[1].seconds, Some(83.0));
    }
}
