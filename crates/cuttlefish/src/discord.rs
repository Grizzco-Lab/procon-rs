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
//! Messages become one document per conversation: a forum post or thread;
//! in a plain channel, a video post with the replies to it and the
//! messages that follow without a gap longer than [`CONVERSATION_GAP_S`]
//! ([`to_documents`]). Attachments (VOD clips) are kept as links. Each
//! message is also a row of the document's `messages` ([`MessageRow`]):
//! its id, the message it replies to, its thread, the video it is about
//! ([`VideoFrom`] tells how that was found) and the moments it points at
//! (video links with a time, times and waves in the text,
//! [`crate::moments`]), linked to that video.

use crate::doc::{Document, SourceKind};
use crate::game::{self, Game};
use crate::moments::{self, Moment, MomentKind};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
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
    /// The message this one replies to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// The replied-to message as the API sends it along with a reply
    /// (`referenced_message`), for when it is not among the messages read
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted: Option<Box<Message>>,
}

/// How the video a message is about was found ([`MessageRow::video_from`])
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoFrom {
    /// A YouTube link or a video attachment in the message itself
    Own,
    /// In the message it replies to, or further up the reply chain
    Reply,
    /// In the starter message of its thread or forum post
    Starter,
    /// The replied-to author's nearest earlier video post in the channel
    EarlierPost,
}

/// One message of a document, for linking comments with videos
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageRow {
    /// Snowflake id
    pub id: String,
    /// Display name of the author
    pub author: String,
    /// When it was posted
    pub time: DateTime<Utc>,
    /// The game era, from the date ([`crate::game::era`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<Game>,
    /// The message it replies to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Who wrote the replied-to message, when known
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_author: Option<String>,
    /// The thread or forum post it is in
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    /// The video it is about: a YouTube or attachment link
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_url: Option<String>,
    /// The time in that video it points at: its link's start, else the
    /// first video time written in it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_t: Option<f32>,
    /// How the video was found
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_from: Option<VideoFrom>,
    /// The video's file in the knowledge folder, when it was downloaded
    /// (`media/discord/<guild>/<channel>/<message id>/<file name>`,
    /// [`link_media`])
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_local: Option<String>,
    /// The moments it points at, with the video as their `url` when they
    /// name none
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub moments: Vec<Moment>,
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
    /// Where the messages were posted, as titles name it: `#channel`, or
    /// `#channel > thread` for a thread
    pub fn place(&self) -> String {
        match &self.parent {
            Some(p) => alloc::format!("#{p} > {}", self.name),
            None => alloc::format!("#{}", self.name),
        }
    }

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
    /// `Default`, `Reply`, `ThreadCreated`, ...
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    reference: Option<ExportReference>,
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
#[serde(rename_all = "camelCase")]
struct ExportReference {
    #[serde(default)]
    message_id: Option<String>,
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
                reply_to: (m.r#type == "Reply")
                    .then(|| m.reference.and_then(|r| r.message_id))
                    .flatten(),
                quoted: None,
            }
        })
        .collect();
    Ok((channel, messages))
}

/// Extensions of video attachments
pub const VIDEO_EXTENSIONS: [&str; 6] = ["mp4", "mov", "webm", "mkv", "m4v", "avi"];

/// A message's text with its links, where moments and videos are looked for
fn with_links(m: &Message) -> String {
    let mut text = m.content.clone();
    for l in &m.links {
        text.push('\n');
        text.push_str(l);
    }
    text
}

/// Whether a file name or link names a video file
fn is_video(s: &str) -> bool {
    let s = s
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    VIDEO_EXTENSIONS
        .iter()
        .any(|e| s.ends_with(&alloc::format!(".{e}")))
}

/// Every video a message itself links, in order: its YouTube links (each
/// with the link's start time), then its video attachments
pub fn videos_in(m: &Message) -> Vec<(String, Option<f32>)> {
    let mut out: Vec<(String, Option<f32>)> = moments::extract(&with_links(m))
        .into_iter()
        .filter_map(|x| Some((x.url?, x.seconds)))
        .collect();
    out.extend(m.links.iter().filter_map(|l| {
        let (name, url) = l.rsplit_once(": ")?;
        (is_video(name) || is_video(url)).then(|| (String::from(url), None))
    }));
    out
}

/// The video a message itself links: its first YouTube link (with the
/// link's start time), else its first video attachment
pub fn video_in(m: &Message) -> Option<(String, Option<f32>)> {
    videos_in(m).into_iter().next()
}

/// The link to a message of a channel
pub fn message_link(channel: &Channel, message_id: &str) -> String {
    alloc::format!(
        "https://discord.com/channels/{}/{}/{message_id}",
        channel.guild_id,
        channel.id
    )
}

/// A row for each message (oldest first): the reply relation, the thread
/// and the video it is about, found in this order: a link in the message
/// itself; in the message it replies to, following the chain; in its
/// thread's or post's starter message (the one whose id is the thread's);
/// else the replied-to author's nearest earlier video post in the channel
pub fn rows(channel: &Channel, messages: &[Message]) -> Vec<MessageRow> {
    let by_id: BTreeMap<&str, usize> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    let own: Vec<Option<(String, Option<f32>)>> = messages.iter().map(video_in).collect();
    let starter = channel
        .thread
        .then(|| by_id.get(channel.id.as_str()))
        .flatten()
        .and_then(|&i| own[i].clone());
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let replied = m.reply_to.as_deref().and_then(|r| by_id.get(r).copied());
            let reply_author = match replied {
                Some(j) => Some(messages[j].author.clone()),
                None => m.quoted.as_ref().map(|q| q.author.clone()),
            };
            let mut video = own[i].clone().map(|v| (v, VideoFrom::Own));
            // Up the reply chain: in the file, else what a reply quotes
            let mut at = i;
            for _ in 0..messages.len() {
                if video.is_some() {
                    break;
                }
                let r = messages[at].reply_to.as_deref();
                match r.and_then(|r| by_id.get(r)) {
                    Some(&j) => {
                        video = own[j]
                            .clone()
                            .map(|(url, _)| ((url, None), VideoFrom::Reply));
                        at = j;
                    }
                    None => {
                        video = messages[at]
                            .quoted
                            .as_deref()
                            .filter(|_| r.is_some())
                            .and_then(video_in)
                            .map(|(url, _)| ((url, None), VideoFrom::Reply));
                        break;
                    }
                }
            }
            if video.is_none() {
                video = starter
                    .clone()
                    .map(|(url, _)| ((url, None), VideoFrom::Starter));
            }
            // The replied-to author's latest video post before the reply
            // (ids are times, so this works for a message not read too)
            if video.is_none()
                && let (Some(r), Some(who)) = (m.reply_to.as_deref(), &reply_author)
            {
                let before = snowflake(r);
                video = (0..i)
                    .rev()
                    .filter(|&j| snowflake(&messages[j].id) <= before)
                    .find(|&j| &messages[j].author == who && own[j].is_some())
                    .and_then(|j| own[j].clone())
                    .map(|(url, _)| ((url, None), VideoFrom::EarlierPost));
            }
            let mut found = moments::extract(&with_links(m));
            let (video_url, mut video_t, video_from) = match video {
                Some(((url, t), from)) => (Some(url), t, Some(from)),
                None => (None, None, None),
            };
            if video_t.is_none() {
                video_t = found
                    .iter()
                    .find(|x| x.kind == MomentKind::VideoTime && x.url.is_none())
                    .and_then(|x| x.seconds);
            }
            for x in &mut found {
                x.message_id = Some(m.id.clone());
                x.author = Some(m.author.clone());
                x.at = Some(m.timestamp);
                if x.url.is_none() {
                    x.url.clone_from(&video_url);
                }
            }
            MessageRow {
                id: m.id.clone(),
                author: m.author.clone(),
                time: m.timestamp,
                game: Some(game::era(m.timestamp)),
                reply_to: m.reply_to.clone(),
                reply_author,
                thread: channel.thread.then(|| channel.id.clone()),
                video_url,
                video_t,
                video_from,
                video_local: None,
                moments: found,
            }
        })
        .collect()
}

/// The attachment id in a CDN attachment link
/// (`https://cdn.discordapp.com/attachments/<channel>/<id>/<name>?...`)
pub fn attachment_id(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if !crate::discord_media::CDN_HOSTS.contains(&host.to_lowercase().as_str()) {
        return None;
    }
    let mut parts = path.strip_prefix("attachments/")?.split('/');
    parts.next()?;
    let id = parts.next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// Points each message's `video_local` at the downloaded file of its
/// video, when `local` knows the attachment id (the channel's media
/// manifest, see [`crate::discord_media`])
pub fn link_media(docs: &mut [Document], local: &dyn Fn(&str) -> Option<String>) {
    for doc in docs {
        link_media_rows(&mut doc.messages, local);
    }
}

/// [`link_media`] for the rows of one conversation
pub fn link_media_rows(rows: &mut [MessageRow], local: &dyn Fn(&str) -> Option<String>) {
    for row in rows {
        row.video_local = row
            .video_url
            .as_deref()
            .and_then(attachment_id)
            .and_then(local);
    }
}

/// The conversations of a channel: one when `whole` (a thread or forum
/// post), else [`conversations`]
pub fn groups(messages: &[Message], whole: bool) -> Vec<Vec<usize>> {
    if whole {
        alloc::vec![(0..messages.len()).collect()]
    } else {
        conversations(messages)
    }
}

/// Conversations of a plain channel's messages (oldest first), as lists of
/// indices: a reply joins the conversation of the message it replies to; a
/// message with a video of its own starts one; any other message joins the
/// one before it, unless more than [`CONVERSATION_GAP_S`] passed
pub fn conversations(messages: &[Message]) -> Vec<Vec<usize>> {
    let by_id: BTreeMap<&str, usize> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| (m.id.as_str(), i))
        .collect();
    let mut of: Vec<usize> = Vec::with_capacity(messages.len());
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        let replied = m.reply_to.as_deref().and_then(|r| by_id.get(r).copied());
        let g = match replied {
            Some(j) if j < i => of[j],
            _ if video_in(m).is_some() => groups.len(),
            _ if i > 0
                && (m.timestamp - messages[i - 1].timestamp).num_seconds()
                    <= CONVERSATION_GAP_S =>
            {
                of[i - 1]
            }
            _ => groups.len(),
        };
        if g == groups.len() {
            groups.push(Vec::new());
        }
        groups[g].push(i);
        of.push(g);
    }
    groups
}

/// Makes a document of each conversation of messages (oldest first).
/// `whole` keeps them as one (a thread or forum post); else a video post
/// with its replies, and what follows it without a long gap, is one
/// ([`conversations`]). Each keeps its messages' [`rows`] and the game era
/// of its first message's date.
pub fn to_documents(channel: &Channel, messages: &[Message], whole: bool) -> Vec<Document> {
    to_documents_with(channel, messages, whole, &|_| None)
}

/// [`to_documents`], with what the model read in each image of a message
/// ([`crate::image_text`]) under the image's link, so the image's content
/// is found with the message; `images` gives it by attachment id
pub fn to_documents_with(
    channel: &Channel,
    messages: &[Message],
    whole: bool,
    images: &dyn Fn(&str) -> Option<String>,
) -> Vec<Document> {
    let groups = groups(messages, whole);
    let all_rows = rows(channel, messages);
    let source = if channel.is_vod_review() {
        SourceKind::DiscordVodReview
    } else {
        SourceKind::Discord
    };
    groups
        .into_iter()
        .filter(|g: &Vec<usize>| !g.is_empty())
        .map(|g| {
            let first = &messages[g[0]];
            let url = message_link(channel, &first.id);
            let title = alloc::format!(
                "{}, {}",
                channel.place(),
                first.timestamp.format("%Y-%m-%d")
            );
            let mut text = String::new();
            let mut authors: Vec<&str> = Vec::new();
            for &i in &g {
                let (m, row) = (&messages[i], &all_rows[i]);
                if !authors.contains(&m.author.as_str()) {
                    authors.push(&m.author);
                }
                let reply = match (&row.reply_to, &row.reply_author) {
                    (_, Some(who)) => alloc::format!(" \u{21aa} {who}"),
                    (Some(_), None) => String::from(" \u{21aa} ?"),
                    (None, None) => String::new(),
                };
                text.push_str(&alloc::format!(
                    "[{}] {}{reply}: {}\n",
                    m.timestamp.format("%Y-%m-%d %H:%M UTC"),
                    m.author,
                    m.content.trim()
                ));
                for l in &m.links {
                    text.push_str(&alloc::format!("  [{l}]\n"));
                    let read = l
                        .rsplit_once(": ")
                        .and_then(|(name, url)| Some((name, images(attachment_id(url)?)?)));
                    if let Some((name, read)) = read {
                        text.push_str(&alloc::format!("  Image {name}, as the model read it:\n"));
                        for line in read.lines() {
                            if !line.trim().is_empty() {
                                text.push_str("  ");
                                text.push_str(line.trim_end());
                            }
                            text.push('\n');
                        }
                    }
                }
                text.push('\n');
            }
            let mut doc = Document::new(source, &url, title, String::from(text.trim_end()));
            doc.url = Some(url);
            doc.attribution = Some(if channel.guild.is_empty() {
                authors.join(", ")
            } else {
                alloc::format!("{} ({})", authors.join(", "), channel.guild)
            });
            doc.license = Some(String::from(LICENSE));
            doc.game = Some(game::era(first.timestamp));
            doc.messages = g.iter().map(|&i| all_rows[i].clone()).collect();
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

/// Message type of a reply
const REPLY: u64 = 19;
/// Message type of a thread's first message in a text channel: an empty
/// message whose `referenced_message` is the channel message that started
/// the thread
const THREAD_STARTER: u64 = 21;

/// A message object of the API
#[derive(Deserialize)]
struct ApiMessage {
    id: String,
    #[serde(default, rename = "type")]
    kind: u64,
    #[serde(default)]
    message_reference: Option<ApiReference>,
    #[serde(default)]
    referenced_message: Option<Box<ApiMessage>>,
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
struct ApiReference {
    #[serde(default)]
    message_id: Option<String>,
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
    /// A thread's starter message becomes the channel message that started
    /// the thread, which it quotes
    fn from(m: ApiMessage) -> Self {
        if m.kind == THREAD_STARTER
            && let Some(started) = m.referenced_message
        {
            return Message::from(*started);
        }
        let reply_to = (m.kind == REPLY)
            .then(|| m.message_reference.and_then(|r| r.message_id))
            .flatten();
        let quoted = m
            .referenced_message
            .filter(|_| reply_to.is_some())
            .map(|q| Box::new(Message::from(*q)));
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
            reply_to,
            quoted,
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
        assert_eq!(docs[0].game, Some(Game::S3));
        assert_eq!(docs[0].era(), Some(Game::S3));
        assert_eq!(docs[0].video(), Some("https://cdn.example/clip.mp4"));
        assert_eq!(docs[0].messages[0].game, Some(Game::S3));
        assert_eq!(to_documents(&ch, &msgs, true).len(), 1);
    }

    #[test]
    fn every_video_of_a_message() {
        let m = Message {
            id: String::from("1"),
            timestamp: Utc::now(),
            author: String::from("a"),
            content: String::from("two runs https://youtu.be/a?t=5 and https://youtu.be/b"),
            links: alloc::vec![
                String::from("clip.MP4: https://cdn.example/clip.MP4?ex=1"),
                String::from("shot.png: https://cdn.example/shot.png"),
            ],
            reply_to: None,
            quoted: None,
        };
        let videos = videos_in(&m);
        assert_eq!(
            videos,
            [
                (String::from("https://youtu.be/a?t=5"), Some(5.0)),
                (String::from("https://youtu.be/b"), None),
                (String::from("https://cdn.example/clip.MP4?ex=1"), None),
            ]
        );
        assert_eq!(video_in(&m), videos.first().cloned());
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
        let rows = &docs[0].messages;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].id, "12");
        assert_eq!(rows[1].thread.as_deref(), Some("3"));
        assert_eq!(
            rows[1].video_url.as_deref(),
            Some("https://youtu.be/x?t=83")
        );
        assert_eq!(rows[1].video_t, Some(83.0));
        assert_eq!(rows[1].video_from, Some(VideoFrom::Own));
        let m = &rows[1].moments;
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].raw, "W1 :50");
        assert_eq!(m[0].message_id.as_deref(), Some("12"));
        assert_eq!(m[0].author.as_deref(), Some("Alice"));
        // Linked to the message's video
        assert_eq!(m[0].url.as_deref(), Some("https://youtu.be/x?t=83"));
        assert_eq!(m[1].url.as_deref(), Some("https://youtu.be/x?t=83"));
        assert_eq!(m[1].seconds, Some(83.0));
        assert!(rows[0].moments.is_empty());
    }

    /// An API message line; `extra` is more JSON fields
    fn api(id: &str, minute: u32, who: &str, text: &str, extra: &str) -> String {
        alloc::format!(
            r#"{{"id": "{id}", "timestamp": "2024-05-01T10:{minute:02}:00+00:00", "content": "{text}", "author": {{"id": "{who}", "username": "{who}"}}{extra}}}"#
        )
    }

    fn reply(to: &str) -> String {
        alloc::format!(r#", "type": 19, "message_reference": {{"message_id": "{to}"}}"#)
    }

    fn plain_channel() -> Channel {
        Channel {
            guild_id: String::from("1"),
            id: String::from("2"),
            name: String::from("vod-review"),
            ..Channel::default()
        }
    }

    #[test]
    fn videos_through_replies_and_earlier_posts() {
        let lines = [
            // souper's VOD, then a comment of hers without a video
            api("100", 0, "souper", "my run https://youtu.be/vod1", ""),
            api("101", 1, "souper", "I died at wave 2", ""),
            // A reply to the VOD post, and a reply to that reply
            api("102", 2, "Ben", "at 1:20 go left", &reply("100")),
            api("103", 3, "souper", "thanks!", &reply("102")),
            // A reply to her comment: her nearest earlier video post
            api("104", 4, "Ben", "W2 :40 you were alone", &reply("101")),
            // A reply to a message not read: its video comes quoted
            api(
                "105",
                5,
                "Cy",
                "nice",
                &alloc::format!(
                    r#"{}, "referenced_message": {}"#,
                    reply("50"),
                    api("50", 0, "Dee", "old clip", r#", "attachments": [{"url": "https://cdn.example/a.MP4?ex=1", "filename": "a.MP4"}]"#)
                ),
            ),
            // Not a reply, no video, soon after: no video, same conversation
            api("106", 6, "Cy", "gg", ""),
        ]
        .join("\n");
        let msgs = parse_api_messages(&lines);
        let ch = plain_channel();
        let rows = rows(&ch, &msgs);
        let video = |i: usize| (rows[i].video_url.as_deref(), rows[i].video_from);
        let vod1 = Some("https://youtu.be/vod1");
        assert_eq!(video(0), (vod1, Some(VideoFrom::Own)));
        assert_eq!(video(1), (None, None));
        assert_eq!(video(2), (vod1, Some(VideoFrom::Reply)));
        assert_eq!(rows[2].video_t, Some(80.0));
        assert_eq!(rows[2].reply_author.as_deref(), Some("souper"));
        assert_eq!(video(3), (vod1, Some(VideoFrom::Reply)));
        assert_eq!(video(4), (vod1, Some(VideoFrom::EarlierPost)));
        assert_eq!(rows[4].moments[0].url.as_deref(), vod1);
        assert_eq!(
            video(5),
            (
                Some("https://cdn.example/a.MP4?ex=1"),
                Some(VideoFrom::Reply)
            )
        );
        assert_eq!(rows[5].reply_author.as_deref(), Some("Dee"));
        assert_eq!(video(6), (None, None));
        assert!(rows.iter().all(|r| r.thread.is_none()));

        let docs = to_documents(&ch, &msgs, false);
        assert_eq!(docs.len(), 1);
        assert!(
            docs[0]
                .text
                .contains("[2024-05-01 10:03 UTC] souper \u{21aa} Ben: thanks!"),
            "{}",
            docs[0].text
        );
        assert!(docs[0].text.contains("Cy \u{21aa} Dee: nice"));
        assert_eq!(docs[0].messages.len(), 7);
    }

    #[test]
    fn puts_image_texts_under_their_links() {
        let cdn = |aid: &str, name: &str| {
            alloc::format!(
                r#"{{"id": "{aid}", "filename": "{name}", "url": "https://cdn.discordapp.com/attachments/2/{aid}/{name}?ex=1"}}"#
            )
        };
        let files = alloc::format!(
            r#", "attachments": [{}, {}], "embeds": [{{"title": "clip", "url": "https://youtu.be/c"}}]"#,
            cdn("71", "map.png"),
            cdn("72", "chart.png")
        );
        let lines = [
            api("100", 0, "Ka", "where to lure", &files),
            api("101", 1, "Ben", "thanks", ""),
        ]
        .join("\n");
        let msgs = parse_api_messages(&lines);
        let docs = to_documents_with(&plain_channel(), &msgs, false, &|id| {
            (id == "71").then(|| String::from("Text in the image:\nA  B\n\nIn English:\nA map"))
        });
        let text = &docs[0].text;
        assert!(
            text.contains(
                "  [map.png: https://cdn.discordapp.com/attachments/2/71/map.png?ex=1]\n  \
                 Image map.png, as the model read it:\n  Text in the image:\n  A  B\n\n  \
                 In English:\n  A map\n  [chart.png: "
            ),
            "{text}"
        );
        // An image without a text and an embed keep only their links
        assert!(text.contains("chart.png?ex=1]\n  [clip: https://youtu.be/c]\n\n[2024"));
        assert_eq!(
            to_documents(&plain_channel(), &msgs, false)[0]
                .text
                .matches("as the model read it")
                .count(),
            0
        );
    }

    #[test]
    fn plain_channels_split_by_video_posts_and_reply_chains() {
        let lines = [
            api("100", 0, "souper", "run A https://youtu.be/a", ""),
            api("101", 1, "Ben", "wave 1 was fine", ""),
            api("102", 2, "Cy", "run B https://youtu.be/b", ""),
            api("103", 3, "Ben", "1:10 in A you left", &reply("100")),
            api("104", 4, "Dee", "and in B?", ""),
            api("105", 5, "Cy", "B again", &reply("102")),
        ]
        .join("\n");
        let msgs = parse_api_messages(&lines);
        let docs = to_documents(&plain_channel(), &msgs, false);
        let ids: Vec<Vec<&str>> = docs
            .iter()
            .map(|d| d.messages.iter().map(|r| r.id.as_str()).collect())
            .collect();
        assert_eq!(
            ids,
            [
                alloc::vec!["100", "101", "103", "104"],
                alloc::vec!["102", "105"]
            ]
        );
        assert_eq!(
            docs[0].url.as_deref(),
            Some("https://discord.com/channels/1/2/100")
        );
        assert_eq!(
            docs[1].url.as_deref(),
            Some("https://discord.com/channels/1/2/102")
        );
    }

    #[test]
    fn threads_take_their_starters_video() {
        // A thread started from a channel message: its first message
        // quotes the starter, whose id is the thread's
        let starter = api("30", 0, "souper", "VOD https://youtu.be/s", "");
        let lines = [
            api(
                "31",
                1,
                "souper",
                "",
                &alloc::format!(
                    r#", "type": 21, "message_reference": {{"message_id": "30"}}, "referenced_message": {starter}"#
                ),
            ),
            api("32", 2, "Ben", "2:05 rotate", ""),
        ]
        .join("\n");
        let msgs = parse_api_messages(&lines);
        assert_eq!(msgs[0].id, "30");
        assert_eq!(msgs[0].content, "VOD https://youtu.be/s");
        let ch = Channel {
            id: String::from("30"),
            thread: true,
            ..plain_channel()
        };
        let rows = rows(&ch, &msgs);
        assert_eq!(rows[1].video_url.as_deref(), Some("https://youtu.be/s"));
        assert_eq!(rows[1].video_from, Some(VideoFrom::Starter));
        assert_eq!(rows[1].video_t, Some(125.0));
        assert_eq!(rows[1].thread.as_deref(), Some("30"));
    }

    #[test]
    fn export_replies() {
        let json = r#"{"guild": {"id": "1", "name": "G"}, "channel": {"id": "2", "name": "c"},
          "messages": [
            {"id": "10", "timestamp": "2024-05-01T10:00:00+00:00", "content": "https://youtu.be/q", "author": {"name": "a"}},
            {"id": "11", "type": "Reply", "reference": {"messageId": "10"}, "timestamp": "2024-05-01T10:01:00+00:00", "content": "ok", "author": {"name": "b"}}
          ]}"#;
        let (ch, msgs) = parse_export(json).unwrap();
        assert_eq!(msgs[1].reply_to.as_deref(), Some("10"));
        let rows = rows(&ch, &msgs);
        assert_eq!(rows[1].video_from, Some(VideoFrom::Reply));
        assert_eq!(rows[1].reply_author.as_deref(), Some("a"));
    }
}
