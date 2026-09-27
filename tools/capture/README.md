# xcap: Salmon Run posts from X, slowly, with your own browser

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

# 2. A dry run: browse a little, print what would be kept, write nothing
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
