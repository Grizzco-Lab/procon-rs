// Whether a note's cover looks like Splatoon, before the note is opened:
// the tile's thumbnail, fetched from the site's image CDN as the browser
// does for every tile it draws (a plain GET, no cookies, never stored),
// scored by SigLIP 2 zero-shot in a helper process (`cover.py`, in
// AgentZero's environment, on the CPU) against prompts for the game's
// screenshots and art versus everyday photos.

import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

/** The helper script, beside this file */
export const HELPER = fileURLToPath(new URL("./cover.py", import.meta.url));

/** How the helper is run: in AgentZero's folder, on its environment */
export const COMMAND = ["uv", "run", "python", HELPER];

/** A cover scored at least this is Splatoon's: the model's own
 * probability that the picture matches one of the Splatoon prompts
 * (small in absolute terms, see `cover.py`); chosen on the captured
 * notes' covers, where game pictures score 0.07 and up and the rest 0.03
 * and down, one photo apart */
export const THRESHOLD = 0.05;

/** Seconds the helper may take to load the model */
const READY_TIMEOUT_S = 180;

/** The browser's own kind of request for a picture */
const HEADERS = Object.freeze({
  "User-Agent":
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36",
  Accept: "image/webp,image/*,*/*;q=0.8",
});

/** The file id in one of the site's image addresses: the path after the
 * signed prefix (`/<time>/<hash>/`, which expires within hours; the file
 * id does not), or the whole path on the unsigned hosts, without a
 * `!style` suffix; it may hold a folder (`notes_pre_post/1040g…`). Null
 * when the address holds no id. */
export function fileIdOf(url) {
  if (typeof url !== "string") return null;
  let path;
  try {
    path = new URL(url).pathname;
  } catch {
    return null;
  }
  path = path.replace(/^\/+/, "").split("!")[0];
  const signed = /^\d{8,}\/[0-9a-f]{32}\/(.+)$/.exec(path);
  const id = signed ? signed[1] : path;
  return /[0-9a-z]{20,}/i.test(id) ? id : null;
}

/** A 360-pixel-wide thumbnail of the picture by file id, from the site's
 * image service (unsigned, so it works long after the listed address) */
export const thumbnailUrl = (fileId) =>
  `https://ci.xiaohongshu.com/${fileId}?imageView2/2/w/360/format/webp`;

/** The thumbnail of `url`: the address as listed, then the unsigned one
 * by its file id; null when neither answers a picture */
export async function fetchImage(
  url,
  { fetch = globalThis.fetch, timeoutMs = 15_000 } = {},
) {
  const id = fileIdOf(url);
  const candidates = [url, id && thumbnailUrl(id)].filter(
    (u, i, all) => u && all.indexOf(u) === i,
  );
  for (const candidate of candidates) {
    try {
      const r = await fetch(candidate, {
        headers: HEADERS,
        signal: AbortSignal.timeout(timeoutMs),
      });
      if (!r.ok || !/^image\//.test(r.headers.get("content-type") ?? "")) {
        continue;
      }
      return Buffer.from(await r.arrayBuffer());
    } catch {
      continue;
    }
  }
  return null;
}

/** The helper process: one picture at a time, a JSON line each way */
export class CoverCheck {
  /**
   * @param {object} options `command` (default `COMMAND`), `cwd` (AgentZero's
   *   folder), `log(line)`, `spawn` (for tests)
   */
  constructor({ command = COMMAND, cwd, log = () => {}, spawn: run = spawn }) {
    this.command = command;
    this.cwd = cwd;
    this.log = log;
    this.spawn = run;
    this.child = null;
    /** Answers awaited, by request id */
    this.pending = new Map();
    this.next = 0;
    this.ready = null;
  }

  /** Starts the helper and waits for the model; rejects when it cannot */
  start() {
    if (this.ready) return this.ready;
    const [file, ...args] = this.command;
    const child = this.spawn(file, args, {
      cwd: this.cwd,
      env: { ...process.env, CUDA_VISIBLE_DEVICES: "" },
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child = child;
    child.stderr.on("data", (d) => {
      for (const line of String(d).split("\n"))
        if (/error|traceback/i.test(line)) this.log(`cover check: ${line}`);
    });
    this.ready = new Promise((resolve, reject) => {
      const timer = setTimeout(
        () =>
          reject(
            new Error(`the cover check did not load in ${READY_TIMEOUT_S} s`),
          ),
        READY_TIMEOUT_S * 1000,
      );
      const fail = (why) => {
        clearTimeout(timer);
        for (const p of this.pending.values()) p.reject(new Error(why));
        this.pending.clear();
        this.child = null;
        reject(new Error(why));
      };
      child.on("error", (e) =>
        fail(`the cover check could not start: ${e.message}`),
      );
      child.on("exit", (code, signal) =>
        fail(`the cover check ended (${signal ?? `code ${code}`})`),
      );
      createInterface({ input: child.stdout }).on("line", (line) => {
        let answer;
        try {
          answer = JSON.parse(line);
        } catch {
          return;
        }
        if (answer.ready) {
          clearTimeout(timer);
          this.log(
            `cover check: ${answer.model ?? "model"} on the ${answer.device ?? "cpu"}`,
          );
          resolve();
          return;
        }
        const p = this.pending.get(answer.id);
        if (!p) return;
        this.pending.delete(answer.id);
        if (answer.error) p.reject(new Error(answer.error));
        else p.resolve(answer.p);
      });
    });
    return this.ready;
  }

  /** The share (0 to 1) of `image` (a Buffer) that is Splatoon's */
  async score(image) {
    await this.start();
    if (!this.child) throw new Error("the cover check is not running");
    const id = String(++this.next);
    const answer = new Promise((resolve, reject) =>
      this.pending.set(id, { resolve, reject }),
    );
    this.child.stdin.write(
      JSON.stringify({ id, image: image.toString("base64") }) + "\n",
    );
    return answer;
  }

  /** Ends the helper */
  stop() {
    const child = this.child;
    this.child = null;
    if (!child) return;
    child.stdin.end();
    child.kill("SIGTERM");
  }
}

/** A cover judge for a crawl: fetches the thumbnail and scores it; null
 * when the picture could not be fetched or the helper failed on it (the
 * note stays unjudged), never a throw */
export function judge(check, { fetch, log = () => {} } = {}) {
  return async (url) => {
    const image = await fetchImage(url, fetch ? { fetch } : {});
    if (!image) {
      log(`  cover ${url}: no picture`);
      return null;
    }
    try {
      return await check.score(image);
    } catch (error) {
      log(`  cover ${url}: ${error.message}`);
      return null;
    }
  };
}
