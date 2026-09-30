# Cuttlefish

Knowledge store and model backend for Cuttlefish, Grizzco Lab's AI reviewer
for Splatoon 3 Salmon Run. It imports guides, wikis, video transcripts and
Discord VOD-review discussions into a local store; retrieves what matters for
a moment of gameplay or a question; and asks Claude (Anthropic Messages API)
with the frames, the retrieved knowledge and a jargon glossary. On the Claude
Code CLI the model looks the store up itself instead, with read-only
knowledge tools (see [Knowledge tools](#knowledge-tools-the-model-looks-things-up)).
Library (`cuttlefish`) plus a CLI of the same name.

## Quick start

```bash
cargo build --release -p cuttlefish
alias cuttlefish=target/release/cuttlefish

# The knowledge folder is the lab's: found through its config (--config,
# else ./config.toml: [cuttlefish] knowledge, else "Knowledge" next to the
# sessions), else --data or $CUTTLEFISH_DATA; without any, the CLI stops
cd ~/Developing/procon-rs    # where config.toml is
export CUTTLEFISH_CONTACT=you@example.org   # put in the crawler's User-Agent

cuttlefish ingest inbox                    # everything dropped into <data>/inbox/
cuttlefish ingest inbox --reimport 'stat.ink-3.128.4.zip'   # forget what a file gave, read it again

cuttlefish ingest file fundamentals.pdf --source guide --license "by permission of the authors"
cuttlefish ingest url https://example.org/guide --source guide
cuttlefish ingest url --sitemap https://example.org/sitemap.xml --max-pages 100
cuttlefish ingest url "https://docs.google.com/spreadsheets/d/<id>/edit" --all-tabs   # every tab of a sheet
cuttlefish ingest wiki https://splatoonwiki.org/wiki/Category:Salmon_Run --depth 2 --dry-run
cuttlefish ingest site https://example.org/ --skip /app/ --max-pages 50 --dry-run
cuttlefish ingest youtube https://www.youtube.com/playlist?list=...
cuttlefish ingest discord-export vod-review.json
DISCORD_BOT_TOKEN=... cuttlefish ingest discord-bot --channel 123456789012345678
cuttlefish fetch discord --channel https://discord.com/channels/<server>/<channel> --count   # your own account: against Discord's terms, see below
cuttlefish fetch discord --channel ... --attachments videos    # also download the uploaded VODs (see "Downloading the VODs")
cuttlefish read-images --effort medium     # the downloaded images as text, read once each by the model (see "Reading the images")
cuttlefish corpus build                    # the reviewed VODs of the archive as corpus/vod-review.jsonl, with counts
cuttlefish corpus videos --list            # the YouTube VODs with their 480p sizes; `corpus videos` downloads them slowly
cuttlefish corpus align                    # read the HUD of the videos on disk (wave tables), place the wave-timer moments
cuttlefish corpus reviews                  # a review in the lab per VOD on disk, the community's comments at their moments
cuttlefish corpus index                    # every reviewer comment as an expert comment of its own in the store
cuttlefish corpus retrieval                # how well a moment's summary alone finds expert comments (no model)
cuttlefish ingest leanny --dry-run         # Lean's Splatoon 3 datamine: list the files, fetch nothing
cuttlefish ingest leanny                   # fact cards of exact game numbers and the Eggstra Work events; re-runs fetch only changed files

cuttlefish search "バクダンの処理"          # top-k chunks with sources; any language
cuttlefish search "Eggstra Work #7 wave 3" --mode keyword   # or embedding; hybrid by default
cuttlefish glossary "Steelhead"            # a term's names and definition
cuttlefish stats                           # documents, terms per language, tables, assets
cuttlefish docs                            # every document with its id
cuttlefish delete 4d7e4072ed28b64e         # a document and its chunks
cuttlefish eval eval.example.toml          # retrieval check, no key needed
cuttlefish eval retrieval                  # the crate's retrieval set: recall@1/5 of embeddings, BM25, hybrid

export ANTHROPIC_API_KEY=...               # only ever from the environment, or the env file
                                           # scripts/run.sh loads (~/.config/procon/env)
# ...or none: with the Claude Code CLI installed and logged in, `auto` (the
# default) runs `claude -p` on your own subscription; --backend claude-cli forces it
cuttlefish ask "When should I leave the basket to kill a Stinger?"   # on the CLI the model looks it up itself
cuttlefish ask "绿帽怪的炸弹多久爆炸？" --no-tools   # the one-shot way: retrieved excerpts go with it
cuttlefish mcp                             # the knowledge tools on stdio, for any MCP client
cuttlefish translate "Kill the Steelhead before the Flyfish" --to ja
cuttlefish eval eval.example.toml --answer
cuttlefish eval deep --lang zh --max 5       # the deep question bank; answers into <data>/eval/, reviewed in the lab
cuttlefish eval deep --dry-run --lang zh     # what retrieval gives each bank question, no model asked
```

The first command that embeds downloads the embedding model (about 470 MB)
into `~/.cache/procon-cuttlefish/models/` (`$CUTTLEFISH_CACHE`, else
`$XDG_CACHE_HOME/procon-cuttlefish`), on this machine rather than in a synced
data folder. `--model` / `$CUTTLEFISH_MODEL` picks the model
(default `claude-opus-5-5`, on either backend), `--effort` its effort
(default `medium`). Every answer kept (eval rows, the lab's chat messages)
records the backend, the model that answered and the effort.
`RUST_LOG=debug` shows more.

## Data folder

```text
<data>/
  inbox/             drop anything here (see "The inbox")
  inbox.json         what each inbox file gave, with its size, time and hash
  glossary.toml      your glossary; the crate's glossary.toml seed until you add one
  glossary-user.toml slang and new terms you taught, approved or auto-applied, and your
                     edits of glossary terms' definitions and relations (never overwritten)
  notes/<id>.md      expert notes: your corrections, Markdown with front matter (see
                     "Deep questions and expert notes"); the most trusted source
  eval/deep-<date>.jsonl
                     answers of `eval deep` runs, with your verdicts
  terms/<id>.json    name tables imported from the inbox, merged into the glossary
  assets.json        images and icons from the inbox
  reports/<t>.json   one report per inbox import (the last 30)
  digest.md          curated fundamentals, sent with every request (optional)
  inbox/x/<handle>/posts.jsonl
                     the Salmon Run threads of an account you follow on X, captured
                     by tools/capture/xcap.mjs; inbox/x/state.json is its state
  inbox/rednote/<user id>/notes.jsonl
                     the Salmon Run notes of a Xiaohongshu creator you follow, with
                     their comments, captured by tools/capture/rednote.mjs;
                     inbox/rednote/state.json is its state
  media/discord/<guild>/<channel>/<message id>/<file>
                     VODs and images downloaded by `fetch discord --attachments`,
                     with a media.jsonl manifest per channel (large: see below)
  media/image-text.jsonl
                     what the model read in those images, by their SHA-256
                     (`read-images`, see "Reading the images")
  media/youtube/<id>.mp4, <id>.info.json
                     the corpus's YouTube VODs at 480p (`corpus videos`) with
                     title, channel, duration, size, or why one is unavailable
  <video stem>.wave_starts.json
                     beside a video: its waves read from the HUD (`corpus align`)
  corpus/vod-review.jsonl
                     the #vod-review corpus, one reviewed VOD per line (`corpus build`)
  corpus/eggstra_events.json
                     the Eggstra Work events: dates, stage, weapons, waves (`ingest leanny`)
  raw/<kind>/        pages, subtitles, exports as received; raw/leanny/ holds
                     Lean's data files with their ETags (state.json)
  docs/<id>.json     processed documents with source, url or inbox path, title,
                     language, license, attribution, revision (wiki pages),
                     fetch time, weight and text
  index/             meta.json, entries.jsonl, vectors.f32, keywords.json
                     (the BM25 index, in the entries' order; rebuilt from
                     entries.jsonl when missing or out of step)
~/.cache/procon-cuttlefish/ models/, thumbs/, unpack/: on this machine only
```

Ingesting the same url again replaces its document (`--refresh` refetches
pages already stored). Opening the store embeds the documents the index
lacks, 32 chunks at a time; the lab shows "embedding N of M chunks" in the
running import and stops when the import is cancelled (what was embedded is
kept, the rest waits for the next opening). `cuttlefish reindex` re-embeds everything after a
change of embedder or chunk sizes; `cuttlefish delete <id>` removes a document.

The data folder can be a synced folder (Dropbox, rclone mount). Every file is
written whole (temporary file, then rename). Files that do not read
(half-synced, `(conflicted copy)`) are skipped with a warning. Documents are
the truth: opening the store drops chunks of documents that are gone, rebuilds
an index that does not read, and embeds documents that arrived from another
machine. The index is rewritten every 50 documents of an import and at its
end. The folder's parent must exist, so an unmounted synced folder is not
silently replaced.

**One writer at a time.** Every writer (`ingest`, `delete` and `reindex` of
the CLI; the lab's imports and deletes) holds `<data>/.lock` while it
writes: an exclusive `flock`, which the system releases when the process
ends however it ends, and a record of who holds it (program, pid, host, start
time), cleared when done. A second writer stops with

```
the knowledge store is being written by cuttlefish ingest pid 41234 on studio-pc since 2026-09-26 21:04:10 UTC; wait for it, or if that process is gone, delete /path/to/Knowledge/.lock
```

and the lab fails the import job with the same line. `flock` does not
reach across machines, so on a synced folder the record does: a record naming
another host stops writers here too, until that machine's writer clears it
(or you delete the file after checking that nothing runs there). A record
left by a process of this machine that is gone is taken over. Searching,
asking, `stats` and `docs` never wait for the lock.

The data folder of before (`$XDG_DATA_HOME/cuttlefish`, usually
`~/.local/share/cuttlefish`) is shared with another program, so
`store::migrate` (the lab at startup, the CLI on each run) handles only our
entries there: `docs/`, `index/`, `raw/`, `terms/`, `reports/`, `inbox/`,
`glossary.toml`, `digest.md`, `assets.json`, `inbox.json` and `models/`. Their
data is copied into the knowledge folder if that holds none and checked (every
file with its size); then they are moved into
`procon-migrated-<date>.safe-to-delete/` in that folder, the model too (it is
downloaded again into the cache). Anything else there stays untouched, nothing
is deleted, and nothing moves when the copy differs. `Store::open` refuses
that folder.

## The inbox

`<data>/inbox/` takes anything: files, folders, zip or tar archives, source
repositories. `cuttlefish ingest inbox` (or **Import inbox** in the lab,
which can also upload into it) looks at each file by name and first bytes:

| File | Taken as |
|---|---|
| md, txt, rst, org, adoc, html, pdf, docx, srt, vtt | a document (chunked, embedded), keyed by its inbox path |
| DiscordChatExporter JSON, or `<id>.messages.jsonl` of `fetch discord` (with its `<id>.channel.json` beside it) | its conversations; the fetcher's channel objects and `state.json` are skipped |
| `x/<handle>/posts.jsonl` of `tools/capture/xcap.mjs` | one document per thread (the post, its quoted post, the replies); the capture's `x/state.json` is skipped |
| `rednote/<user id>/notes.jsonl` of `tools/capture/rednote.mjs` | one document per Xiaohongshu note with its comments (source kind `rednote`); the capture's `rednote/state.json` is skipped |
| json, yaml, toml, csv, tsv, po, properties | a name table if it holds the same keys in several languages; never embedded. Otherwise a data table: a small text document of `key / path: value` lines under 1 MB, skipped above. Project configuration (`package.json`, `Cargo.toml`, ...) is skipped |
| php in a message folder (`messages/<lang>/<category>.php`, as Yii apps such as stat.ink keep them) | a name table: the keys are the English names, the values the translations. Interface categories (`app`, `email`, `privacy`, time zones, ...) and machine-translated folders (`_deepl`) are skipped; other `.php` is code |
| png, jpg, gif, webp, svg, bmp, ico, avif | an asset; site images (folders named after logos, screenshots, clip art, "about") are skipped |
| zip, tar(.gz/.xz/.bz2/.zst), 7z | unpacked with `bsdtar` into the cache, contents taken the same way |
| code, media, office files other than docx, fonts, binaries, unknown | skipped, with the reason |

Hidden files and folders (`.git`, partial uploads) and `node_modules`,
`target`, `build`, `dist`, `vendor` and the like are never entered; lock files
and license/changelog files are skipped. So a source tree gives its README and
docs as documents and its locale and string files as name tables.

**Name tables** (`tables.rs`). Structured files are flattened into key paths.
The language of a string comes from the file's path (`locales/ja/weapons.json`,
`JPja.json`, `strings_ko.po`; files differing only by it are read together as
one table, `locales/*/weapons.json`) or from a key (`en`, `name_ja`,
`Japanese`, a CSV column `Chinese (Simplified)`); Nintendo region codes
(`USen`, `EUfr`, `CNzh`, `TWzh`, ...) are known. A key with names in two
languages or more becomes a term with those names and its origin
(`from = ["inbox/<file>#<key>"]`). Only names are kept (one line, at most 60
characters, no markup or placeholders, 3+ characters if ASCII); in a table of
more than 500 terms, only keys containing "name" (a game's whole interface
text otherwise floods the glossary with "OK" and "Back"), unless the keys are
the names themselves (PHP messages, gettext). Tables are stored per file or
family in `terms/` and merged into the glossary: a term with an English name
of an existing one adds its languages to it (the seed's Steelhead gains
French and Chinese), a term without English names merges on the id or any
shared name, others are added as new terms. English decides because
localized names are shared more often (Splattershot and the Shooter class are
both "Lanzatintas" in Spanish).

**Message folders** (`messages.rs`, `php.rs`). One folder per language, one
file per category, as stat.ink's `messages/zh-CN/salmon-boss3.php`. The PHP
array is read as literals (single or double quotes, escapes, comments, `.`
concatenation, nested arrays, `array_merge(...)`), never run; the file's
`@license` and `@copyright` are kept on the table. Regional variants (`en-GB`,
`es-MX`, `fr-CA`, `pt-BR`; Nintendo's `EUen`, `USes`, `USfr`) are folded into
their language, with the names that differ kept under the variant's own code
(`forms.en = ["Armor Jacket Replica"], forms.en-GB = ["Armour Jacket
Replica"]`), so `name("es")` is the Spanish name and a lookup finds the
Mexican one too; a variant without its language (only `pt-BR`) is the
language. The category names the kind of the terms (`salmon-boss3` → `boss`,
`map3` → `stage`, `weapon3` → `weapon`, `special3`, `subweapon3` → `sub`,
`ability3`, `salmon-event3` → `event`, `salmon-title3`, `salmon-uniform3`,
...) and their game (`3` → S3, `2` → S2, no suffix beside a suffixed sibling →
S1); both go on the terms (`kind`, `game`) and into prompts (`[boss]`, `[stage,
Splatoon 2]`). Tables of Splatoon 3 merge into the glossary first, then
untagged ones, then older games, so an older name never comes before the
current one. A table's main name that is none of the official names of the
glossary's own term (`glossary.toml` or the seed) is listed in the import
report (`name differs: steelhead: zh "炸弹鱼" here, "铁盔" in ...`); both
names are kept, the glossary's first. The seed follows stat.ink's
Simplified Chinese names, so stat.ink's tables report no Chinese
differences.

**Assets** (`assets.rs`). Each image gets an entry in `assets.json`: path,
size, dimensions from its header, a name from the file name and its folder,
and the glossary term its file name names (the whole name or its end, matched
against term ids, names and imported keys: `Wst_Shooter_Normal_00.png` is the
term whose key is `Shooter_Normal_00`). Thumbnails are made by ffmpeg on
request into the cache; small icons and SVGs are served as they are. Images
are not embedded; an image embedder (CLIP, SigLIP) could make them searchable
by content later. (The images of fetched Discord channels are read as text
instead, see "Reading the images".)

**Dedup.** `inbox.json` remembers each file's size, time, content hash and
the version of the reader that took it (`inbox::version`, per kind of file).
An unchanged file is not read again (an unchanged archive is not unpacked)
unless its reader is newer; a changed file replaces its document or table, a
file with the content of another is skipped as a copy, and files gone from
the inbox are reported (their documents stay). A deleted document stays
deleted until its file changes or the import runs with `--refresh`.
`--reimport <path>` (or **Import again** next to a file in a report) forgets
one file, folder, archive or family of files: its documents are removed, the
archives around it are unpacked again, and what it gives now replaces its
tables and assets. Every import writes a report: what was taken as what,
skipped and why, failed, gone.

## Sources and their terms

| Source | How | Notes |
|---|---|---|
| Expert notes (your own corrections) | **Correct / add to memory** under an answer in the lab, the Notes panel, or a file in `<data>/notes/` | Source `expert-note`, weight 1.3, the highest; retrieved first and labelled "Expert note (user), <date>". See "Deep questions and expert notes" |
| Guides ("Overfishing Fundamentals", Lenny, ...) | `ingest file` (md, txt, html, pdf) or `ingest url` | `--source guide` (weight 1.15); record the license with `--license` |
| Inkipedia, other MediaWiki wikis | `ingest wiki <start pages or categories>` (the lab: **Wiki / site**, MediaWiki topic) | A whole topic through the API, re-runs fetch only changed pages; the wiki's license (from `siteinfo`) and "<wiki> contributors" kept per document. See "Whole wikis and sites" |
| A whole site | `ingest site <start address>` (the lab: **Wiki / site**, Whole site) | Same host only, a page cap, assets and given paths skipped. See "Whole wikis and sites" |
| Other pages, stat.ink docs | `ingest url` (urls, `--list`, `--sitemap`) | robots.txt obeyed, one request per site every 3 s or the site's `Crawl-delay` |
| Google Docs, Sheets, Slides | `ingest url <the address you share>` | Read through their exports (see below); only files shared as "Anyone with the link can view"; a sheet's tabs one by one with `--all-tabs` |
| YouTube | `ingest youtube <video/playlist/channel>` | `yt-dlp` fetches subtitles and metadata only; uploaded subtitles preferred over auto captions |
| Discord #vod-review | `ingest discord-export`, `ingest discord-bot`, or `fetch discord` + `ingest inbox` | The export and the bot are the sanctioned ways; `fetch discord` reads with your own account, against Discord's terms. See below |
| X (Twitter) | `tools/capture/xcap.mjs` + `ingest inbox` | The Salmon Run posts of the accounts you follow, with their replies, captured by your own logged-in Chrome, slowly and read-only (source kind `x`, weight 1.0); against X's terms, see below. Or save a thread as text and `ingest file` |
| Xiaohongshu (RedNote, 小红书) | `tools/capture/rednote.mjs` + `ingest inbox` | The Salmon Run notes of the creators you follow, with their comments and replies, captured by your own logged-in Chrome, slowly and read-only (source kind `rednote`, weight 1.0); can breach the site's terms, see below |
| Twitch | not automated | Twitch VODs have no subtitles (a speech-to-text step would be needed) |
| Lean's Splatoon 3 datamine (leanny.github.io) | `ingest leanny` (the lab: **Game data (Lean)**) | Fact cards of exact game numbers (source kind `game-data`, weight 1.1) and the Eggstra Work events table; no licence, the data is Nintendo's: fetched at run time, private study only. See "Game data" |

**Google Docs, Sheets and Slides.** Their pages are drawn by JavaScript, so
the page itself holds only a shell ("This browser version is no longer
supported... File Edit View"). An address on `docs.google.com`
(`/document/d/<id>/...`, `/spreadsheets/d/<id>/...`, `/presentation/d/<id>/...`)
is read through its export instead (`google.rs`): a document as Markdown
(`export?format=md`; headings kept for the chunker, embedded images, heading
anchors and the table of contents dropped), else plain text, else `.docx`; a
sheet as CSV (`export?format=csv`, the tab of the address's `gid`), which
becomes a name table in the glossary when it holds names in several languages
and a text document otherwise, like a CSV in the inbox. A sheet's address
without a `gid`, or any with `--all-tabs` (the lab's **Every tab of a
Google Sheet**), brings every tab, each a document of its own under its
tab's address: the tabs are listed from the sheet's HTML view
(`/htmlview`);
slides as text (`export/txt`). The document is stored under the address you
gave, so importing it again with `--refresh` (in the lab: **Again if
stored**) replaces it. Exports work only for files shared publicly: in Google
Docs, **Share > General access > Anyone with the link** (Viewer is enough).
A file shared only with some people answers with Google's sign-in page or
401/403; the import then says "the doc isn't shared publicly" and stores
nothing. Such a file can still be imported by downloading it (File >
Download > Markdown, `.docx` or CSV) and importing that file. Published
copies (`/d/e/.../pub`) are ordinary pages.

**Pages drawn by JavaScript.** Other sites that send only a shell (a page
whose text says "enable JavaScript" or "This browser version is no longer
supported", or has almost no text in a large page) are skipped with the
reason rather than stored; save such a page from the browser and import the
file.

**Local files.** `ingest file` (the lab's **Files**) reads the prose
formats above. A folder or an archive (by its name or first bytes) is
copied into the inbox (`inbox/<its name>`) and taken as the inbox takes it:
unpacked, sorted, deduplicated. Other binary files are refused with the
reason, and the store never takes a document whose text is binary data
(opening the store skips such a document with a warning; delete it).

**Discord.** Two supported ways, both needing the server's consent:
1. *Export file*: someone with access runs DiscordChatExporter (JSON format)
   on the channel or its threads and gives you the file. `--whole` keeps a
   file as one conversation (a thread or forum post).
2. *Bot*: a server admin creates a bot in the Discord developer portal,
   enables the Message Content intent, invites it with View Channel and Read
   Message History on the channel, and shares its token; pass it as
   `DISCORD_BOT_TOKEN` (never stored). Threads and forum posts are read
   too unless `--no-threads`.

Threads and forum posts stay whole, one document each. A plain channel's
messages are grouped into **conversations**: a message with a video of its
own (a YouTube link or a video attachment: a VOD post) starts one, a reply
joins the conversation of the message it replies to, and any other message
joins the one before it unless 2 hours passed. Each message line carries its
time and author, and whom it replies to (`[2024-05-01 10:05 UTC] souper ↪
Ben: ...`); the authors are the document's attribution. Clips stay as links;
they are not downloaded. A channel named `vod-review` (or its threads) gets
the highest weight (1.2).

Every Discord document also keeps its messages as a table, `messages` in
the document's JSON, for aligning comments with a video later:

```json
{"id": "1290000000000000102", "author": "Ben", "time": "2024-05-01T10:02:00Z",
 "reply_to": "1290000000000000100", "reply_author": "souper",
 "thread": "1290000000000000090",
 "video_url": "https://youtu.be/abc", "video_t": 80.0, "video_from": "reply",
 "moments": [{"raw": "1:20", "kind": "video_time", "seconds": 80.0,
              "url": "https://youtu.be/abc", "message_id": "...", "author": "Ben", "at": "..."}]}
```

`reply_to` is the replied-to message (`reply_author` its author, from the
file or from the copy Discord sends along with a reply), `thread` the thread
or forum post the message is in. `video_url` is the video the message is
about, `video_local` its file in the knowledge folder when `fetch discord
--attachments` downloaded it (`media/discord/<guild>/<channel>/<message
id>/<file name>`; a review can then open the file instead of the expired
link), and `video_from` says how it was found, first match wins: `own` (a
YouTube link or video attachment in the message; `video_t` is the link's
`t=` start), `reply` (in the message it replies to, following the chain of
replies up), `starter` (the thread's or post's starter message), or
`earlier-post` (the replied-to author's nearest earlier video post in the
channel: people reply to a comment under their VOD post). `video_t`
otherwise is the first video time written in the message.

The **moments** are what a message points at: YouTube links (with the `t=`
start when there is one), waves and times written in the text ("W1 :50",
"wave 2 at 1:20", "86s", "1:02:03", "wave 3"). Each has the text as written
(`raw`), the message id, author and time it came from, the message's video
as its `url` when it names none, and a guess of what it means (`kind`): the
wave timer counts down from 100 s, so a bare number of seconds up to 100 and
any time named with a wave is `wave_timer`; `m:ss` and `1m20s` forms and
anything over 100 s are `video_time`; a wave with no time is `unknown`.
Documents imported before this table keep a flat `moments` list instead;
import them again to get it (`ingest inbox --reimport <path>`).

### Fetching #vod-review with your own account

`cuttlefish fetch discord` archives channels you are a member of, with your
own account's token. **Automating a user account breaks Discord's terms of
service and can get the account banned**, even for reading. It is here for
one private archive (a knowledge base and, later, training data) after that
was weighed and accepted; it does as little as a person reading the channel
would, and nothing else. The bot and the export stay the sanctioned ways.

What it does, and does not do:

- **Read-only**, and **only what you name**: the channel object, its
  messages 100 at a time (`before`/`after`), and its threads and forum
  posts (the archived-threads listing of the channel; the server's
  active-threads listing when the account has it, filtered to the channel;
  threads started by its messages); with `--count`, the server's message
  search limited to the channel. Every request is checked against that
  scope before it is made. No gateway or websocket, no typing, no writes.
- **Slow.** A random delay between requests, 3 to 8 s by default (`--delay
  3-8`); every 40 to 120 requests (`--pause-every`) a longer pause of 1 to
  5 minutes (`--pause 60-300`); each drawn anew. A 429 is waited out as its
  `retry_after` asks and the `X-RateLimit-*` headers are obeyed; server
  errors back off exponentially; a 401 or 403 stops the run with a clear
  message and nothing else is tried. `--daily-cap N` stops after N requests
  in a day (UTC); `--max-requests N` and `--max-minutes M` end a run early,
  the next run continues.
- **Your browser's headers** when you give them (below), else a current
  desktop Firefox's User-Agent with your system's language and timezone.
- **Resumable and incremental.** The API's JSON is kept as received:
  `<id>.channel.json` (the channel object), `<id>.messages.jsonl` (one
  message per line, appended), the threads in `threads/` the same way, and
  `state.json` with the cursors and the day's count. A run first fetches
  what is newer than the last one (skipped when the channel's last message
  is known), then keeps backfilling older history until the first message.
  Ctrl+C finishes the request under way, saves and stops. Embeds stay as
  links in the JSON; attachments are only counted unless asked for (see
  "Downloading the VODs").
- Progress after each page: `#vod-review: 1300 messages, oldest 2024-03-02;
  14 requests this run, 14 today; delay 5.3 s`, and a line for each pause
  or wait.

By default each channel goes to `<knowledge>/inbox/discord/<guild
id>/<channel id>/` (the knowledge folder found as for every other command:
`--config`, else `./config.toml`, else `--data`, else `$CUTTLEFISH_DATA`),
where **`cuttlefish ingest inbox`** (or the lab's Import inbox) reads it:
each channel and thread becomes conversations as above, `#vod-review` and
its threads with source kind `discord-vod-review`; the channel objects and
`state.json` are skipped as the fetcher's own. A re-run appends to the
messages files, so the next inbox import sees them changed and replaces
their documents. `--out <folder>` puts the channels' files in that folder
itself instead.

**The token** comes from `DISCORD_USER_TOKEN` in the environment, which the
CLI loads from the same env file as `scripts/run.sh` (`$PROCON_ENV`, else
`~/.config/procon/env`, else `.env`; `set -a; . ~/.config/procon/env` does
the same in a shell). Put it there yourself, with the file readable by you
only; the tool never prints or writes it.

**The channel** (`--channel`, repeatable) is easiest as its link: in
Discord, right-click the channel > Copy Link, which gives
`https://discord.com/channels/<server id>/<channel id>`. The fetcher takes
the server from it. Also accepted: a message's link (the channel is taken
from it), `ptb.`/`canary.discord.com` and `discordapp.com` links,
`<server id>/<channel id>`, or the channel id alone (User Settings >
Advanced > Developer Mode, then right-click the channel > Copy Channel ID).
Anything else is refused with these forms. `--guild` gives the server when
only an id is given.

**The browser's headers.** The Discord web client sends more than a
User-Agent with each request, and a request with a made-up or stale set
stands out. Copy yours from your own browser, the one you use Discord in:
open discord.com in it, open DevTools (F12) > Network, click any request to
`discord.com/api/...` (reload the page, or open a channel, if there is none),
and under Request Headers copy the values of `User-Agent` and
`X-Super-Properties` into the env file:

```bash
# Exactly as the request shows them, in single quotes (comments on lines of their own)
DISCORD_USER_AGENT='Mozilla/5.0 (...) ...'
DISCORD_SUPER_PROPERTIES='eyJvcyI6...'
# Optional: X-Discord-Locale and X-Discord-Timezone, when not the system's
DISCORD_LOCALE=en-US
DISCORD_TIMEZONE=Europe/Berlin
```

They are sent exactly as given. `X-Super-Properties` describes your
browser and client build; it is sent only when you give it, never made up.
Without `DISCORD_USER_AGENT` the agent named inside your super properties is
used (so the two agree), else a current desktop Firefox's. The locale and
the timezone default to the system's (`LANG`, `TZ` or `/etc/localtime`);
`Accept-Language` follows the locale, and `Referer` is the channel's page
(`https://discord.com/channels/<server>/<channel>`). The first lines of a
run say where each header comes from, without the values. Copy them again
when your browser updates. None of this makes automating an account
allowed: it is still against Discord's terms and can get the account
banned; it only keeps the requests from looking like something they are
not.

**Counting first.** `--count` asks Discord how big the channel is, reading
no message: the channel object, then the server's message search limited to
the channel (`GET /guilds/<server>/messages/search?channel_id=<channel>`,
whose `total_results` is the count; Discord has no message count
otherwise), and for a forum its post listing (`total_results` of its posts).
A channel not indexed for search yet answers 202 with a `retry_after`; the
count waits as asked and tries twice more, then says to try later. It prints
the estimated requests and time of a whole fetch at the pace given (and days
at `--daily-cap`); a text channel's threads add a request or more each. The
count's requests count toward the daily cap like any other.

```bash
# The token (and, best, your browser's headers) in the env file, readable by you only
chmod 600 ~/.config/procon/env      # after adding: DISCORD_USER_TOKEN=...

# How big is it, and how long would it take? Two or three requests, no messages
cuttlefish fetch discord --channel https://discord.com/channels/737359708276654121/737962428553232465 --count

# A check run: the channel object and one page of messages, then stop
cuttlefish fetch discord --channel https://discord.com/channels/737359708276654121/737962428553232465 --max-requests 2

# The archive: newer messages first, then older history; run it again any time
cuttlefish fetch discord --channel https://discord.com/channels/737359708276654121/737962428553232465

# Gentler still: two hours a run, at most 600 requests a day
cuttlefish fetch discord --channel 737359708276654121/737962428553232465 --max-minutes 120 --daily-cap 600

cuttlefish ingest inbox             # the archive into the store
```

At the default pace a page of 100 messages takes about 5.5 s plus the
pauses, so 20,000 messages (200 pages) take roughly 20 to 30 minutes, and
each thread or forum post at least one more request; a forum with 300 posts
adds about half an hour. `--count` does this sum for a channel.

#### Downloading the VODs

Most VODs in #vod-review are uploaded to Discord as files, and their links
in the messages (`cdn.discordapp.com`, `media.discordapp.net`) are signed
and expire after about a day. So the fetcher can download them itself,
right after the messages, with `--attachments`:

- `list` (the default): counts the channel's attachments and adds up their
  sizes from the messages, downloads nothing, and prints what `videos` and
  `media` would take: `Attachments in the archive: 1830 videos (412.5 GB),
  120 images (0.3 GB), 5 other files (0 MB)`. **Run this first**: the
  files go into the knowledge folder, and if that is a synced folder
  (Dropbox), so does their space. `none` does not even count.
- `videos` downloads the video attachments (by content type or extension:
  mp4, mov, webm, mkv, m4v, avi), `media` the videos and the images.
  Sequential, one file at a time, with the same random delays and pauses
  as the requests (`--delay`, `--pause-every`, `--pause`; Ctrl+C stops
  after the file under way, `--max-minutes` counts), with the browser's
  User-Agent and the channel page as `Referer`, never the token. Only
  links on Discord's CDN, and only those of the messages in the archive,
  are followed.
- `--max-file-mb N` skips files larger than N MB; `--max-total-gb G` stops
  downloading when a channel's media folder would grow past G GB. Neither
  has a default: nothing is skipped unless asked, and nothing asks for
  confirmation, so give `--max-total-gb` when the space is limited.

The files land in `<knowledge>/media/discord/<guild id>/<channel
id>/<message id>/<file name>` (with `--out`, in `<out>/media/...`), newest
message first, and `media.jsonl` in the channel's folder records each one:
message id, attachment id, file name, size, content type, path and SHA-256.
The CDN serves an image in the format its file name says (a phone's
screenshot uploaded as a JPEG named `IMG_1.png` comes as a PNG), so an
image's file is seldom the attachment's size: a file is whole when it has
the length the answer announced (`Content-Length`), and the manifest
records the file's own size.
A run is **resumable**: a file the manifest names with its full size is
skipped, a partial download (`<file name>.part`) is continued with a
`Range` request, and one that came back with the wrong size is discarded
and tried again next time. A link that expired (its `ex` parameter, about
24 h) or that answers 404 has its message's page read again, an ordinary
paced message GET (`before=<message id + 1>`, read-only, no refresh
endpoint), which gives fresh links for every wanted file on that page. The
run ends with what was done: `downloaded 12 files (3.4 GB) into ...; 1818
already there; 2 pages read again for fresh links`. Files are downloaded
after a run that read all the messages; a run stopped by a cap or a limit
only lists them, and the next complete run downloads.

The inbox import then points each message's `video_local` at the file
(`messages` in the document's JSON): a channel whose manifest changed is
read again although its messages did not.

```bash
# First: how much is there? Nothing is downloaded
cuttlefish fetch discord --channel https://discord.com/channels/737359708276654121/737962428553232465

# Then the videos, with a cap on the folder's growth; run again any time to continue
cuttlefish fetch discord --channel https://discord.com/channels/737359708276654121/737962428553232465 \
  --attachments videos --max-total-gb 200

cuttlefish ingest inbox             # the rows now carry video_local
```

#### Reading the images

The images people post hold what no message says: a table of every
weapon's damage per second, a stage map with circles, arrows and lettered
spawn points, a screenshot with numbers. `--attachments media` downloads
them with the videos (`--max-file-mb 2` keeps it to the images and the
smallest clips), and **`cuttlefish read-images`** (`image_text.rs`) sends
each image once to the model backend (`--backend`: the API with
`ANTHROPIC_API_KEY`, else the Claude Code CLI on your subscription; one
call per image, `--parallel 3` at once) with the message it was posted
with (channel or thread, author, date, text) and the glossary's entries
for the terms named there. The model writes the text in the image in its
own language (tables as Markdown tables with every row, numbers as shown,
`[unreadable]` where it cannot read), then the same in English with what
the image shows: the kind of picture, a map's stage and tide, what is
marked where and what the marks mean. The image goes as a JPEG at high
quality without chroma subsampling (small colored text stays sharp),
scaled down to the model's limits (2576 pixels on the longer side, 2560 x
1440 pixels in all) and over white where it is transparent.

The answers are kept by the image's SHA-256 (the manifest's) in
`<knowledge>/media/image-text.jsonl`, with the backend, the model and the
time: an image is paid for once however many messages post it, a run reads
only images without a text (`--refresh` reads them all again), and an image
that failed is tried again by the next run; three failures before any
success stop a run. The next `cuttlefish ingest inbox` reads a channel
again when texts of its images arrived (their hash joins the channel's)
and puts each text under its image's link in the conversation's document,
so a search finds the image's content with its message, which the answer
then cites:

```text
[2025-09-17 06:01 UTC] Ka: Salmon Run weapon DPS
  [IMG_5574.png: https://cdn.discordapp.com/attachments/...]
  Image IMG_5574.png, as the model read it:
  Text in the image:
  | ... | ... |

  In English:
  | ... | ... |
  A table of ...
```

The import's report says how many images of a channel came as text.

```bash
cuttlefish fetch discord --channel https://discord.com/channels/<server>/<channel> --attachments media --max-file-mb 2
cuttlefish read-images --dry-run                 # what a run would read; nothing is sent
cuttlefish read-images --channel https://discord.com/channels/<server>/<channel> --effort medium
cuttlefish ingest inbox                          # the texts join their messages
```

#### The VOD-review corpus

The archive is also a dataset: every video someone posted for review, with
the comments on it and the moments they point at. `cuttlefish corpus build`
reads the #vod-review channel and its threads from the inbox
(`inbox/discord/`), the media manifests and the downloaded videos, and
writes `<knowledge>/corpus/vod-review.jsonl`, one line per **VOD**: a
conversation of the channel ([above](#discord)) in which a video was posted.
Each line holds the conversation's id and link, the poster, the day and the
**game era** (`game`: `S2` before Splatoon 3's launch on 2022-09-09, else
`S3`; `game_from` says whether the date or a wave table decided), the
**video** (`key`: `youtube-<id>` or `discord-<attachment id>`; the link as
posted, a link's start, an attachment's file name, and `local`, its file
below the knowledge folder when downloaded; more videos of the same post in
`other_videos`), and the **messages** in order, each with its author, time,
text, reply and the video it is about when that is another one. A message's
`moments` are what it points at, with the text as written, the kind
([`moments`](#discord): `video_time`, `wave_timer`, a wave without a time),
the wave (named, or the last one named before it in the same message: "W1"
on a line of its own heads the timers below it) and one of two flags:
**`aligned`** (`t_s` is seconds into the video: a time written as `1:20`,
a link's `t=`, or a timer the wave table placed; `placed_by` tells which,
with the table's `confidence`) or **`needs_hud`** (a wave timer, `W2 :50`,
waiting for the video's wave table). The build prints the counts: VODs by
era and by video on disk, comments, moments by kind, how many are aligned
now and how many wait. It is deterministic and re-runnable; nothing is
downloaded or changed by it.

**The videos.** `cuttlefish corpus videos --list` asks yt-dlp about each
YouTube VOD of the corpus (metadata only, one request at a time, cached in
`media/youtube/<id>.info.json`) and adds up the size a download would take;
`cuttlefish corpus videos` then downloads them, at most 480 lines
(`bv*[height<=480]+ba/b[height<=480]`, merged into `<id>.mp4`), one at a
time with a pause of 5 s between videos (`--delay 5`, or a range such as `--delay 5-15`),
`--max N` videos a run. It is resumable: a downloaded video is skipped, a
partial one continues, a deleted or private video is remembered in its
`info.json` as unavailable and not asked for again (`--retry-unavailable`
asks). Three failures in a row (the network, a bot check) end the run;
Ctrl+C ends it after the video under way. The uploaded attachments come
from `fetch discord --attachments videos` (above).

**Aligning with the HUD.** The wave timer counts down from 100 within a
wave, so "W2 :50" is a moment of the video only once the video's waves are
known. `cuttlefish corpus align` reads the HUD of every VOD video on disk
that has no wave table yet (`gameplay-vision`'s reader: the wave label and
the timer from the top-left corner, sampled every 0.5 s, about 1-2 s per
minute of video; `--refresh` reads them all again) and writes the table
beside the video as `<video stem>.wave_starts.json`, the file
`gameplay-vision hud scan` writes (see that crate's README: one entry per
continuous piece of a wave, `start_video_s`, `timer_at_start`, `agree`).
The build then places each timer at `start_video_s + timer_at_start -
timer_s` for the first entry of that wave whose span holds the result, a
timer that names no wave ("13s using a bomb", about a clip of one wave) in
the only wave it fits, and a wave named without a time where the wave is
first seen; the entry's `agree` becomes the moment's `confidence`. A video without waves in it (a
lobby, a results screen) keeps its timers unplaced. The table is also the
hook for evidence about the game: a `game` key (`"S2"` or `"S3"`) in it,
written by hand or by a reader that tells the games apart, overrides the
era from the date.

**Reviews for the lab.** `cuttlefish corpus reviews` creates or updates
a review of the Cuttlefish app for every VOD whose video is on disk, in the
lab's reviews folder (`[cuttlefish] reviews` of `--config`, else
`Reviews` next to the knowledge folder; `--reviews <dir>` overrides):
`<reviews>/discord-<conversation id>/review.json`, titled by the poster and
the day, with the era and the origin (`source: {from: discord, url,
video}`). The review's video is the file in the knowledge folder by its
full path (kind `file`), not a copy: the VODs add up to gigabytes, and both
folders may be synced folders where a hard link is not possible, so a
reference is what keeps working. Each message becomes comments at the
moments it places in the video (one per aligned moment, with the text from
that moment to the next, the reviewer's name as the author, and the
message's link as `source`), and one note for the rest: the text without a
time, and the moments that wait for the HUD as `unplaced` (text, kind,
wave, seconds left), which the page shows as chips. Ids of what was
written start with `discord-`; a run replaces those and nothing else, so
comments, notes and chats added in the lab stay where they are, and a
run that changes nothing writes nothing. The library's **Community**
filter shows these reviews; the Knowledge page's **Create reviews from
#vod-review** button runs `align`, `reviews` and `index` as a job.

**Expert comments** (`expert.rs`). The whole conversations are documents of
the store already (from the inbox), but a question about a moment is best
answered by the one comment about a similar moment. `cuttlefish corpus index`
makes every message of a VOD's conversation that is not the poster's (at
least 30 characters once custom emoji are gone) an **expert comment**: the
comment, split at line ends into pieces of at most 900 characters, the first
piece of a reply after the message it answers (`(replying to X: "...")`,
200 characters), and who said it, when and about what: reviewer, day, the
VOD's era and video, and the moment of the piece (its first moment with a
wave or a video time, else the wave named last before it in the message; a
timer the wave table placed in a wave it did not name gets that wave). One
document per VOD holds them (source `discord-vod-review`, the highest
weight, the S2 penalty for Splatoon 2 VODs; its `expert_comments`), and the
store indexes each piece as one chunk headed by its label, **"Centritide,
2023 (S3), about a W2 :50 moment"**, with the Discord message as its link.
It runs under the store's write lock and embeds only VODs whose comments
changed; VODs gone from the corpus lose theirs. About 1000 comments of 114
VODs take a few minutes to embed on the CPU the first time.

**The moment as text** (`situation.rs`). A chat or review about a moment
of a video gets a `<moment>` block, cheap tokens next to the frames. For a
recorded session at 113.7 s (shortened):

```text
<moment>
Controller input 109.7–115.7 s, recorded from the controller:
- 109.7–110.9 s: ZL ×2 (swimming)
- 109.8–110.1 s: turning left ~65°/s (peak 98°/s)
- 111.0–112.9 s: ZR ×4 (shooting)
- 113.2–113.6 s: moving forward-right (left stick)
- 113.5–114.5 s: B ×2 (jump)
Objects a person labelled on the frame at 113.7 s: chum ×2 (left, left), cohock ×4 (center, right, right, right), steel eel (right), golden egg ×4 (center, center, center, center)
</moment>
```

and for a #vod-review video with a wave table, at 20 s:

```text
<moment>
HUD at 20.0 s: wave 2, 30 s left (W2 :30), golden eggs 21/25
</moment>
```

The **controller input** comes from per-frame labels (`gameplay-data`):
a session's `controller.bin`, aligned as the Inkspector aligns it, or, for
other videos, the Predictor's newest finished run on that video (the IDM's
`pred.jsonl`, marked as an estimate). Buttons are held (0.4 s or longer),
tapped (presses under 0.5 s apart, `×n`) or pressed once, named with what
they do in Salmon Run; a **squid roll** is B while ZL is held with the left
stick's direction turned at least 120° within the 0.25 s before; the left
stick's direction held 0.4 s is movement; the **camera turn** is gyro yaw
plus the right stick at AgentZero's shared fit (0.00137° of yaw per raw
unit per frame), or the IDM's `camera_turn` (7.68 px per degree), over 30°/s
for 0.25 s. The **HUD** comes from the wave table beside the video
(`<stem>.wave_starts.json`: wave and timer at that time) with the golden
eggs read from the frames within 0.5 s; without a table, none. **Objects**:
the boxes a person drew on the session's frame at the moment (the
Inkspector's annotations); `detected` is the hook for a detector's boxes
later. The same situation joins the **retrieval query**, without times:
`W2 :30, wave 2 with 30 seconds left, 21 of 25 golden eggs`, `player:
shooting, squid roll, turning left`, `on screen: chum, steel eel`.

**Retrieval** then takes the 4 expert comments nearest to the query (at
most 2 of one VOD, so one reviewer's comment per wave does not fill the
list) and the 8 best other chunks, numbered S1, S2, ... as one list; the
expert comments go into an `<expert_comments>` block, each after its label:

```text
<expert_comments>
<comment id="S3" era="[Splatoon 3 era]" video="https://cdn.discordapp.com/...">
Centritide, 2023 (S3), about a W2 :50 moment: - at 50s when you throw an egg into basket you could've ...
</comment>
</expert_comments>
<knowledge>
<excerpt id="S5" source="guide" title="...">...</excerpt>
</knowledge>
```

The persona tells the model these were said about someone else's game, to
use one when the situation matches and to quote it by reviewer and year.
The chat's reply lists every expert comment it was given (`experts`, with
`expert`: reviewer, date, era, wave, timer), which the page shows under
**Expert comments given**, each linking to its Discord message.

**How well does a moment find them?** `cuttlefish corpus retrieval` (no
model) takes, for up to `--n 10` VODs whose video is on disk with a wave
table, the first comment placed in a wave, builds the query from the HUD at
its time alone (the corpus's videos have no controller input), leaves that
message out, and counts, in the top `--k 10`, comments on the same VOD and,
in the top 5, comments about the same wave, next to what a random pick
would give; `--question` puts a player's question first. On the archive of
September 2026 (990 comments): no comment of the same VOD in the top 10 for
any of the 10 moments (0.6 expected by chance); 2.3 comments about the same
wave in the top 5 (0.2 by chance), 2.5 with "What should I have done
here?". The HUD alone finds the wave, not the game: the controller summary
(own sessions, IDM predictions) and the question carry the rest.

```bash
cuttlefish corpus build                       # the corpus and its counts
cuttlefish corpus videos --list               # what a download would take; run first
cuttlefish corpus videos --max 10             # ten videos, then run again
cuttlefish corpus align                       # wave tables for the videos on disk
cuttlefish corpus reviews                     # the reviews; again after align or new videos
cuttlefish corpus index                       # expert comments; again after build or align
cuttlefish corpus retrieval                   # the check above
```

#### Game data: Lean's Splatoon 3 datamine

[Lean](https://leanny.github.io/) (@LeanYoshi) publishes the game's data
files as JSON (the `Leanny/splat3` repository behind
`leanny.github.io/splat3/`) and, for every Eggstra Work event, a scenario
page (`eggstra_work/coop_event_NN.html`, data in
`eggstrawork/EggstraWorkNN.js`): stage, weapons, specials, the five waves'
tide and occurrence and the boss spawn schedule per hazard level. Thanks
to Lean for all of it. `cuttlefish ingest leanny` (`leanny.rs`,
`eggstra.rs`; the lab's **Game data (Lean)** import) fetches what
matters for Salmon Run and stores **fact cards**, one document per
entity, with source kind `game-data` (weight 1.1), the version of the
game the data is from (`versions.json`, `11.3.0` in September 2026) and
Lean's credit in every card:

- `CoopEnemyInfo`: one card per Salmonid (26): category (`Rare` is a Boss
  Salmonid, `Boss` a King, `Zako` a lesser), the most on the field at once,
  power eggs per hit and on a kill, the Kings' HP coefficient per hazard
  level. Hit points are not in the published data: the card gives
  Inkipedia's where it has one plain number (`stats::SALMONID_HP`), else
  says so.
- `CoopSceneInfo`: one card per stage (14, the Big Run stages included),
  with the Eggstra Work events held there.
- `WeaponInfoMain` (the `_Coop` rows, 71) and `WeaponInfoSpecial` (11
  `_Coop` rows): one card per Salmon Run weapon and special, with its
  battle weapon's key, whether it is a Grizzco weapon and the Eggstra Work
  events it was in; the card also holds its parameters
  (`data/parameter/<version>/weapon/Weapon<Name>_Coop.game__GameParameterTable.json`
  merged into its parent table; a special's Salmon Run table holds its
  damage to Salmonids, `spl__BulletBlastParam.DistanceDamage`;
  `--no-weapons` skips these files: about 180 requests the first time).
- `spl__CoopLevelsConfig`: one card per hazard level (9): the wave and
  known-occurrence parameters at that difficulty (Rush speed, the
  Mothership's HP, tornado eggs per box, quotas).
- Eggstra Work: one overview card per event (dates, stage, weapons,
  specials, waves, Inkipedia's participation reward and high score
  thresholds) and one card per wave with the spawn schedule at every
  hazard level the data holds (the wave timer's seconds left, the boss,
  the spawn point of Lean's map; Rush and Griller waves their target
  order, Goldie Seeking its gushers, the Mothership its boxes, Giant
  Tornado its drop points). Names come from the game's language files
  (`EUen`, `JPja`, `CNzh`), so every card names its things in English,
  Japanese and Simplified Chinese next to the internal key
  (`SakelienBomber`, `Shooter_Normal_Coop`).

Every card but the Eggstra Work ones also keeps the data it was made from
as `facts` (`stats::Facts`: kind, key, version, events, the parameters as
the game has them), which `stats::summary` reads for players and the
Pedia shows: damage, ink, times and sizes of a weapon or special (its
falloff by distance as a table), how many hits each Salmonid takes at its
most damage per hit (Salmonid HP from Inkipedia's "Salmon Run Next Wave
data"), a Salmonid's eggs and HP, a hazard level's numbers by occurrence;
every raw parameter stays folded under it. Only conversions checked
against Inkipedia pages in the store are applied: damage is stored ×10,
ink as a fraction of the tank (shown in percent), times in frames at 60
per second; distances stay in the game's units. Weapon and special cards
carry the same summary in English in their text, ahead of the raw
parameters, for the model. `CARD_FORMAT` in `raw/leanny/state.json` makes
the next run rebuild cards of an older format though nothing changed.

Lean's scenarios carry no dates. Those come from Inkipedia's
[List of Eggstra Work shifts in Splatoon 3](https://splatoonwiki.org/wiki/List_of_Eggstra_Work_shifts_in_Splatoon_3)
(one API request, CC BY-SA 4.0): the shifts in order are the events, each
48 hours from its start day (00:00 UTC), and Lean's scenario of the same
number is attached when its stage is the shift's (a mismatch is logged).
An event without a scenario page (a rerun of an earlier one, as #13 and
#14 were) gets an overview card from Inkipedia's row. The result is
**`corpus/eggstra_events.json`**: number, start and end, stage, weapons,
specials, a wave summary, the scenario number and Lean's page. `corpus
build` reads it and tags every VOD posted during a shift or in the week
after it with `eggstra_event: N` (probable); the counts show in its
stats, the review's origin (`source.eggstra_event`) and the library
("probably Eggstra Work #7").

The same run writes a **name table** (`terms/`, source `leanny:names`):
Salmonids, stages, Salmon Run weapons (with their battle weapon's key) and
specials with their names in the three languages and the internal keys as
origins, so the glossary's Steelhead gains `SakelienBomber` and an icon
named `Wst_Shooter_Normal_00.png` links to the Splattershot. (stat.ink's
icons are named by stat.ink's own keys, `bakudan`, `52gal`; Lean's data
does not map those, so they stay unlinked.)

**Terms.** The repositories state no licence and the data is Nintendo's,
extracted by Lean. Nothing of it is in this repository: the files are
fetched at run time into `raw/leanny/` (each with its ETag in
`state.json`), and every card records "No licence stated: Splatoon 3 game
data © Nintendo, extracted and published by Lean (leanny.github.io);
private study only" as its license and Lean as its attribution; the
Knowledge overview shows a credits line while such cards are stored.

**Politeness and re-runs.** One request at a time through the crawler
(`--delay-s`, 1.5 s by default; `robots.txt` obeyed, the site has none),
about 20 requests without the weapon parameters, about 180 with them. A
re-run asks for each file with `If-None-Match`; GitHub Pages answers 304
for an unchanged file, so nothing is downloaded and, when nothing changed
and every card is stored, nothing is embedded again (`--refresh` rebuilds
the cards anyway). Event pages are probed upward until the first 404, so
a new event is picked up by the next run. `--dry-run` (the lab's
checkbox) fetches nothing: it lists the files with their state and what
the copies fetched so far would give.

**In prompts.** A `game-data` excerpt is labelled as such; the persona
treats its numbers as exact for the version they name and credits Lean,
while a number the cards lack (hit points) is still not to be invented.
The lab's `crates/grizzco-lab/web/demo-questions.js` holds demo questions the cards answer with exact
numbers, in English and Chinese, plus a few that need the #vod-review
knowledge too; the chat shows three of them at random among its chips.

```bash
cuttlefish ingest leanny --dry-run      # the files and their state; nothing fetched
cuttlefish ingest leanny                # about 180 requests the first time, then only changed files
cuttlefish ingest leanny --no-weapons   # without the weapon parameter files
cuttlefish corpus build                 # tags the VODs with their probable Eggstra Work event
cuttlefish search "Eggstra Work #7 wave 3"
```

**Whole wikis and sites** (`wiki.rs`). Both are as polite as the rest: one
request at a time, `--delay-s` (default 2 s, at least 1) or the site's
`Crawl-delay` when longer, every `robots.txt` rule for `Cuttlefish` (else
`*`), and a `robots.txt` answering with a server error stops the import. A
`--dry-run` (the lab's **Dry run: count the pages first**, on by
default) tells what is in scope and how long fetching it would take, and
stores nothing. Progress and **Stop** work as for any import.

- *MediaWiki topic* (`ingest wiki`): start pages and categories, as titles
  or `/wiki/` addresses (the API is `/w/api.php` on that host unless
  `--api` says otherwise). A category brings its articles, and its
  subcategories' down to `--depth` levels (default 2) except those left out
  with `--exclude`; a start page brings itself and, with `--link-match
  <word>`, the pages it links to whose titles contain the word. Listing
  gives each page's latest revision; pages are fetched rendered
  (`action=parse`) and kept as text with headings (hidden infobox rows,
  navigation boxes, edit links and references dropped). Each document
  records its url, `revision`, the wiki's license and "<wiki> contributors";
  a re-run fetches only the pages whose revision changed (`--refresh`
  fetches all). `--max-pages` (default 500) caps the pages *fetched* in a
  run, so a capped run, or one stopped early (Cancel, the lab closed),
  continues with the rest when run again. Every request carries
  `maxlag=5`: a lagging or busy wiki is left alone for the time it asks
  (or half a minute) and asked again. Raw pages go to `raw/wiki/`.
- *Whole site* (`ingest site`): from a start address, the pages on the same
  host reached through links and through the sitemaps `robots.txt` names
  (else `/sitemap.xml`), up to `--max-pages` pages fetched in a run
  (default 100). Images, scripts, styles, fonts and feeds are skipped, and
  so are the path prefixes given with `--skip` (an app such as a map
  viewer). A page drawn by JavaScript is noted and not kept. The notes end
  with the sections found (pages per first path segment). Every page
  fetched is kept raw (`raw/web/`), and a re-run reads a page with a raw
  copy from it (its links, and its document when not stored yet) instead
  of fetching it again, so it goes on past where the last run stopped;
  `--refresh` fetches everything. A dry run needs no page when the site has
  a sitemap; without one it still fetches pages to find links, but keeps
  nothing.

**Stopping and running again.** Every import stores each document as soon
as it is made, and the index follows the documents (opening the store embeds
the ones it lacks), so an import stopped halfway (the lab's **Stop
(continue later)** button on the running job, shown from "loading the
knowledge store" on, embedding included; Ctrl+C; the lab closed) loses
at most the page under way. Running it again continues: `ingest
url` and `ingest youtube` skip what is stored (a Google Sheet's tab kept as a
name table too), wikis fetch only new or changed revisions, sites read their
raw copies. What is fetched again: the listings (a wiki's categories, a
site's `robots.txt` and sitemaps, a sheet's tab list), and pages that failed
or had no text.

**Inkipedia.** Its `robots.txt` allows general crawlers (`*`) on articles and
`api.php` (it disallows `/w/index.php`, `Help:` and `MediaWiki:` pages and
names no `Crawl-delay` for `*`), but disallows AI crawlers such as ClaudeBot
and GPTBot entirely. Cuttlefish identifies as itself and is run by you for
personal study, so the rules for `*` apply to it, but feeding the wiki to a
model is close to what those lines refuse; import it only for your own
private use, and ask the Inkipedia admins (or use a database dump they
publish) for anything more. The license the wiki states in `siteinfo` is
kept per document (in September 2026: Creative Commons
Attribution-ShareAlike 4.0): attribution, and derived text shared under
the same license.

The Salmon Run topic, as a dry run counts it (107 pages at depth 2, about
4 minutes):

```bash
cuttlefish ingest wiki https://splatoonwiki.org/wiki/Category:Salmon_Run \
  "Category:Salmon Run Next Wave" https://splatoonwiki.org/wiki/Salmon_Run \
  --depth 2 --exclude Category:Mechanics --exclude Category:Collectibles \
  --exclude "Category:Salmon Run music" \
  --link-match salmon --link-match grizzco --link-match "big run" --link-match eggstra \
  --dry-run
```

`Category:Mechanics` and `Category:Collectibles` are subcategories of
`Category:Salmon Run` that hold the whole game's mechanics and collectibles
(Octo Expansion's 8-balls, Sunken Scrolls, ...), so they are left out.

### Capturing X (Twitter) with your own account

The Salmon Run creators the user follows post their findings on X, and
X's API terms and pricing rule out reading them through the API.
`tools/capture/xcap.mjs` (Node 22+, no packages; its README has the
guide) drives the user's own logged-in Chrome over the DevTools protocol
on a profile folder of its own (`~/.config/procon/browser-profile`): it
opens the following list, each followed account's profile and the page of
each post about Salmon Run, scrolls like a reader, and keeps the JSON the
page loaded for itself. **Automating one's own account can breach X's
terms**, and the account can be limited or suspended; slow, read-only use
of a real browser reduces the risk and does not remove it. The user
weighed and accepted it for this private knowledge base. The tool makes no
requests of its own, signs nothing, writes nothing to X, downloads no
media, and never sees or stores credentials (the browser profile holds the
session).

What it keeps: a post is about Salmon Run when its text or its quoted
post's matches the glossary in English, Japanese or Simplified Chinese
(サーモンラン, バイト, 鮭, Salmon Run, 打工, 鲑鱼跑, Grizzco, Eggstra
Work, Big Run, the bosses, Kings, events and stages); retweets, the
accounts' replies to others and everything off topic stay out. Each kept
thread (the post and its replies) is one JSON line in
`<knowledge>/inbox/x/<handle>/posts.jsonl` with id, author, date, text,
language, links, media links, the quoted post, whom it replies to and the
replies; `inbox/x/state.json` holds the following list, a cursor per
account, every post id looked at and the day's action count, so runs
continue and stay incremental. Pace: 4 to 10 s between page actions, a 1
to 3 minute pause every 15 to 30, 400 actions a run and 800 a day by
default, a stop on a 429, a 401 or the login page; about an hour per 100
posts.

```bash
node tools/capture/xcap.mjs login                          # once, in the window that opens
node tools/capture/xcap.mjs run --dry-run --max-actions 20 # browse a little, write nothing but the day's action count
node tools/capture/xcap.mjs run                            # the capture; run again any time
node tools/capture/xcap.mjs status
cuttlefish ingest inbox                                    # or Import inbox on the Knowledge page
```

The inbox (`x.rs`) makes one document per thread with source kind `x`
(weight 1.0, like a Discord channel and below #vod-review): titled by the
author, the day and the start of the text; the post, its quoted post and
the replies as timestamped lines (`[2026-09-24 12:20 UTC] Wave 3 (@wave3)
↪ @ikura_coach: ...`) with their links; the authors as attribution, the
language X recorded, the game era from the date, and "study use only, do
not republish" as the terms. Slang suggestions read `x` documents with the
other community sources.

### Capturing Xiaohongshu (RedNote) with your own account

The Chinese Salmon Run community writes on Xiaohongshu (小红书, RedNote):
guides, clears, and discussion in the comments. `tools/capture/rednote.mjs`
(Node 22+, no packages; the folder's README has the guide) drives the same
logged-in Chrome as `xcap.mjs`, on the same profile folder
(`~/.config/procon/browser-profile`, one login for both sites): it reads
your following list once (from a comment box's @ picker; nothing is sent), each creator's notes list (笔记), and a random
60 to 95% of each creator's Splatoon notes (at most 12 a visit, new ones
first, in a random order; later visits finish the rest) for their comments
and replies, scrolling and clicking like a reader, and keeps the JSON the page loaded for itself (the site's signed
headers, `x-s`/`x-t`, are never made or replayed; the page's server state
and the DOM are the fallbacks). **Automating one's own account can breach
Xiaohongshu's terms**, and the site watches for it (risk control, sliders,
forced re-logins, a restricted account); slow, read-only use of a real
browser reduces the risk and does not remove it. The user weighed and
accepted it for this private knowledge base. The tool makes no requests of
its own, writes nothing to the site, downloads no media, and never sees or
stores credentials.

What it opens: only notes judged Splatoon's from their tiles, before
any is opened, since the creators post their lives too. A note is
wanted when the same three-language glossary as `xcap` finds a term in
the tile's title (`lib/filter.mjs`: 打工, 鲑鱼跑, 熊先生, 金鲑鱼卵, the
bosses, the Kings, the stages and their short names (生筋子, 破船,
发电所), the players' jargon (熊商会, 搬蛋), サーモンラン, バクダン,
Salmon Run, Grizzco, ...) or, titles being jargon a filter misses, when
its cover thumbnail looks like the game to a small local model
(`lib/cover.py`: SigLIP 2 zero-shot on the CPU, in AgentZero's
environment; its own probability of a match, 0.05 and above); a note with
neither is skipped, never
opened, its content never seen. What it keeps: every note it opens; the
record's `matched` lists the glossary's terms in its title, text and
tags, `cover_score` the cover's score when that decided, and `on_topic`
says whether either did; the importer takes every record. Each note is one JSON line in
`<knowledge>/inbox/rednote/<user id>/notes.jsonl`: id, author, date,
title, text, tags, image and video addresses (nothing downloaded), likes,
collects, shares, the comment count, and the comments with their replies
(author, date, text, likes, region, pictures as links, whom a reply
answers; a comment that is a picture alone reads `[picture]` in the
document);
`inbox/rednote/state.json` holds the following list, a record per creator
(its notes, which are read and which left), every note read, and the
day's action count, so runs continue and stay incremental; creators never
visited come first, then those with notes left. Pace: 6 to 12 s between
page actions, a 1 to 4 minute pause every 15 to 30, 750 actions a run and
a day by default; a captcha, a slider, a login prompt, a
risk-control page or a refused answer stops the run at once, nothing is
ever solved. About 5 actions a note (3 scrolls of the comments and 5
reply threads at most): the day's 750 actions read about 120 notes in 3 to
4 hours.

```bash
node tools/capture/rednote.mjs login                          # once, in the window that opens (a code to scan with the app)
node tools/capture/rednote.mjs run --dry-run --max-actions 20 # browse a little, write nothing but the day's action count
node tools/capture/rednote.mjs run                            # the capture; run again any time
node tools/capture/rednote.mjs status
cuttlefish ingest inbox                                       # or Import inbox on the Knowledge page
```

The inbox (`rednote.rs`) makes one document per note with source kind
`rednote` (weight 1.0, like a Discord channel and below #vod-review):
titled by the note; a header `Xiaohongshu note by <creator>, <date>`, the
text, the tags, then a `## Comments` section of `[date] author: text`
lines with the replies indented (`↳ name` for the comment they answer);
the creator as attribution, the note's own language (zh, ja), the game era
from the date, and "study use only, do not republish" as the terms. Slang
suggestions read `rednote` documents with the other community sources.

## Deep questions and expert notes

Fact questions ("how much health does a Steelhead have?") test the store;
the questions a high-level player asks test reasoning: why the opening
kills of a wave are aggressive when bosses are lured to the basket anyway,
which way a Drizzler jumps and when, where to fight the Mothership on a
stage and tide and at what second to open it, how to plant eggs for the
Snatchers. `questions/deep.toml` is a bank of about fifty such questions in
English and Simplified Chinese (`questions.rs`), each with a `category`
(macro, openings, bosses, stages, events, eggs, weapons, moments) and what
it `needs`: `knowledge` alone, a `video_moment`, a `video_range`, the `hud`,
or the `detector` that is not trained yet. The lab offers a few at
random as chips next to the chat (video questions only in a review with a
video; detector ones not yet) and lists the whole bank in the Knowledge
view.

**The eval** (`cuttlefish eval deep [--lang en|zh] [--parallel 3] [--max N]
[--only <id>]`, or **Run the deep eval** in the Knowledge view) asks the
model the questions that need no video, a few at a time, through the
configured backend (`deep_eval.rs`), and writes one line per answer, with
the sources cited and the backend, model and effort that answered, to
`<data>/eval/deep-<date>.jsonl` after every batch. It is a benchmark: it
runs only when you start it. The Knowledge view lists the runs; for each
answer you mark **Good** or **Wrong**, and **Correct → note** opens the
answer in the note editor. Once saved, the card shows your note as the
answer, with Cuttlefish's folded under it and marked wrong, and the file
records the mark and the note's id. **Ask again** asks that one question
again over the store as it is now, your notes first, and keeps the new
answer beside the first (`again` in the file, each answer with its own
verdict and its model).

**Expert notes** are the memory: an answer you edited into the correct
explanation, or anything you wrote from scratch, saved as
`<data>/notes/<id>.md` (`notes.rs`; the id is the date and the question's
words). Every answer of Cuttlefish in the lab has **Correct / add to
memory** (中文: 纠正/补充 → 存为笔记), which opens the editor prefilled with
the question and the answer. The file is Markdown with YAML front matter:

```markdown
---
question: Which way does the Drizzler jump?
question_id: drizzler-jump
tags: [bosses, drizzler]
terms: [drizzler]
author: user
date: 2026-09-27
era: S3
version: 10.0.0
from: chat 2026-09-27_20-15-00
---
It jumps away from the player who last shot its umbrella, ...
```

A note is a document of source kind `expert-note` with the highest weight
(1.3, above #vod-review's 1.2), titled by its question and headed by its
label ("Expert note (user), 2026-09-27"), indexed the moment it is saved.
Retrieval fetches the two closest notes before anything else, the prompt
puts them in an `<expert_notes>` block, and the persona is told they come
from a high-level player checking its earlier answers: when one applies,
follow it over every other source and cite it. A note whose `question_id`
names a bank question is that question's `reference`. The files are the
truth: when the store opens (the lab, or any CLI command), notes edited
by hand or synced from another machine are re-embedded and notes whose file
is gone lose their document (`notes::sync`). The Knowledge view's **Expert
notes** panel lists, edits and deletes them.

**Estimated controller input.** When a chat is about a video without a
recording, the `<moment>` block's controller input comes from AgentZero's
IDM. `situation::IDM_RELIABILITY` holds how far it can be trusted, each
number with the model and the play it was measured on: the buttons from
IDM v4 against the true input of the three held-out sessions 13-20-24,
14-04-14 and 14-54-32 (20 min, measured 2026-09-30: frame F1 ZL 0.92, ZR
0.82, B 0.67, A 0.56 (precision 0.54, recall 0.58), R 0.51, Y 0.48; presses
whose start it marks within ±4 frames: A 0.73, ZR 0.39, Y 0.19), the camera
turn, gyro and stick from IDM v2 on held-out frames (camera turn r 0.80,
gyro pitch r 0.70, right stick x r 0.70; about 14 minutes of training
data). The block says the input is *estimated, not recorded* with those
numbers and tells the model not to build fine claims on it, and the persona
repeats the rule. Update the constant when a better IDM exists; set its
`trusted` once the estimates are good enough, which drops the warning.

## Overfishing Pedia

`pedia` is the lab's encyclopedia view over the glossary: which terms are
about Salmon Run (`candidate`, `in_scope`: the seed's and the user's terms,
terms with slang, a definition or a relation, the Salmon Run tables' bosses,
events, tides, stages and titles, and the Splatoon 2 and 3 weapons, specials,
subs and stages #vod-review talks about; gear, brands, battle modes and
stages' short names are left out), a friendly `Section` per term (its kind,
a few known ids, else the section of the broader term it belongs to), the
games of its names (`games`, from the name tables it came from) and where it
comes from (`facets`: official, community, user). `mentions` searches every
expert comment of the corpus (`expert::comments`, the reply context left
out) for the names of the terms in scope with `Glossary::find_in`, once per
corpus and set of names (`names_key`); `rank_quotes` and `snippet` pick and
cut the quotes. `fact_cards` reads the `game-data` documents,
`note_is_about` and `names_term` link expert notes and deep questions.
`review::SourceRef` carries the cited chunk's document id and position, so
the page can show the chunk itself.

## Library API (for Grizzco Lab)

```rust
use cuttlefish::review::{Reviewer, ReviewRequest, Frame, ExistingComment};
use cuttlefish::llm::Settings;

let reviewer = Reviewer::open(&data_dir, Settings::default())?; // fails clearly without a model backend
let comments = reviewer.review(&ReviewRequest {
    video: "2026-09-25_20-15-00".into(),
    start_s: 120.0,
    end_s: 135.0,
    frames: vec![Frame { t_s: 121.0, jpeg }],       // up to 20 are sent
    question: Some("Why did we lose the basket here?".into()),
    comments: vec![ExistingComment { t_s: 124.0, text: "I died here".into(), author: None }],
    situation: None,   // or the moment as text: cuttlefish::situation::Situation
})?;
let answer = reviewer.ask("...")?;
let ja = reviewer.translate("...", "ja")?;
let what = reviewer.explain("熊刷", "en")?;   // a term or callout: what it means, when it is said

// A conversation: the earlier turns go along as they were, retrieval runs on
// the new message and the last user turns, and a video may be attached
use cuttlefish::review::{ChatRequest, VideoContext};
use cuttlefish::llm::{Role, Turn};
let reply = reviewer.chat(&ChatRequest {
    history: vec![
        Turn { role: Role::User, text: "What does the Flyfish do?".into() },
        Turn { role: Role::Assistant, text: "It fires missiles…".into() },
    ],
    message: "Translate for my teammate: 我还剩一个镭射".into(),
    video: Some(VideoContext { video: "…".into(), start_s: 60.0, end_s: 66.0,
                               frames: vec![], comments: vec![], situation: None }),   // or None
})?;
// reply.text cites [S1] and names moments as times; reply.sources; reply.experts
// (every expert comment given); reply.comments (timed AiComments, only with a video)
```

`review` and `chat` block (seconds to a minute); call them from a blocking
task. In a chat, a translation request is an ordinary message: the persona
translates with the glossary's names for the target language, and explains a
bare callout before translating it. `translate` and `explain` are the
translator's own calls (the lab's Translate view), without a knowledge
store: only the glossary terms the text mentions go along, the target
language's names first.

The lab keeps one `Store` and `E5Embedder` for everything (search, imports
and the chat) instead of a `Reviewer`: `review::chat(&store, &embedder,
&client, k, &request)`, `review::review(...)` and `review::ask(...)` take the
parts separately, with a `Client::from_env` made per request. With a client
that has the knowledge tools (`Client::has_tools`: the CLI backend and
`Settings::mcp_relay`, the lab's own binary) it asks the agentic way:

```rust
use cuttlefish::tools::{Library, Session};
let library = Library::new(&knowledge, Some(sessions));   // kept: the corpus, the markers
let store = std::sync::RwLock::new(store);                 // read-locked per lookup
let tools = Session::new(&store, &embedder, &library, review::first_source_id(&request.history));
let reply = review::chat_with_tools(&tools, &client, &request)?;   // reply.lookups: what it looked up
let answer = review::ask_with_tools(&Session::new(&store, &embedder, &library, 1), &client, "...")?;
```

and for the one-shot path's two-pass chat over a long range
`review::key_moments` gives the second call's request;
`review::translate(&client, &glossary, text, target)` and
`review::explain(...)` take only the glossary. Imports go
through `ingest` (`web`, `youtube`, `files`, `discord_export`, `discord_bot`),
which hand documents to an `ingest::Sink` (the CLI prints; the lab logs
into its import job) and stop when `Sink::cancelled` says so. Its Knowledge
view shows all of this, and `inbox::import` does the inbox with the same kind
of sink.

`AiComment` serializes as:

```json
{"t_s": 124.5, "t_end_s": 128.0, "text": "...",
 "shapes": [{"kind": "arrow", "x0": 0.2, "y0": 0.7, "x1": 0.5, "y1": 0.5, "label": "basket"}],
 "sources": [{"id": "S2", "title": "...", "heading": "...", "url": "...",
              "source": "discord-vod-review", "license": "..."}]}
```

Shapes are in 0-1 frame coordinates (x right, y down); a box goes from its
top-left to its bottom-right corner, an arrow from tail to head.

## Design: knowledge far larger than the context

The sources add up to far more text than fits in one request, and most of it
is irrelevant to any one moment. So the model gets a small, stable core
every time and the relevant slice of the rest on demand.

**1. Retrieval, per request.** Every document is split into chunks of about
400 tokens (by headings, then paragraphs, then sentences, with about 60
tokens of overlap when a section is cut) and embedded with
`intfloat/multilingual-e5-small`. A review retrieves the 4 expert comments
(see [Expert comments](#the-vod-review-corpus)) and the 8 other chunks
nearest to the player's question plus the comments already on the moment and
its situation (without a question, a "fundamentals" query). Asking: nearest
to the question.

"Nearest" is **hybrid**: the embedding's cosine plus a keyword score
(`keyword.rs`, BM25 over each chunk's title, heading and text, kept in
`index/keywords.json` beside the vectors). E5-small places near-identical
cards (the five waves of one Eggstra Work event, wave 3 of two events)
within a few thousandths of each other, so "What spawned in wave 3 of
Eggstra Work #7?" used to land on another event's wave; exact terms decide
there. Tokens: Latin words (plural `s` dropped, a few stop words out),
numbers, and identifiers (`#7`, also from "work 7" and the CJK counters;
`wave 3`, also from `W3`, `wave3` and the CJK wave counter; `333%`); CJK text
as character pairs. The query's glossary terms (by any name or approved
alias, `find_in`) are matched by every official name in every language,
each term counting once as its best-matching name, so slang like 鬼坝 finds
the English Spawning Grounds pages and a page that lists a name in ten
languages does not outrank one that uses it. The score is
`cosine + 0.1 x (weight - 1) - S2 penalty + 0.2 x BM25 / best BM25 + 0.1 x
exact`, where `exact` (game-data cards only) is the share of the query's
event numbers, waves, percentages and names that the card's title holds,
or, without numbers in the query, 1 when the card is about the named thing
("Steelhead (Salmonid, game data)" for a question about the Steelhead).
`cuttlefish eval retrieval` measures it (below, point 5).
The keyword index follows the vectors (add, delete, reindex) and is written
with them; when it is missing, from an older tokenizer, or out of step (the
vectors synced in from another machine), opening the store rebuilds it from
`entries.jsonl` in about 0.1 s for 4,300 chunks, no embedding needed, and
the next write saves it.

Retrieval runs *before* the call, not as a tool the model calls: one
request, predictable cost and latency. A search tool for the model is the
next step if single-shot retrieval misses too often (the model could then
query "Stinger at low tide on Sockeye Station" once it recognizes the
stage).

**2. The core: curated digest.** `<data>/digest.md` is a hand-written (or
model-drafted, then human-checked) summary of the fundamentals: the
priorities, egg-flow rules and per-stage/per-tide plans that the best
sources agree on, a few thousand tokens. It sits in the system prompt, which
is cached, so it costs a tenth of normal input after the first call. Keep it
short and opinionated; retrieval covers the long tail.

Before it, the system prompt holds the rules of an answer and the official
names. The rules: short and specific, the answer first; every claim cites
the excerpt it rests on or is marked as the model's own guess; when the
excerpts do not cover the question, one line saying so and no filler; no
word about excerpts that do not bear on it. The names (`review::names_block`,
made with the glossary as `Store::names`): every Salmonid and Salmon Run
stage of Lean's name table with its official Splatoon 3 name in English,
Japanese and Simplified Chinese, and the nicknames players use (approved
slang, some from Splatoon 2) marked as nicknames, so an answer names every
Salmonid right whatever the question mentions and never claims a name does
not exist. A prompt's glossary block says the first name in each language
is the official one.

**3. Glossary for jargon.** `glossary.toml` lists terms with official names
per language (`forms`), a definition, and aliases: the slang players use,
each with its language, a note on its origin or use, a source (`seed`,
`user`, `suggested`, `imported`) and a status (`approved`, `pending`,
`rejected`):

```toml
[[term]]
id = "grizzco-roller"
definition = "The Grizzco Roller, a Grizzco weapon."
forms = { en = ["Grizzco Roller"], ja = ["クマサン印のローラー"], zh = ["熊先生印章滚筒"] }
aliases = [
  { lang = "en", text = "G Roller" },
  { lang = "zh", text = "熊刷", note = "'bear brush'" },
]
```

Terms found in a query (by an official name or an approved alias) add their
other-language official names to the query (a Japanese question finds
English notes; the embedder is multilingual too), and the matched terms go
into the prompt, each alias on a line of its own (`zh slang: 熊刷 →
熊先生印章滚筒 (en: Grizzco Roller): 'bear brush'`), so answers and
translations use the community's names and resolve its slang; the prompts
say players use slang and to say when a slang word is unclear. The seed has
English, Japanese, the official Simplified Chinese names (stat.ink's zh-CN
translations where it has them: 金鲑鱼, 鲑坝, 喇叭镭射5.1ch, 熊先生印章滚筒,
蛋筐), Chinese players' slang (熊刷, 鬼坝, 破船, 喇叭 and 雷神 for the
Sploosh-o-matic, 小绿 for the Splattershot, 筐, 家里, ...), and the player's
own jargon (惯性取消, 搬蛋, 熊武, 镭射, 出差, 小枪, 外围蛋) as aliases of terms
with descriptive English names; lines marked `# unsure:` are names to check.
Add Spanish, Russian and French names there.

**Slang the user teaches** (`slang.rs`) lives in `<data>/glossary-user.toml`,
apart from the generated glossary, so re-importing name tables never
overwrites it; `Store::load_glossary` adds its approved aliases last. Each
alias there names its term by id and by the English name it had when taught,
which finds the term again when a re-import gives it another id.
`UserGlossary::add`, `edit` and `remove` change it (an official name of any
term cannot be an alias). **Suggestions**: `slang::plan` cuts the community
documents (all but wikis, most trusted first) into batches of text not read
yet (the file's `[scanned]` table keeps how far each document was read) and
says how many there are; `slang::run` sends the batches, a few at once
(3 by default, at most 8), each with the glossary entries of the terms it
mentions and the core terms in brief (`suggest_prompt`), and keeps the
candidates (alias, language, term, a quote, a confidence, a note) that the
text contains, whose term the glossary has, and that are no name yet, as
`pending` aliases with source `suggested`. The user approves (status
`approved`), edits or rejects them (kept as `rejected`, never proposed
again). A run reads at most `max_batches` (5 by default, 50 at most), or,
with `all`, everything not read yet (`max_batches` then a safety limit); a
failed batch is read again by the next run, and a document's `[scanned]`
mark only moves over stretches read in order.

**Auto-apply**: `Found::auto_apply` approves what the model is at least a
threshold sure of (0.6 by default; the lab's `[cuttlefish]
slang_auto_apply` and `slang_threshold`), marked `auto = true` so the page
can list and undo it (`UserGlossary::undo` rejects it). **New terms**: when
the slang names something narrower than any term (the Flyfish's missiles),
or known slang points at a term too broad for it, the model proposes a new
term (`new_terms` in the answer: English name, kind, definition, relation
`part-of` / `kind-of` / `related-to` and the broader term, confidence,
aliases). It is kept as a `[[term]]` of the user file (`UserTerm`, with a
`glossary::Relation`) and its aliases name it by id; approved, it joins the
glossary (`UserGlossary::apply`, before the aliases), and prompts show
`missiles → Flyfish missiles (part of Flyfish)` and `[attack, part of
Flyfish]`. A new term the user rejected, or the glossary has, is never
proposed again. **Moves**: an approved alias whose text an approved new
term's alias also has (`missiles` of the Flyfish, then of Flyfish missiles)
is offered to move to the new term (`UserGlossary::moves`, `apply_move`).
**Editing terms** (`UserGlossary::edit_term`, the lab's `POST
knowledge/slang/term-edit`): a new term's name, kind, definition and
relation change in place and the term becomes the user's (source `user`;
suggestions never change a term that exists); a term of the generated
glossary (the seed, `glossary.toml`, an import) keeps its names, and the
definition, kind and relation you give are kept as an `[[override]]` of the
user file, applied on every load (`UserGlossary::apply`), so a re-import
never loses them (`term-reset` drops one). CLI: `cuttlefish slang suggest
[--all] [--max-batches N] [--parallel N] [--no-auto-apply] [--threshold T]
[--dry-run]` and `cuttlefish slang move [--dry-run]`.

**4. Source quality.** Each document has a weight: expert notes 1.3,
#vod-review 1.2, guides 1.15, game-data fact cards 1.1, wikis/Discord/X/
Xiaohongshu/files 1.0, web pages and video transcripts 0.9 (`--weight` overrides). Ranking adds 0.1 x (weight - 1) to the cosine, which
reorders close matches without burying a clearly better one, and takes
0.02 off a source of the Splatoon 2 era (`game.rs`: Discord conversations
get their era from their date at import, and every document may carry
`game`), so the current game comes first among close matches and the older
one still follows. Each excerpt the model sees is labelled with its source
kind, its era when known (`era="[Splatoon 2 era]"`) and, for a #vod-review
conversation, the video it is about; its lines are `[date] reviewer:
comment`, and the prompt tells the model to quote such advice by reviewer
and year ("Centritide, 2023: ...") and to say when it leans on the older
game. Documents imported before the era existed get it from their first
message's date when indexed (`cuttlefish reindex`). The prompt tells the
model that high-level review outweighs generic pages and to cite only
excerpts it was given; every source keeps its license and url so citations
are clickable.

Names are never evidence. What retrieval hands the model (`review::retrieve`,
for reviews, questions, chats and the deep eval) leaves out, by
`Store::is_evidence`: the documents that came with a *name source*, an
archive or folder at the inbox's top that also gave name tables
(`inbox::name_source_documents`, from `inbox.json`: stat.ink's repository
gave the glossary its names and, with them, its README, API pages and data
files as documents), and a page's table of names (`store::is_name_table`:
Inkipedia's "Names in other languages" and "Internal names"). Those hold
names and keys, which the glossary already carries into the prompt. At most
two chunks of one document are among the `k` other excerpts
(`review::PER_DOCUMENT`), so a long thread cannot fill the list alone. The
lab's Search panel shows the same; `cuttlefish search` still finds
everything. `cuttlefish eval deep --dry-run [--lang zh]` prints what every
question of the bank would be given, counted by source kind and by
document, without asking the model.

**5. Evaluation.** `eval.example.toml` shows the format: questions with the
sources that should be retrieved and points a good answer makes. Build 30-50
real questions from #vod-review threads (with the answer the experts gave),
run `cuttlefish eval` after every change to sources, chunking, weights or
prompts, and `--answer` before changing models or prompts. Retrieval misses
are the cheapest to find and fix; for answers, read them next to the expected
points (a model-graded rubric is the next step).

`cuttlefish eval retrieval` runs the crate's own set,
`questions/retrieval.toml` (45 questions with their target chunks: Eggstra
Work events and waves, hazard levels, weapons and Salmonids by name, slang,
Chinese and Japanese questions over English pages, #vod-review expert
comments by message id, and paraphrases without the names), and any eval
file prints the same table: each question's rank under embeddings, BM25 and
hybrid, then recall@1 and recall@5. On the store of 2026-09-27 (4,259
chunks: wiki, game-data cards, #vod-review, stat.ink files):

| Ranking   | recall@1 | recall@5 |
|-----------|----------|----------|
| embedding | 15/45    | 20/45    |
| BM25      | 30/45    | 40/45    |
| hybrid    | 38/45    | 43/45    |

The weights (`store::KEYWORD_SCALE` 0.2, `store::EXACT_BOOST` 0.1) were the
best of a grid on this set. Name tables (wiki "Names in other languages",
the stat.ink API's response samples) still come up for broad questions,
under either ranking.

**6. When fine-tuning would make sense.** Not for knowledge: facts change
with patches and rotations, and retrieval updates by re-ingesting. It could
pay off later for *style and judgment* (reviews that sound like the best
reviewers, calibrated boss priority), once there are a few thousand accepted
review comments from the lab to learn from, or to distil a cheap model
for high-volume work. Until then prompt + digest + retrieval is cheaper to
iterate on.

**7. Cost per review.** Claude Opus 5.5 is $4 per million input tokens and
$20 per million output tokens (thinking counts as output). An image
costs about width x height / 750 tokens: 1280x720 is about 1,230 tokens, 640x360
about 310. A review with 20 frames at 720p plus 8 excerpts (about 4k tokens)
is about 29k input tokens ($0.12) and 2-4k output tokens including thinking
($0.04-0.08): **about $0.15-0.20**. With 10 frames at 640x360: about 7k input,
**about $0.07**. The cached system prompt and digest (cache reads at $0.20
per million) are negligible. Fewer, smaller frames and `--effort medium` are
the levers; the Batch API halves prices for offline work such as drafting
digests.

**8. Scaling the store.** The flat index scans every vector: 100k chunks x
384 floats is 150 MB and searches in tens of milliseconds, enough for
every source listed here. Beyond that, implement `index::VectorIndex` with an
approximate index (HNSW) or a vector database; documents, chunks and
metadata stay as they are. Other upgrades behind the same interfaces: a
larger embedder (BGE-M3, 568M parameters, 8k-token inputs, also XLM-RoBERTa
based) with `reindex`; re-ranking the top 30 with a cross-encoder; CUDA
embeddings with `--features cuda`. Hybrid keyword + vector search is in
(point 1): its BM25 scan checks each chunk's sorted terms per query token
(the eval's 135 searches, embedding each query, take under 9 s on a CPU);
the keyword file is about half the size of
the vectors (3.3 MB for 4,259 chunks). Far beyond, an inverted index
(postings per term) would replace the scan.

### Embedding model

`intfloat/multilingual-e5-small` (MIT license): 118M parameters, 384
dimensions, trained on about 100 languages including Japanese and Chinese,
a standard BERT architecture that candle runs directly, and fast on a CPU
(about 470 MB of weights). It reads at most 512 tokens, which is why chunks
stay near 400. The revision is pinned so vectors never mix model versions.
BGE-M3 (MIT) retrieves better on long and mixed-language text but is five
times larger; it is the natural upgrade once a GPU is available.

### Model calls

`llm.rs` posts to `https://api.anthropic.com/v1/messages` with ureq: the
system prompt as a cached block, the earlier turns of a chat as plain text
messages, one user turn (knowledge excerpts, glossary, existing comments,
frames as base64 JPEG, the task), adaptive thinking (always on for Claude
Opus 5.5) with an explicit effort, and for reviews and chats a JSON schema
(structured output) for the comments (a chat's has the reply text too). Refusals and
truncated answers are reported as errors; 429 and 5xx are retried twice.
Excerpts are marked as reference material, not instructions, since they
come from the web and chat. Unit tests never touch the network: requests
are built and answers parsed by pure functions, and a fake transport stands
in for HTTPS.

**The Claude Code CLI as backend.** `llm::Backend` (`--backend`,
`$CUTTLEFISH_BACKEND`, the lab's `[cuttlefish] backend`) is `auto`, `api`
or `claude-cli`; `auto` takes the API when `ANTHROPIC_API_KEY` is set and
otherwise the `claude` on PATH. `claude_cli.rs` runs `claude -p` headless
with the same prompt: the system prompt (persona, rules, digest) replaces
Claude Code's own through `--system-prompt`, and the user turn (knowledge
excerpts, glossary, comments, frames as base64 JPEG image blocks, the task)
is one `stream-json` message on stdin, so the CLI needs no tools and runs
with none (`--tools ""`, `--restricted`, `--strict-mcp-config`, no settings
files, no session saved) in an empty temporary folder. Earlier turns are
rendered into the message; a JSON schema is asked for in words and the answer
parsed leniently. `--model` is always passed (the configured model, else
`claude-opus-5-5`), so an answer never depends on the CLI's own default, and
`--effort` too; the model that answered is read back from the result's
`modelUsage` (the one that wrote the most). `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed from the
CLI's environment, so it uses the account it is logged in with: this backend
runs on your Claude subscription and counts against its usage limits, and is
meant for personal testing. At most eight runs at once; a run is stopped after
ten minutes. A missing `claude` or a missing login are reported as such. The
CLI's own usage figures are logged like the API's; there is no prompt cache
to manage. Tests run a fake instead of the process.

### Knowledge tools: the model looks things up

Twelve excerpts retrieved beforehand leave the model stuck when retrieval
picks the wrong ones, and a question in Chinese barely reaches the English
wiki or Lean's fact cards (keywords match their own language only). So on
the Claude CLI (`Client::has_tools`), questions (`review::ask_with_tools`,
`cuttlefish ask`, the deep eval) and chats (`review::chat_with_tools`, the
lab) retrieve nothing beforehand: the model gets the question (with a
video's frames, comments and moment), a system prompt of its own
(`review::tools_system_prompt`: look things up first; search in English and
in Simplified Chinese with the official names; open what you cite; trust
the player's notes, then #vod-review expert comments, then game data, then
Inkipedia, then other Discord channels, then RedNote and X; say in one line
when nothing covers the question; with a video, the review rules and how
far estimated input can be trusted; then the official names and the digest)
and five read-only tools (`tools.rs`):

| Tool | Takes | Gives |
| --- | --- | --- |
| `search` | `query` (any language), `kinds` (source kinds), `era` (`S3`: no Splatoon 2 material; `S2`: only that), `limit` (8, at most 20) | hybrid results (E5 and BM25, the query expanded with the other-language names of the terms it mentions), at most two passages a document, never a name table: id, kind, era, place, snippet |
| `open` | `id`, `around` (1, at most 3) | the passage whole with the passages before and after it (each with its id), its link, licence and video; a #vod-review message with the message it replies to and its replies |
| `pedia` | `term` (a name in any language, a nickname or an id) | the term's entry: names in every language, slang, relations both ways, its game-data fact cards (numbers in players' units, the raw parameters one `open` away), the player's expert notes, the #vod-review comments that mention it (the best five) and the examples the player recorded (the sessions' technique markers) |
| `thread` | `id` of a #vod-review result, a message link or a VOD id | the conversation of the corpus: the VOD, then every message with its moments in the video |
| `names` | `text` (a name or a sentence) | the terms it refers to with their English, Japanese and Chinese names and slang, and possible short forms |

A `Session` serves one answer: every source a tool shows gets an id `S<n>`
in the order first shown, starting past the highest id the conversation
cited (`review::first_source_id`), so an old answer's `[S3]` never names
another source; the answer's citations resolve against what was shown
(`Session::cited`), and every call is kept as a `Lookup` (tool, arguments,
the ids and titles it showed, its error, how long it took), which answers,
chat messages and eval rows carry and the lab shows folded under the
answer. The store is read-locked per call, never for a whole answer; a
`Library` keeps what the tools read besides the store (the corpus, read
again when its file changes; the sessions' markers, for five minutes). An
answer may make 40 lookups; the next are told to answer.

The tools reach the CLI as an MCP server (`mcp.rs`, JSON-RPC lines:
`initialize`, `tools/list`, `tools/call`, `ping`). The process that asks
(the lab, or `cuttlefish ask`) has the store and the embedder loaded
already, so it serves the tools itself on a Unix socket in the run's
private folder (`mcp::Listener`), and the MCP server the CLI starts is its
own binary as `<program> mcp --socket <path>` (`Settings::mcp_relay`), a
relay of bytes that starts at once and loads nothing. The CLI gets
`--mcp-config` with that one server, `--strict-mcp-config`, `--tools ""`
(no built-in tool at all) and `--allowedTools` naming the five, which run
without asking; anything else would ask and `--permission-prompts none`
denies it. `cuttlefish mcp` without `--socket` serves the same tools on its
stdin and stdout over the store it opens (read only), for any MCP client:
`claude mcp add cuttlefish -- /path/to/cuttlefish mcp --data /path/to/Knowledge`.
The one-shot path stays for the API backend, `--no-tools`, the translator
and the reviews of a stretch; the scout pass of a long range
(`review::key_moments`) has no tools either.
