#!/usr/bin/env node
// xcap: a slow, browser-driven capture of the Salmon Run posts of the
// accounts one follows on X, with their replies, into Cuttlefish's inbox
// (`<knowledge>/inbox/x/<handle>/posts.jsonl`), for `cuttlefish ingest
// inbox`. It drives a real, logged-in Chrome over the DevTools protocol on
// a profile folder of its own, reads the JSON the page loads for itself,
// and does nothing else: no private API calls, no writes, no signing.
//
//   node tools/capture/xcap.mjs login             # once: log in, in the window that opens
//   node tools/capture/xcap.mjs run --dry-run --max-actions 20
//   node tools/capture/xcap.mjs run
//   node tools/capture/xcap.mjs status
//
// Automating one's own account can breach X's terms (see the README).
// Credentials are never seen, stored or logged by this tool; the browser
// profile holds the session.

import { homedir } from "node:os";
import { existsSync, readdirSync, statSync } from "node:fs";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import * as cdp from "./lib/cdp.mjs";
import { knowledgeFolder } from "./lib/config.mjs";
import { Crawl } from "./lib/crawl.mjs";
import {
  DEFAULTS as PACE,
  Pace,
  estimateSeconds,
  parseRange,
} from "./lib/pace.mjs";
import * as state from "./lib/state.mjs";

const DEFAULT_PROFILE = join(homedir(), ".config", "procon", "browser-profile");
const DEFAULT_PORT = 9251;

const USAGE = `usage: xcap.mjs <login | run | status> [options]

  login                    open Chrome on the capture's profile for you to log in, then close it
  run                      visit the following list, the profiles, the Salmon Run posts' replies
  status                   what the state file says

Where (run, status):
  --config <path>          the studio's config.toml (default ./config.toml); the knowledge folder is found as the cuttlefish CLI finds it
  --data <folder>          the knowledge folder itself

Browser:
  --profile <folder>       Chrome profile folder (default ${DEFAULT_PROFILE})
  --port <n>               DevTools port (default ${DEFAULT_PORT})
  --chrome <binary>        Chrome to start (default $CHROME or google-chrome-stable)
  --attach                 do not start Chrome: attach to the one listening on --port
  --keep-open              leave Chrome running when done
  --headless               no window (login and challenges need one; not recommended)

What to read (run):
  --me <handle>            your handle, when the page does not tell
  --accounts <a,b,c>       only these accounts, instead of the following list
  --refresh-following      read the following list again now
  --since-days <n>         posts newer than this (default 90)
  --max-posts <n>          threads captured this run (default 60)
  --profile-scrolls <n>    scrolls down a profile, at most (default 8)
  --reply-scrolls <n>      scrolls down a post for more replies (default 1)
  --dry-run                browse and print; write nothing but the day's action count

Pace (run):
  --delay <s-s>            seconds between page actions (default 4-10)
  --pause-every <n-n>      page actions between longer pauses (default 15-30)
  --pause <s-s>            seconds of a longer pause (default 60-180)
  --max-actions <n>        page actions this run (default 400)
  --daily-cap <n>          page actions per day, all runs (default 800)
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
    accounts: { type: "string" },
    "refresh-following": { type: "boolean", default: false },
    "since-days": { type: "string" },
    "max-posts": { type: "string" },
    "profile-scrolls": { type: "string" },
    "reply-scrolls": { type: "string" },
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
      `port ${port} is taken: something listens there already (another xcap? give --attach to use it, or --port for another)`,
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

// ------------------------------------------------------------------- login

async function login() {
  if (o.headless) {
    console.error("login needs a window: drop --headless");
    process.exit(2);
  }
  const b = await browser();
  const page = await cdp.openPage(port);
  try {
    await page.goto("https://x.com/home");
    let handle = await page.signedIn();
    if (handle) {
      log(`already signed in as @${handle}`);
    } else {
      await page.goto("https://x.com/login");
      log(
        "log in to X in the window that opened; this waits (up to 20 minutes) and never reads what you type",
      );
      const end = Date.now() + 20 * 60_000;
      while (!handle && Date.now() < end) {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        handle = await page.signedIn().catch(() => null);
      }
      if (!handle) {
        console.error("no sign-in seen; run login again");
        process.exitCode = 1;
      } else
        log(
          `signed in as @${handle}; the session stays in ${resolve(o.profile)}`,
        );
    }
  } finally {
    await page.close().catch(() => {});
    await b.stop();
  }
}

// --------------------------------------------------------------------- run

async function run() {
  const knowledge = knowledgeFolder({ data: o.data, config: o.config });
  const dir = join(knowledge, "inbox", "x");
  const st = state.load(dir);
  if (o["refresh-following"])
    st.following = { me: null, handles: [], at: null };
  const paceOptions = {
    delay: o.delay ? parseRange(o.delay) : PACE.delay,
    pauseEvery: o["pause-every"]
      ? parseRange(o["pause-every"])
      : PACE.pauseEvery,
    pause: o.pause ? parseRange(o.pause) : PACE.pause,
    maxActions: integer("max-actions", PACE.maxActions),
    dailyCap: integer("daily-cap", PACE.dailyCap),
    maxMinutes: o["max-minutes"] == null ? null : Number(o["max-minutes"]),
  };
  const options = {
    me: o.me,
    accounts: o.accounts
      ?.split(",")
      .map((h) => h.trim().replace(/^@/, ""))
      .filter(Boolean),
    sinceDays: integer("since-days", 90),
    maxPosts: integer("max-posts", 60),
    profileScrolls: integer("profile-scrolls", 8),
    replyScrolls: integer("reply-scrolls", 1),
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
        setTimeout(tick, Math.max(0, Math.min(200, end - Date.now())));
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
    `knowledge folder ${knowledge}; posts go to ${dir}/<handle>/posts.jsonl${options.dryRun ? " (dry run: nothing is written but the day's action count)" : ""}`,
  );
  log(
    `pace: ${paceOptions.delay.join("-")} s between actions, a ${paceOptions.pause.join("-")} s pause every ${paceOptions.pauseEvery.join("-")}; at most ${paceOptions.maxActions} actions this run, ${paceOptions.dailyCap} today (${st.day.actions} used${st.day.day ? ` on ${st.day.day}` : ""}); about ${(estimateSeconds(paceOptions.maxActions, paceOptions) / 60).toFixed(0)} min if the run uses them all`,
  );
  const b = await browser();
  const page = await cdp.openPage(port);
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
    `${options.dryRun ? "would have kept" : "kept"} ${s.kept} threads (${s.replies} replies) of ${s.accounts} accounts, ${s.offTopic} posts off topic; ${pace.actions} page actions this run, ${st.day.actions} today`,
  );
  for (const f of s.files) log(`  ${f}`);
  if (s.stopped) log(`stopped: ${s.stopped.message}`);
  if (s.kept && !options.dryRun)
    log(
      "next: cuttlefish ingest inbox (or Import inbox on the Knowledge page)",
    );
  if (
    s.stopped &&
    ["login", "rate-limit", "forbidden"].includes(s.stopped.reason)
  )
    process.exitCode = 1;
}

// ------------------------------------------------------------------ status

function status() {
  const knowledge = knowledgeFolder({ data: o.data, config: o.config });
  const dir = join(knowledge, "inbox", "x");
  const st = state.load(dir);
  console.log(
    `state: ${join(dir, state.STATE_FILE)}${existsSync(join(dir, state.STATE_FILE)) ? "" : " (none yet)"}`,
  );
  console.log(
    `page actions today: ${st.day.actions}${st.day.day ? ` (${st.day.day})` : ""}`,
  );
  const f = st.following;
  console.log(
    f.handles.length
      ? `following list: ${f.handles.length} accounts of @${f.me}, read ${f.at?.slice(0, 10)}`
      : "following list: not read yet",
  );
  const seen = Object.values(st.seen);
  const count = (kind) => seen.filter((s) => s === kind).length;
  console.log(
    `posts looked at: ${seen.length} (${count("kept")} kept, ${count("off-topic")} off topic, ${count("retweet")} retweets, ${count("reply")} replies)`,
  );
  const accounts = Object.entries(st.accounts).sort(([a], [b]) =>
    a.localeCompare(b),
  );
  for (const [handle, a] of accounts) {
    let size = "";
    try {
      size = ` ${(statSync(join(dir, handle, "posts.jsonl")).size / 1024).toFixed(0)} KB`;
    } catch {
      // No file yet
    }
    console.log(
      `  @${handle.padEnd(16)} ${String(a.kept).padStart(4)} kept ${String(a.off_topic).padStart(4)} off topic  visited ${a.visited_at?.slice(0, 16).replace("T", " ") ?? "never"}${size}`,
    );
  }
  if (existsSync(dir)) {
    const folders = readdirSync(dir, { withFileTypes: true }).filter((d) =>
      d.isDirectory(),
    );
    const orphans = folders.filter((d) => !st.accounts[d.name]);
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
