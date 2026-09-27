# Captures: Salmon Run posts from X and Xiaohongshu, slowly, with your own browser

Two tools share this folder, a Chrome profile (`~/.config/procon/browser-profile`,
logged into both sites once), a DevTools port (9251), and the libraries in
`lib/`: `xcap.mjs` for X (Twitter) and `rednote.mjs` for Xiaohongshu
(RedNote, 小红书), [below](#rncap-salmon-run-notes-from-xiaohongshu). Node
22+ and Google Chrome, no packages.

## xcap: Salmon Run posts from X

`xcap.mjs` captures the Salmon Run posts of the accounts you follow on X
(Twitter), each with its replies, into Cuttlefish's inbox
(`<knowledge>/inbox/x/<handle>/posts.jsonl`), where `cuttlefish ingest
inbox` (or **Import inbox** on the Knowledge page) turns each thread into a
community document (source kind `x`). Node 22+ and Google Chrome, no
packages.

## The risk, first

**Automating your own account can breach X's terms of service**, and an
account seen doing it can be limited or suspended. Slow, read-only use of a
real browser reduces the risk; it does not remove it. This tool exists
because that was weighed and accepted for one private knowledge base. It
does nothing an attentive reader would not: it opens pages in a real,
logged-in Chrome, scrolls them, and keeps the JSON the page loaded for
itself. It calls no private API of its own, signs nothing, forges nothing,
writes nothing to X, and downloads no media. It never sees, stores or
logs credentials: the Chrome profile folder holds the session, as any
browser's does.

## How it works

- **A Chrome of its own.** `~/.config/procon/browser-profile` (or
  `--profile`) is a profile folder used only by this tool. `login` opens a
  window on it for you to sign in once; `run` starts Chrome on it with a
  DevTools port (`--port`, 9251) or attaches to one already listening
  there (`--attach`). Your everyday Chrome and its profile are never
  touched.
- **What it visits.** `x.com/home` (are we signed in, and as whom), your
  following list (kept in the state for a week), each followed account's
  profile, scrolling down until it reaches posts the last run saw or the
  `--since-days` window (90 days), and the page of each post that is about
  Salmon Run, once, scrolling once more for replies.
- **What counts as Salmon Run** (`lib/filter.mjs`): a glossary match in
  English, Japanese or Simplified Chinese on the post's text or the post it
  quotes: the mode (サーモンラン, バイト, Salmon Run, 打工, 鲑鱼跑), Grizzco,
  Big Run, Eggstra Work, the bosses, lessers and Kings (バクダン, Flyfish,
  铁板鱼, ...), the events and the stages. Off-topic posts are noted in
  the state and never written; retweets and the account's replies to
  others are skipped.
- **Where the posts come from.** The GraphQL answers the page receives
  (`UserTweets`, `TweetDetail`, `Following`), read from the network events
  and walked for anything shaped like a post or a user, so a change in the
  page's layout does not matter. When a profile's answers were missed, the
  posts drawn on the page are read from the DOM instead.
- **Pace** (`lib/pace.mjs`): 4 to 10 s between page actions (a navigation
  or a wheel scroll of a few notches), drawn anew each time, plus 1.5 to
  3 s for the page to draw; a pause of 1 to 3 minutes every 15 to 30
  actions; at most 400 actions a run and 800 a day (UTC) by default; a
  `--max-minutes` limit if you like. A 429 answer, a 401, a 403 to the
  operations it needs, or the login page stops the run at once. Ctrl+C
  finishes the action under way and saves.
- **Resumable and incremental** (`lib/state.mjs`): `<inbox>/x/state.json`
  keeps the day's action count, the following list, a record per account
  (newest post id, last visit, counts) and every post id looked at, with
  what became of it (`kept`, `off-topic`, `retweet`, `reply`). A thread not
  reached before a cap is not marked, so the next run takes it.

## Steps

```bash
cd ~/Developing/procon-rs                   # where config.toml is: the knowledge folder is found as the cuttlefish CLI finds it

# 1. Once: sign in, in the window that opens (nothing is read while you type); the window closes when signed in
node tools/capture/xcap.mjs login

# 2. A dry run: browse a little, print what would be kept, write nothing (but the day's action count)
node tools/capture/xcap.mjs run --dry-run --max-actions 20

# 3. A real run: the following list, the profiles, the Salmon Run threads (about an hour with the default caps)
node tools/capture/xcap.mjs run

# Later runs continue where the last stopped; run it daily, or in an evening
node tools/capture/xcap.mjs run --max-minutes 45
node tools/capture/xcap.mjs status          # the accounts, counts and today's actions

# 4. Into the store: the Knowledge page's Import inbox button, or
cuttlefish ingest inbox
```

`--accounts a,b,c` reads only those accounts (no following list),
`--refresh-following` reads the list again now, `--me` gives your handle
when the page does not show it. `--config <studio config>` or `--data
<knowledge folder>` when not run beside `config.toml`.

**Where the files go:** `<knowledge>/inbox/x/<handle>/posts.jsonl`, one
JSON line per thread, appended; `<knowledge>/inbox/x/state.json` beside
them. The inbox import sees a changed file and replaces its documents; the
state file is skipped as the tool's own. Nothing else is written.

**Expected time.** A thread costs two page actions (its page, one scroll
for replies), a profile one to four, the following list a few once a week.
At the default pace that is about 20 s per page action and a 2-minute
pause every 22 or so, so **100 posts take roughly 60 to 75 minutes**,
profiles included, and the default caps (400 actions) give about 150
threads a run at most. `--delay 8-20 --pause 120-300` is gentler still.

## The record

```json
{"source": "x", "id": "1971…", "url": "https://x.com/ikura_coach/status/1971…",
 "author": {"handle": "ikura_coach", "name": "Ikura Coach"},
 "date": "2026-09-24T12:00:00.000Z", "text": "バクダンは湧いた瞬間に処理…", "lang": "ja",
 "urls": ["https://youtu.be/abc123"],
 "media": [{"type": "photo", "url": "https://pbs.twimg.com/media/one.jpg"},
           {"type": "video", "url": "https://pbs.twimg.com/…thumb.jpg", "video": "https://video.twimg.com/….mp4"}],
 "quoted": {"id": "…", "url": "…", "author": {…}, "date": "…", "text": "…"},
 "reply_to": null,
 "replies": [{"id": "…", "url": "…", "author": {…}, "date": "…", "text": "…", "lang": "ja",
              "urls": [], "media": [], "reply_to": {"id": "1971…", "handle": "ikura_coach"}}],
 "matched": ["バクダン"], "captured_at": "2026-09-27T10:00:00.000Z"}
```

Long posts carry their full text (the "note"); t.co links are written
out; media are links only, nothing is downloaded. In the store
(`crates/cuttlefish/src/x.rs`) each thread is one document titled by the
author, the day and the start of the text, with the post, its quoted post
and the replies as timestamped lines (`[2026-09-24 12:20 UTC] Wave 3
(@wave3) ↪ @ikura_coach: …`), the authors as attribution, the post's
language, and the game era from its date; weight 1.0 like a Discord
channel, below #vod-review.

## Tests

```bash
node --test tools/capture/test/     # GraphQL parsing on fixture answers, the filter, pacing and caps, state, files, the visit against a scripted page
```

No test touches X or a login; the fixtures are synthetic. The CDP layer
(`lib/cdp.mjs`) is the same pattern as `scripts/layout-check.mjs`.

## rncap: Salmon Run notes from Xiaohongshu

`rednote.mjs` captures the Salmon Run notes (笔记) of the creators you
follow on Xiaohongshu (RedNote, 小红书), each with its comments and their
replies, into Cuttlefish's inbox (`<knowledge>/inbox/rednote/<user
id>/notes.jsonl`), where `cuttlefish ingest inbox` (or **Import inbox** on
the Knowledge page) turns each note into a community document (source kind
`rednote`, `crates/cuttlefish/src/rednote.rs`).

### The risk, first

**Automating your own account can breach Xiaohongshu's terms of service.**
The site watches for it: a fresh browser or a new network can meet its
risk-control page at once ("IP at risk"), a slider or a forced re-login,
and an account seen automating can be restricted. Slow, read-only use of a
real, logged-in browser reduces the risk; it does not remove it. This tool
exists because that was weighed and accepted for one private knowledge
base. It does nothing an attentive reader would not: it opens pages in your
own Chrome, scrolls them, clicks a tile or a "more replies" button, and
keeps the JSON the page loaded for itself. It calls no API of its own,
makes no signed header (`x-s`, `x-t`), writes nothing to the site, and
downloads no media. It never sees, stores or logs credentials: the Chrome
profile folder holds the session, as any browser's does. **It never solves
a captcha**: anything that wants a person stops the run at once.

### How it works

- **The same Chrome as xcap.** `~/.config/procon/browser-profile` (or
  `--profile`), port 9251 (or `--port`); `login` opens a window on it for
  you to log in (the site shows a code to scan with the app) and leaves it
  open until you close it, `run` starts
  Chrome on it or attaches to one already listening (`--attach`). Your
  everyday Chrome and its profile are never touched.
- **What it visits.** The home page (are we logged in, and as whom: the
  site's `user/me` answer, else the page state), your own profile once for
  the following list (the web profile does not open it, so one of your own
  notes is opened, "@" typed into its comment box, whose picker lists the
  accounts you follow (`intimacy_list`), and taken back: nothing is sent;
  kept in the state for a week, `--refresh-following` reads it
  again, `--creators` gives creators yourself as profile links, ids or
  `@<file>`), each creator's profile, scrolling the notes list until it
  ends or, on later visits, until it shows only notes seen before, and each
  new note about Salmon Run: its tile is clicked (the list draws only the
  tiles near the viewport, so the page is scrolled toward the tile first;
  a tile not reached within `--tile-scrolls` (8) gives way to the note's
  address), the comments pane
  (`.note-scroller`) scrolled while the site says there are more
  (`--max-comments`, 200), folded reply threads unfolded (`--max-replies`,
  10), then Escape closes it.
- **What counts as Salmon Run:** the same three-language glossary as xcap
  (`lib/filter.mjs`), on the note's title as the list shows it (only
  matching notes are opened), or with `--match detail` on the whole note
  (title, text, tags: every new note is opened, one visit each). The
  creators you follow are Salmon Run creators already, so this keeps their
  other notes out.
- **Where the notes come from** (`lib/rednote.mjs`): the JSON the page
  receives (`user_posted` for a creator's list, `/feed` for a note,
  `comment/page` and `comment/sub/page` for comments and replies, the
  following list), read by shape, in the API's snake case and the page
  state's camel case (`__INITIAL_STATE__`, Vue refs unwrapped) alike;
  when a note's answers were missed, its title, text and comments are read
  off the page (`#detail-title`, `#detail-desc`, `.comment-item`).
- **Pace** (`lib/pace.mjs`): 8 to 20 s between page actions (a navigation,
  a wheel scroll of a few notches, a click, a key), drawn anew each time,
  plus the page's own settling; a pause of 2 to 8 minutes every 10 to 25
  actions; at most 150 actions a run and 450 a day (UTC) by default;
  `--max-notes` (60) and `--max-minutes` if you like. **A stop at once**
  on: an address holding `captcha`, `verify`, `login`, `risk` or
  `security`; a page whose text asks for a verification, a slider, a code,
  a login or reports an account or traffic anomaly; an answer with HTTP
  403, 461 or 471; a JSON answer with a code that means the session is not
  accepted (300011 to 300015, -100). The run reports what it saw and ends;
  pass the check yourself in the `login` window, and run again later,
  gentler (`--delay 8-20 --pause 120-600`). Ctrl+C finishes the action
  under way and saves.
- **Resumable and incremental** (`lib/state.mjs`, `tool: "rncap"`):
  `<inbox>/rednote/state.json` keeps the day's action count, the following
  list (creator ids), a record per creator (nickname, whether the whole
  list was scrolled once, last visit, counts) and every note id decided on
  (`kept`, `off-topic`, `failed`). A note not reached before a cap is not
  marked, so the next run takes it.

### Steps

```bash
cd ~/Developing/procon-rs                   # where config.toml is: the knowledge folder is found as the cuttlefish CLI finds it

# 1. Once: log in, in the window that opens (a code to scan with the app); close the window when done
node tools/capture/rednote.mjs login
# Outside China the site sends you to rednote.com: give every command where you log in
#   --site https://www.rednote.com

# 2. A dry run: the following list, a creator's notes, a note or two, printed; nothing written but the day's action count
node tools/capture/rednote.mjs run --dry-run --max-actions 20

# 3. A real run: the following list, the creators, the Salmon Run notes with their comments
node tools/capture/rednote.mjs run

# Later runs continue where the last stopped (new notes first); run it in an evening or daily
node tools/capture/rednote.mjs run --max-minutes 60 --max-notes 30
node tools/capture/rednote.mjs status       # the creators, counts and today's actions

# 4. Into the store: the Knowledge page's Import inbox button, or
cuttlefish ingest inbox
```

`--creators <link,id,...>` or `--creators @creators.txt` reads only those
creators (profile links `https://www.xiaohongshu.com/user/profile/<id>` or
24-character ids, one per line in the file); `--me <id>` gives your
account's id when the page does not tell it. `--config <studio config>`
or `--data <knowledge folder>` when not run beside `config.toml`.

**Where the files go:** `<knowledge>/inbox/rednote/<user id>/notes.jsonl`,
one JSON line per note, appended (a note read again adds a line; the
import keeps the last); `<knowledge>/inbox/rednote/state.json` beside
them. The inbox import sees a changed file and replaces its documents; the
state file is skipped as the tool's own. Nothing else is written.

**Expected time.** A note costs about 6 page actions: its tile, one or two
scrolls of the comments, a reply thread or two, Escape, and its share of
the list's scrolls. At the default pace (14 s between actions on average
plus the page's settling, a 5-minute pause every 17 actions or so) that is
about 3 to 4 minutes a note, so **100 notes take roughly 5 to 7 hours**
depending on how many comments they carry; the default caps (150 actions a
run) give about 25 notes a run, and the daily cap (450) about 75 a day.
`--max-comments 60 --max-replies 3` is quicker per note.

### The record

```json
{"source": "rednote", "id": "66aa…", "url": "https://www.xiaohongshu.com/explore/66aa…",
 "author": {"user_id": "5f00…", "nickname": "…"},
 "date": "2024-08-30T06:40:00.000Z", "updated": "2024-08-30T07:40:00.000Z", "kind": "video",
 "title": "打工400分教学", "text": "…", "tags": ["打工", "Splatoon3"],
 "images": ["https://sns-img…/1.jpg"], "video": "https://sns-video…/1.mp4",
 "likes": 12000, "collects": 3210, "shares": 12, "comment_count": 88,
 "comments": [{"id": "…", "author": {"user_id": "…", "nickname": "alice"}, "date": "…", "text": "…",
               "likes": 5, "location": "北京", "images": [], "reply_to": null, "reply_to_author": null,
               "replies": [{"id": "…", "author": {…}, "date": "…", "text": "…", "likes": 1,
                            "location": null, "reply_to": "…", "reply_to_author": "alice",
                            "replies": [], "replies_total": 0}],
               "replies_total": 2}],
 "comments_complete": true, "matched": ["打工"], "captured_at": "2026-09-27T10:00:00.000Z"}
```

Images and videos are addresses only; nothing is downloaded.
`comments_complete` is false when a cap cut the comments. In the store
(`crates/cuttlefish/src/rednote.rs`) each note is one document titled by
the note, with a header (`Xiaohongshu note by <creator>, <date>`), the
text, the tags, and a `## Comments` section of timestamped lines with the
replies indented (`[2024-08-30 12:13 UTC] bob ↳ alice: …`); the creator as
attribution, the note's language, and the game era from its date; weight
1.0 like a Discord channel, below #vod-review.

### Tests

```bash
node --test tools/capture/test/     # both tools: rednote.test.mjs (the site's answers, the page state, links, markers, the record), rednote-crawl.test.mjs (the visit against a scripted page)
```

No test touches the site or a login; the fixtures are synthetic, shaped
like the site's answers.
