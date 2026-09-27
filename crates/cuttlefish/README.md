# Cuttlefish

Knowledge store and model backend for Cuttlefish, the studio's AI reviewer
for Splatoon 3 Salmon Run. It imports guides, wikis, video transcripts and
Discord VOD-review discussions into a local store; retrieves what matters for
a moment of gameplay or a question; and asks Claude (Anthropic Messages API)
with the frames, the retrieved knowledge and a jargon glossary. Library
(`cuttlefish`) plus a CLI of the same name.

## Quick start

```bash
cargo build --release -p cuttlefish
alias cuttlefish=target/release/cuttlefish

# The knowledge folder is the studio's: found through its config (--config,
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
cuttlefish fetch discord --channel 123456789012345678   # your own account: against Discord's terms, see below

cuttlefish search "バクダンの処理"          # top-k chunks with sources; any language
cuttlefish glossary "Steelhead"            # a term's names and definition
cuttlefish stats                           # documents, terms per language, tables, assets
cuttlefish docs                            # every document with its id
cuttlefish delete 4d7e4072ed28b64e         # a document and its chunks
cuttlefish eval eval.example.toml          # retrieval check, no key needed

export ANTHROPIC_API_KEY=...               # only ever from the environment, or the env file
                                           # scripts/run.sh loads (~/.config/procon/env)
# ...or none: with the Claude Code CLI installed and logged in, `auto` (the
# default) runs `claude -p` on your own subscription; --backend claude-cli forces it
cuttlefish ask "When should I leave the basket to kill a Stinger?"
cuttlefish translate "Kill the Steelhead before the Flyfish" --to ja
cuttlefish eval eval.example.toml --answer
```

The first command that embeds downloads the embedding model (about 470 MB)
into `~/.cache/procon-cuttlefish/models/` (`$CUTTLEFISH_CACHE`, else
`$XDG_CACHE_HOME/procon-cuttlefish`), on this machine rather than in a synced
data folder. `--model` / `$CUTTLEFISH_MODEL` picks the model
(default `claude-opus-5-5`), `--effort` its effort (default `high`).
`RUST_LOG=debug` shows more.

## Data folder

```text
<data>/
  inbox/             drop anything here (see "The inbox")
  inbox.json         what each inbox file gave, with its size, time and hash
  glossary.toml      your glossary; the crate's glossary.toml seed until you add one
  glossary-user.toml slang you taught or approved in the studio (never overwritten)
  terms/<id>.json    name tables imported from the inbox, merged into the glossary
  assets.json        images and icons from the inbox
  reports/<t>.json   one report per inbox import (the last 30)
  digest.md          curated fundamentals, sent with every request (optional)
  raw/<kind>/        pages, subtitles, exports as received
  docs/<id>.json     processed documents with source, url or inbox path, title,
                     language, license, attribution, revision (wiki pages),
                     fetch time, weight and text
  index/             meta.json, entries.jsonl, vectors.f32
~/.cache/procon-cuttlefish/ models/, thumbs/, unpack/: on this machine only
```

Ingesting the same url again replaces its document (`--refresh` refetches
pages already stored). Opening the store embeds the documents the index
lacks, 32 chunks at a time; the studio shows "embedding N of M chunks" in the
running import and stops when the import is cancelled (what was embedded is
kept, the rest waits for the next opening). `cuttlefish reindex` re-embeds everything after a
change of embedder or chunk sizes; `cuttlefish delete <id>` removes a document.

The data folder can be a synced folder (Dropbox, rclone mount). Every file is
written whole (temporary file, then rename) and nothing is locked. Files that
do not read (half-synced, `(conflicted copy)`) are skipped with a warning.
Documents are the truth: opening the store drops chunks of documents that are
gone, rebuilds an index that does not read, and embeds documents that arrived
from another machine. The index is rewritten every 50 documents of an import
and at its end. The folder's parent must exist, so an unmounted synced folder
is not silently replaced.

The data folder of before (`$XDG_DATA_HOME/cuttlefish`, usually
`~/.local/share/cuttlefish`) is shared with another program, so
`store::migrate` (the studio at startup, the CLI on each run) handles only our
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
repositories. `cuttlefish ingest inbox` (or **Import inbox** in the studio,
which can also upload into it) looks at each file by name and first bytes:

| File | Taken as |
|---|---|
| md, txt, rst, org, adoc, html, pdf, docx, srt, vtt | a document (chunked, embedded), keyed by its inbox path |
| DiscordChatExporter JSON, or `<id>.messages.jsonl` of `fetch discord` (with its `<id>.channel.json` beside it) | its conversations; the fetcher's channel objects and `state.json` are skipped |
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
by content later.

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
| Guides ("Overfishing Fundamentals", Lenny, ...) | `ingest file` (md, txt, html, pdf) or `ingest url` | `--source guide` (weight 1.15); record the license with `--license` |
| Inkipedia, other MediaWiki wikis | `ingest wiki <start pages or categories>` (the studio: **Wiki / site**, MediaWiki topic) | A whole topic through the API, re-runs fetch only changed pages; the wiki's license (from `siteinfo`) and "<wiki> contributors" kept per document. See "Whole wikis and sites" |
| A whole site | `ingest site <start address>` (the studio: **Wiki / site**, Whole site) | Same host only, a page cap, assets and given paths skipped. See "Whole wikis and sites" |
| Other pages, stat.ink docs | `ingest url` (urls, `--list`, `--sitemap`) | robots.txt obeyed, one request per site every 3 s or the site's `Crawl-delay` |
| Google Docs, Sheets, Slides | `ingest url <the address you share>` | Read through their exports (see below); only files shared as "Anyone with the link can view"; a sheet's tabs one by one with `--all-tabs` |
| YouTube | `ingest youtube <video/playlist/channel>` | `yt-dlp` fetches subtitles and metadata only; uploaded subtitles preferred over auto captions |
| Discord #vod-review | `ingest discord-export`, `ingest discord-bot`, or `fetch discord` + `ingest inbox` | The export and the bot are the sanctioned ways; `fetch discord` reads with your own account, against Discord's terms. See below |
| Twitter/X, Twitch | not automated | X's API terms and pricing rule out scraping; save the posts or threads you value as text and `ingest file`. Twitch VODs have no subtitles (a speech-to-text step would be needed) |

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
without a `gid`, or any with `--all-tabs` (the studio's **Every tab of a
Google Sheet**), brings every tab, each a document of its own under its
tab's address: the tabs are listed from the sheet's HTML view
(`/htmlview`);
slides as text (`export/txt`). The document is stored under the address you
gave, so importing it again with `--refresh` (in the studio: **Again if
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

**Local files.** `ingest file` (the studio's **Files**) reads the prose
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

Channel messages are grouped into conversations (split at 2-hour gaps);
threads stay whole. Each message line carries its time and author
(`[2024-05-01 10:05 UTC] Alice: ...`), the authors are the document's
attribution. Clips stay as links; they are not downloaded. A channel
named `vod-review` (or its threads) gets the highest weight (1.2).

Every Discord document also keeps the **moments** its messages point at, as
`moments` in the document's JSON, for aligning comments with a video later:
YouTube links (with the `t=` start when there is one), waves and times
written in the text ("W1 :50", "wave 2 at 1:20", "86s", "1:02:03", "wave
3"). Each moment has the text as written (`raw`), the message id, author
and time it came from, and a guess of what it means (`kind`): the wave
timer counts down from 100 s, so a bare number of seconds up to 100 and any
time named with a wave is `wave_timer`; `m:ss` and `1m20s` forms and
anything over 100 s are `video_time`; a wave with no time is `unknown`.

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
  threads started by its messages). Every request is checked against that
  scope before it is made. No gateway or websocket, no typing, no writes.
- **Slow.** A random delay between requests, 3 to 8 s by default (`--delay
  3-8`); every 40 to 120 requests (`--pause-every`) a longer pause of 1 to
  5 minutes (`--pause 60-300`); each drawn anew. A 429 is waited out as its
  `retry_after` asks and the `X-RateLimit-*` headers are obeyed; server
  errors back off exponentially; a 401 or 403 stops the run with a clear
  message and nothing else is tried. `--daily-cap N` stops after N requests
  in a day (UTC); `--max-requests N` and `--max-minutes M` end a run early,
  the next run continues. The User-Agent is a desktop browser's.
- **Resumable and incremental.** The API's JSON is kept as received:
  `<id>.channel.json` (the channel object), `<id>.messages.jsonl` (one
  message per line, appended), the threads in `threads/` the same way, and
  `state.json` with the cursors and the day's count. A run first fetches
  what is newer than the last one (skipped when the channel's last message
  is known), then keeps backfilling older history until the first message.
  Ctrl+C finishes the request under way, saves and stops. Attachments and
  embeds stay as links in the JSON; nothing is downloaded.
- Progress after each page: `#vod-review: 1300 messages, oldest 2024-03-02;
  14 requests this run, 14 today; delay 5.3 s`, and a line for each pause
  or wait.

By default each channel goes to `<knowledge>/inbox/discord/<guild
id>/<channel id>/` (the knowledge folder found as for every other command:
`--config`, else `./config.toml`, else `--data`, else `$CUTTLEFISH_DATA`),
where **`cuttlefish ingest inbox`** (or the studio's Import inbox) reads it:
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
only; the tool never prints or writes it. **The channel id**: in Discord,
User Settings > Advanced > Developer Mode, then right-click the channel (or
the server for `--guild`) > Copy Channel ID.

```bash
# The token in the env file, readable by you only
chmod 600 ~/.config/procon/env      # after adding: DISCORD_USER_TOKEN=...

# A check run: the channel object and one page of messages, then stop
cuttlefish fetch discord --channel 123456789012345678 --max-requests 2

# The archive: newer messages first, then older history; run it again any time
cuttlefish fetch discord --channel 123456789012345678

# Gentler still: two hours a run, at most 600 requests a day
cuttlefish fetch discord --channel 123456789012345678 --max-minutes 120 --daily-cap 600

cuttlefish ingest inbox             # the archive into the store
```

At the default pace a page of 100 messages takes about 5.5 s plus the
pauses, so 20,000 messages (200 pages) take roughly 20 to 30 minutes, and
each thread or forum post at least one more request; a forum with 300 posts
adds about half an hour.

**Whole wikis and sites** (`wiki.rs`). Both are as polite as the rest: one
request at a time, `--delay-s` (default 2 s, at least 1) or the site's
`Crawl-delay` when longer, every `robots.txt` rule for `Cuttlefish` (else
`*`), and a `robots.txt` answering with a server error stops the import. A
`--dry-run` (the studio's **Dry run: count the pages first**, on by
default) tells what is in scope and how long fetching it would take, and
stores nothing. Progress and **Cancel** work as for any import.

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
  fetches all). Every request carries `maxlag=5`: a lagging or busy wiki
  is left alone for the time it asks (or half a minute) and asked again.
  Raw pages go to `raw/wiki/`.
- *Whole site* (`ingest site`): from a start address, the pages on the same
  host reached through links and through the sitemaps `robots.txt` names
  (else `/sitemap.xml`), up to `--max-pages` (default 100). Images,
  scripts, styles, fonts and feeds are skipped, and so are the path
  prefixes given with `--skip` (an app such as a map viewer). A page drawn
  by JavaScript is noted and not kept. The notes end with the sections
  found (pages per first path segment). A re-run reads the links of pages
  already stored from their raw copy (`raw/web/`) instead of fetching them
  again; `--refresh` fetches everything. A dry run needs no page when the
  site has a sitemap; without one it still fetches pages to find links,
  but keeps nothing.

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

## Library API (for the studio)

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
                               frames: vec![], comments: vec![] }),   // or None
})?;
// reply.text cites [S1] and names moments as times; reply.sources; reply.comments
// (timed AiComments, only with a video)
```

`review` and `chat` block (seconds to a minute); call them from a blocking
task. In a chat, a translation request is an ordinary message: the persona
translates with the glossary's names for the target language, and explains a
bare callout before translating it. `translate` and `explain` are the
translator's own calls (the studio's Translate view), without a knowledge
store: only the glossary terms the text mentions go along, the target
language's names first.

The studio keeps one `Store` and `E5Embedder` for everything (search, imports
and the chat) instead of a `Reviewer`: `review::chat(&store, &embedder,
&client, k, &request)`, `review::review(...)` and `review::ask(...)` take the
parts separately, with a `Client::from_env` made per request;
`review::translate(&client, &glossary, text, target)` and
`review::explain(...)` take only the glossary. Imports go
through `ingest` (`web`, `youtube`, `files`, `discord_export`, `discord_bot`),
which hand documents to an `ingest::Sink` (the CLI prints; the studio logs
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
`intfloat/multilingual-e5-small`. A review retrieves the 8 chunks nearest to
the player's question plus the comments already on the moment (without a
question, a "fundamentals" query). Asking: nearest to the question.
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
says how many there are; `slang::suggest_batch` sends one batch with the
glossary entries of the terms it mentions and the core terms in brief, and
keeps the candidates (alias, language, term, a quote, a confidence, a note)
that the text contains, whose term the glossary has, and that are no name
yet, as `pending` aliases with source `suggested`. The user approves (status
`approved`), edits or rejects them (kept as `rejected`, never proposed
again). A run reads at most `max_batches` (5 by default, 50 at most).

**4. Source quality.** Each document has a weight: #vod-review 1.2, guides
1.15, wikis/Discord/files 1.0, web pages and video transcripts 0.9
(`--weight` overrides). Ranking adds 0.1 x (weight - 1) to the cosine, which
reorders close matches without burying a clearly better one. The prompt
tells the model that high-level review outweighs generic pages and to cite
only excerpts it was given; every source keeps its license and url so
citations are clickable.

**5. Evaluation.** `eval.example.toml` shows the format: questions with the
sources that should be retrieved and points a good answer makes. Build 30-50
real questions from #vod-review threads (with the answer the experts gave),
run `cuttlefish eval` after every change to sources, chunking, weights or
prompts, and `--answer` before changing models or prompts. Retrieval misses
are the cheapest to find and fix; for answers, read them next to the expected
points (a model-graded rubric is the next step).

**6. When fine-tuning would make sense.** Not for knowledge: facts change
with patches and rotations, and retrieval updates by re-ingesting. It could
pay off later for *style and judgment* (reviews that sound like the best
reviewers, calibrated boss priority), once there are a few thousand accepted
review comments from the studio to learn from, or to distil a cheap model
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
based) with `reindex`; hybrid keyword + vector search for exact names and
numbers; re-ranking the top 30 with a cross-encoder; CUDA embeddings with
`--features cuda`.

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
`$CUTTLEFISH_BACKEND`, the studio's `[cuttlefish] backend`) is `auto`, `api`
or `claude-cli`; `auto` takes the API when `ANTHROPIC_API_KEY` is set and
otherwise the `claude` on PATH. `claude_cli.rs` runs `claude -p` headless
with the same prompt: the system prompt (persona, rules, digest) replaces
Claude Code's own through `--system-prompt`, and the user turn (knowledge
excerpts, glossary, comments, frames as base64 JPEG image blocks, the task)
is one `stream-json` message on stdin, so the CLI needs no tools and runs
with none (`--tools ""`, `--restricted`, `--strict-mcp-config`, no settings
files, no session saved) in an empty temporary folder. Earlier turns are
rendered into the message; a JSON schema is asked for in words and the answer
parsed leniently. `--model` is passed only when one is configured, `--effort`
always. `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` are removed from the
CLI's environment, so it uses the account it is logged in with: this backend
runs on your Claude subscription and counts against its usage limits, and is
meant for personal testing. At most eight runs at once; a run is stopped after
ten minutes. A missing `claude` or a missing login are reported as such. The
CLI's own usage figures are logged like the API's; there is no prompt cache
to manage. Tests run a fake instead of the process.
