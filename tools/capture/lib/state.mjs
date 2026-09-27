// What a capture knows between runs: `state.json` in its inbox folder
// (`<inbox>/x/`, `<inbox>/rednote/`), with the day's action count, the
// following list, a cursor per account and every post id looked at (kept,
// or why not). Written whole (temporary file, then rename), as the
// knowledge folder may be a synced folder. `tool` names whose file it is
// (xcap's by default), so the inbox skips it and another tool's is refused.

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Written into the state, so the inbox knows the file is ours */
export const TOOL = "xcap";
export const STATE_FILE = "state.json";

/** A fresh state of `tool` */
export function fresh(tool = TOOL) {
  return {
    tool,
    version: 1,
    /** The day (UTC) `actions` counts, and the count */
    day: { day: "", actions: 0 },
    /** Whose following list this is, and when it was read */
    following: { me: null, handles: [], at: null },
    /** By handle: the newest post id seen on the profile, when it was last
     * visited, and counts */
    accounts: {},
    /** By post id: `kept`, `off-topic`, `retweet` or `reply` */
    seen: {},
  };
}

/** The state of `tool` in `dir`, or a fresh one */
export function load(dir, tool = TOOL) {
  try {
    const text = readFileSync(join(dir, STATE_FILE), "utf8");
    const state = JSON.parse(text);
    if (state.tool !== tool) throw new Error(`${STATE_FILE} is not ${tool}'s`);
    return { ...fresh(tool), ...state };
  } catch (error) {
    if (error.code === "ENOENT") return fresh(tool);
    throw error;
  }
}

/** Writes `state` into `dir` whole */
export function save(dir, state) {
  mkdirSync(dir, { recursive: true });
  const path = join(dir, STATE_FILE);
  const part = `${path}.part`;
  writeFileSync(part, JSON.stringify(state, null, 2) + "\n");
  renameSync(part, path);
}

/** Writes only the day's action count (`{day, actions}`) into the state
 * of `tool` in `dir`, over what the file holds: a dry run's page actions
 * count against the daily cap, but nothing it looked at is remembered */
export function saveDay(dir, tool, day) {
  const kept = load(dir, tool);
  kept.day = { ...day };
  save(dir, kept);
}

/** The account's record, made when first seen */
export function account(state, handle) {
  state.accounts[handle] ??= {
    newest_id: null,
    visited_at: null,
    kept: 0,
    off_topic: 0,
  };
  return state.accounts[handle];
}
