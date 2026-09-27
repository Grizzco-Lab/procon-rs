#!/usr/bin/env node
// rncap: a slow, browser-driven capture of the Salmon Run notes (笔记) of
// the creators one follows on Xiaohongshu (RedNote), with their comments
// and replies, into Cuttlefish's inbox
// (`<knowledge>/inbox/rednote/<user id>/notes.jsonl`), for `cuttlefish
// ingest inbox`. It drives a real, logged-in Chrome over the DevTools
// protocol on the capture tools' shared profile folder, reads the JSON the
// page loads for itself, and does nothing else: no private API calls, no
// signing (x-s, x-t), no writes.
//
//   node tools/capture/rednote.mjs login             # once: log in, in the window that opens
//   node tools/capture/rednote.mjs run --dry-run --max-actions 20
//   node tools/capture/rednote.mjs run
//   node tools/capture/rednote.mjs status
//
// Automating one's own account can breach Xiaohongshu's terms (see the
// README). Credentials are never seen, stored or logged by this tool; the
// browser profile holds the session.

import { homedir } from "node:os";
import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import * as cdp from "./lib/cdp.mjs";
import { knowledgeFolder } from "./lib/config.mjs";
import { Pace, estimateSeconds, parseRange } from "./lib/pace.mjs";
import * as rn from "./lib/rednote.mjs";
import { Crawl, DEFAULTS as CRAWL } from "./lib/rednote-crawl.mjs";
import * as state from "./lib/state.mjs";

/** The profile and port xcap uses too: one login for both sites */
const DEFAULT_PROFILE = join(homedir(), ".config", "procon", "browser-profile");
const DEFAULT_PORT = 9251;

/** The pace of a person reading, on a site with strict risk control: 8 to
 * 20 s between actions, a pause of 2 to 8 minutes every 10 to 25, at most
 * 150 actions a run and 450 a day */
const PACE = Object.freeze({
  delay: [8, 20],
  pauseEvery: [10, 25],
  pause: [120, 480],
  maxActions: 150,
  dailyCap: 450,
  maxMinutes: null,
});

const USAGE = `usage: rednote.mjs <login | run | status> [options]

  login                    open Chrome on the capture's profile for you to log in, then close it
  run                      visit the following list, the creators' notes, the Salmon Run notes' comments
  status                   what the state file says

Where (run, status):
  --config <path>          the studio's config.toml (default ./config.toml); the knowledge folder is found as the cuttlefish CLI finds it
  --data <folder>          the knowledge folder itself

Browser:
  --profile <folder>       Chrome profile folder (default ${DEFAULT_PROFILE}, shared with xcap)
  --port <n>               DevTools port (default ${DEFAULT_PORT})
  --chrome <binary>        Chrome to start (default $CHROME or google-chrome-stable)
  --attach                 do not start Chrome: attach to the one listening on --port
  --keep-open              leave Chrome running when done
  --headless               no window (login and challenges need one; the site tells headless browsers apart more easily)

What to read (run):
  --me <user id>           your account's id, when the page does not tell
  --site <origin>          where the account logs in (default ${rn.SITE}; https://www.rednote.com outside China)
  --creators <a,b,...>     only these creators (profile links or ids; @<file> with one per line), instead of the following list
  --refresh-following      read the following list again now
  --match <title|detail>   what decides that a note is about Salmon Run: its title in the list (default), or the whole note (every new note is opened)
  --max-notes <n>          notes kept this run (default ${CRAWL.maxNotes})
  --max-comments <n>       comments (with replies) loaded per note (default ${CRAWL.maxComments})
  --max-replies <n>        reply threads unfolded per note (default ${CRAWL.maxReplies})
  --list-scrolls <n>       scrolls down a creator's list, at most (default ${CRAWL.listScrolls})
  --dry-run                browse and print; write nothing

Pace (run):
  --delay <s-s>            seconds between page actions (default ${PACE.delay.join("-")})
  --pause-every <n-n>      page actions between longer pauses (default ${PACE.pauseEvery.join("-")})
  --pause <s-s>            seconds of a longer pause (default ${PACE.pause.join("-")})
  --max-actions <n>        page actions this run (default ${PACE.maxActions})
  --daily-cap <n>          page actions per day, all runs (default ${PACE.dailyCap})
  --max-minutes <n>        end the run after this long; the next continues
`;

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    config: { type: "string" },
    data: { type: "string" },
    profile: { type: "string", default: DEFAULT_PROFILE },
    port: { type: "string", default: String(DEFAULT_PORT) },
    chrome: {
      type: "string",
      default: process.env.CHROME ?? "google-chrome-stable",
    },
    attach: { type: "boolean", default: false },
    "keep-open": { type: "boolean", default: false },
    headless: { type: "boolean", default: false },
    me: { type: "string" },
    site: { type: "string", default: rn.SITE },
    creators: { type: "string", multiple: true },
    "refresh-following": { type: "boolean", default: false },
    match: { type: "string", default: "title" },
    "max-notes": { type: "string" },
    "max-comments": { type: "string" },
    "max-replies": { type: "string" },
    "list-scrolls": { type: "string" },
    "dry-run": { type: "boolean", default: false },
    delay: { type: "string" },
    "pause-every": { type: "string" },
    pause: { type: "string" },
    "max-actions": { type: "string" },
    "daily-cap": { type: "string" },
    "max-minutes": { type: "string" },
    help: { type: "boolean", short: "h", default: false },
  },
});

const command = positionals[0];
if (o.help || !["login", "run", "status"].includes(command)) {
  console.error(USAGE);
  process.exit(command ? 2 : 0);
}
if (!["title", "detail"].includes(o.match)) {
  console.error("--match: title or detail");
  process.exit(2);
}

/** A whole number option, or its default */
const integer = (name, fallback) => {
  if (o[name] == null) return fallback;
  const n = Number(o[name]);
  if (!Number.isInteger(n) || n < 0) {
    console.error(`--${name}: expected a whole number`);
    process.exit(2);
  }
  return n;
};
const port = integer("port", DEFAULT_PORT);
const log = (line) =>
  console.log(`${new Date().toISOString().slice(11, 19)} ${line}`);

/** The creators of `--creators`: ids, links, or `@<file>` with one per
 * line (`#` comments) */
function creatorsOf(values) {
  const out = [];
  for (const v of values ?? []) {
    if (v.startsWith("@")) {
      const text = readFileSync(v.slice(1), "utf8");
      for (const line of text.split("\n")) {
        const t = line.split("#")[0].trim();
        if (t) out.push(rn.creatorId(t));
      }
    } else
      for (const part of v.split(","))
        if (part.trim()) out.push(rn.creatorId(part));
  }
  return [...new Set(out)];
}

// -------------------------------------------------------------- the browser

/** Chrome on the profile: started, or the one listening on the port */
async function browser() {
  const profile = resolve(o.profile);
  if (o.attach) {
    const version = await cdp.probe(port);
    if (!version) {
      console.error(
        `nothing listens on port ${port}; start Chrome with --remote-debugging-port=${port} --user-data-dir=${profile}, or drop --attach`,
      );
      process.exit(2);
    }
    log(`attached to ${version.Browser} on port ${port}`);
    return { chrome: null, stop: async () => {} };
  }
  if (await cdp.probe(port)) {
    console.error(
      `port ${port} is taken: something listens there already (another capture? give --attach to use it, or --port for another)`,
    );
    process.exit(2);
  }
  const chrome = await cdp.launch({
    binary: o.chrome,
    profile,
    port,
    headless: o.headless,
  });
  log(`Chrome started (pid ${chrome.pid}) on ${profile}`);
  return {
    chrome,
    stop: async () => {
      if (o["keep-open"]) log(`Chrome left running (pid ${chrome.pid})`);
      else await cdp.stop(chrome);
    },
  };
}

const openPage = () => cdp.openPage(port, { keep: rn.isSiteApi });

/** Whether the page shows a logged-in account: the site's `user/me`
 * answer among those drained, else the page state; undefined when
 * nothing tells */
/** The `--site` origin, checked to be one of the site's */
function site() {
  const origin = rn.originOf(o.site);
  if (!origin) {
    console.error(`--site ${o.site}: not an address of the site`);
    process.exit(2);
  }
  return origin;
}

let shownFields = false;
async function loggedIn(page) {
  let known;
  for (const r of page.drain()) {
    const p = rn.recognise(r.url, r.json);
    if (p?.kind === "me") {
      known = !p.guest;
      if (!shownFields) log(`user/me holds: ${p.fields.join(", ")}`);
      shownFields = true;
    }
  }
  if (known !== undefined) return known;
  const st = await page.evaluate(rn.JS_STATE).catch(() => null);
  const name = rn.field(st?.user?.userInfo, ["nickname", "redId", "red_id"]);
  if (typeof name === "string" && name) return true;
  if (typeof st?.user?.loggedIn === "boolean") return st.user.loggedIn;
  return undefined;
}

// ------------------------------------------------------------------- login

async function login() {
  if (o.headless) {
    console.error("login needs a window: drop --headless");
    process.exit(2);
  }
  const b = await browser();
  const page = await openPage();
  try {
    await page.goto(site());
    await page.waitFor((r) => /user\/me/.test(r.url), 6000);
    let ok = await loggedIn(page);
    if (ok) log("already logged in");
    else {
      log(
        "log in to Xiaohongshu in the window that opened (the site offers a code to scan with the app); this waits up to 20 minutes and never reads what you type or scan",
      );
      const end = Date.now() + 20 * 60_000;
      while (!ok && Date.now() < end) {
        await new Promise((r) => setTimeout(r, 3000));
        ok = await loggedIn(page).catch(() => undefined);
      }
    }
    if (!ok) {
      console.error("no login seen; run login again");
      process.exitCode = 1;
    } else if (b.chrome) {
      // The window is the user's now: Chrome saves the profile as it closes
      log(
        `logged in; the session stays in ${resolve(o.profile)}. Close the window when you are done`,
      );
      await b.chrome.exited;
    } else log("logged in");
  } finally {
    await page.close().catch(() => {});
    await b.stop();
  }
}

// --------------------------------------------------------------------- run

async function run() {
  const knowledge = knowledgeFolder({ data: o.data, config: o.config });
  const dir = join(knowledge, "inbox", "rednote");
  const st = state.load(dir, rn.TOOL);
  const paceOptions = {
    delay: o.delay ? parseRange(o.delay) : PACE.delay,
    pauseEvery: o["pause-every"]
      ? parseRange(o["pause-every"])
      : PACE.pauseEvery,
    pause: o.pause ? parseRange(o.pause) : PACE.pause,
    maxActions: integer("max-actions", PACE.maxActions) || null,
    dailyCap: integer("daily-cap", PACE.dailyCap) || null,
    maxMinutes: o["max-minutes"] == null ? null : Number(o["max-minutes"]),
  };
  const options = {
    site: site(),
    me: o.me,
    creators: creatorsOf(o.creators),
    refreshFollowing: o["refresh-following"],
    match: o.match,
    maxNotes: integer("max-notes", CRAWL.maxNotes),
    maxComments: integer("max-comments", CRAWL.maxComments),
    maxReplies: integer("max-replies", CRAWL.maxReplies),
    listScrolls: integer("list-scrolls", CRAWL.listScrolls),
    dryRun: o["dry-run"],
  };
  // Ctrl+C: the action under way finishes, the state is saved
  let interrupted = false;
  const sleep = (ms) =>
    new Promise((resolve) => {
      const end = Date.now() + ms;
      const tick = () => {
        if (interrupted) return resolve(false);
        if (Date.now() >= end) return resolve(true);
        setTimeout(tick, Math.min(200, end - Date.now()));
      };
      tick();
    });
  const pace = new Pace(paceOptions, { sleep, onWait: log });
  process.on("SIGINT", () => {
    if (interrupted) process.exit(130);
    interrupted = true;
    pace.interrupted = true;
    log("stopping after the action under way (Ctrl+C again to quit at once)");
  });

  log(
    `knowledge folder ${knowledge}; notes go to ${dir}/<user id>/notes.jsonl${options.dryRun ? " (dry run: nothing is written)" : ""}`,
  );
  log(
    `pace: ${paceOptions.delay.join("-")} s between actions, a ${paceOptions.pause.join("-")} s pause every ${paceOptions.pauseEvery.join("-")}; at most ${paceOptions.maxActions ?? "any"} actions this run, ${paceOptions.dailyCap ?? "any"} today (${st.day.actions} used${st.day.day ? ` on ${st.day.day}` : ""}); about ${(estimateSeconds(6, paceOptions) / 60).toFixed(1)} min a note, ${(estimateSeconds(600, paceOptions) / 3600).toFixed(1)} h per 100 notes`,
  );
  log(
    `creators: ${options.creators.length ? `${options.creators.length} given` : "the accounts you follow"}; a note is about Salmon Run by its ${options.match === "title" ? "title" : "whole text"}; up to ${options.maxNotes} notes this run, ${options.maxComments} comments and ${options.maxReplies} reply threads a note`,
  );
  const b = await browser();
  const page = await openPage();
  let summary;
  try {
    const crawl = new Crawl(page, { pace, state: st, dir, log, options });
    summary = await crawl.run();
  } finally {
    await page.close().catch(() => {});
    await b.stop();
  }
  const s = summary;
  log(
    `${options.dryRun ? "would have kept" : "kept"} ${s.kept} notes (${s.comments} comments) of ${s.creators} creators; ${s.listed} notes listed, ${s.fresh} new, ${s.offTopic} not about Salmon Run, ${s.failed} unreadable; ${pace.actions} page actions this run, ${st.day.actions} today`,
  );
  for (const f of s.files) log(`  ${f}`);
  if (s.stopped) log(`stopped: ${s.stopped.message}`);
  if (s.kept && !options.dryRun)
    log(
      "next: cuttlefish ingest inbox (or Import inbox on the Knowledge page)",
    );
  if (s.stopped && ["login", "blocked", "challenge"].includes(s.stopped.reason))
    process.exitCode = 1;
}

// ------------------------------------------------------------------ status

function status() {
  const knowledge = knowledgeFolder({ data: o.data, config: o.config });
  const dir = join(knowledge, "inbox", "rednote");
  const st = state.load(dir, rn.TOOL);
  console.log(
    `state: ${join(dir, state.STATE_FILE)}${existsSync(join(dir, state.STATE_FILE)) ? "" : " (none yet)"}`,
  );
  console.log(
    `page actions today: ${st.day.actions}${st.day.day ? ` (${st.day.day})` : ""}`,
  );
  const f = st.following;
  console.log(
    f.handles.length
      ? `following list: ${f.handles.length} creators of ${f.me}, read ${f.at?.slice(0, 10)}`
      : "following list: not read yet",
  );
  const seen = Object.values(st.seen);
  const n = (kind) => seen.filter((s) => s === kind).length;
  console.log(
    `notes looked at: ${seen.length} (${n("kept")} kept, ${n("off-topic")} not about Salmon Run, ${n("failed")} unreadable)`,
  );
  for (const [id, a] of Object.entries(st.accounts).sort(([x], [y]) =>
    x.localeCompare(y),
  )) {
    let size = "";
    try {
      size = ` ${(statSync(join(dir, id, rn.NOTES_FILE)).size / 1024).toFixed(0)} KB`;
    } catch {
      // No file yet
    }
    console.log(
      `  ${id} ${(a.nickname ?? "").padEnd(20)} ${String(a.kept).padStart(4)} kept ${String(a.off_topic).padStart(4)} off topic  visited ${a.visited_at?.slice(0, 16).replace("T", " ") ?? "never"}${a.listed_to_end ? "" : " (list not finished)"}${size}`,
    );
  }
  if (existsSync(dir)) {
    const orphans = readdirSync(dir, { withFileTypes: true }).filter(
      (d) => d.isDirectory() && !st.accounts[d.name],
    );
    if (orphans.length)
      console.log(
        `folders without a record: ${orphans.map((d) => d.name).join(", ")}`,
      );
  }
}

try {
  if (command === "login") await login();
  else if (command === "run") await run();
  else status();
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
