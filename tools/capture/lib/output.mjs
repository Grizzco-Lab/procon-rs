// The captured threads, one JSON line each, appended to
// `<inbox>/x/<handle>/posts.jsonl`, which `cuttlefish ingest inbox` reads.

import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

export const POSTS_FILE = "posts.jsonl";

/** A handle as a folder name: X handles are `[A-Za-z0-9_]`, anything
 * else is refused rather than escaped */
export function folderOf(handle) {
  if (!/^[A-Za-z0-9_]{1,15}$/.test(handle ?? ""))
    throw new Error(`not an X handle: ${handle}`);
  return handle;
}

/** One thread as a record of the file: the post with its replies */
export function record(post, replies, matched, capturedAt = new Date()) {
  const reply = (r) => ({
    id: r.id,
    url: r.url,
    author: r.author,
    date: r.date,
    text: r.text,
    lang: r.lang,
    urls: r.urls,
    media: r.media,
    reply_to: r.reply_to,
  });
  return {
    source: "x",
    id: post.id,
    url: post.url,
    author: post.author,
    date: post.date,
    text: post.text,
    lang: post.lang,
    urls: post.urls,
    media: post.media,
    quoted: post.quoted,
    reply_to: post.reply_to,
    replies: replies.map(reply),
    matched,
    captured_at: capturedAt.toISOString(),
  };
}

/** Appends `records` to the account's file under `dir` (the `x` folder of
 * the inbox); returns the file's path */
export function append(dir, handle, records) {
  const folder = join(dir, folderOf(handle));
  mkdirSync(folder, { recursive: true });
  const path = join(folder, POSTS_FILE);
  appendFileSync(path, records.map((r) => JSON.stringify(r) + "\n").join(""));
  return path;
}
