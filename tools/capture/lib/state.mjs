// What the capture knows between runs: `state.json` in `<inbox>/x/`, with
// the day's action count, the following list, a cursor per account and
// every post id looked at (kept, or why not). Written whole (temporary
// file, then rename), as the knowledge folder may be a synced folder.

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** Written into the state, so the inbox knows the file is ours */
export const TOOL = "xcap";
export const STATE_FILE = "state.json";

/** A fresh state */
export function fresh() {
  return {
    tool: TOOL,
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

/** The state in `dir`, or a fresh one */
export function load(dir) {
  try {
    const text = readFileSync(join(dir, STATE_FILE), "utf8");
    const state = JSON.parse(text);
    if (state.tool !== TOOL) throw new Error(`${STATE_FILE} is not ${TOOL}'s`);
    return { ...fresh(), ...state };
  } catch (error) {
    if (error.code === "ENOENT") return fresh();
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
