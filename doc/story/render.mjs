#!/usr/bin/env node
// Renders the story's scenes (doc/story.html) to MP4 files next to this
// script: the page draws every frame with its own canvas code
// (window.storyRender), headless Chrome runs it (DevTools protocol over a
// WebSocket, Node 22+, no packages, as scripts/layout-check.mjs does), and
// ffmpeg encodes the PNG frames with libx264 on the CPU, niced.
//
// Usage:
//   node doc/story/render.mjs                     every scene, English and Chinese
//   node doc/story/render.mjs mash futures        some scenes
//   node doc/story/render.mjs mash --lang zh      one language
//   node doc/story/render.mjs mash --at 2,9.5     PNG stills at those seconds
//                                                 (into doc/story/stills/)
//   --crf 28        x264 quality (lower is better and bigger)
//   --page <file>   another page with window.storyRender
//   --out <dir>     where videos (or stills) go instead
// Output: doc/story/<scene>-<lang>.mp4 (1280x720, 30 fps, no sound).
import { spawn } from "node:child_process";
import {
  mkdirSync,
  mkdtempSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
/** The value after `--name`, if given */
const option = (name) => {
  const at = args.indexOf(`--${name}`);
  return at >= 0 ? args[at + 1] : undefined;
};
const valued = new Set(["--lang", "--at", "--crf", "--page", "--out"]);
const wanted = args.filter(
  (a, i) => !a.startsWith("--") && !valued.has(args[i - 1]),
);
const langs = (option("lang") ?? "en,zh").split(",");
const stills = option("at")?.split(",").map(Number);
const crf = option("crf") ?? "28";
const page = resolve(option("page") ?? join(here, "..", "story.html"));
const outDir = resolve(option("out") ?? (stills ? join(here, "stills") : here));

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

// ------------------------------------------------------------------ Chrome

const profile = mkdtempSync(join(tmpdir(), "procon-story-"));
const port = 9900 + Math.floor(Math.random() * 90);
const chrome = spawn(
  process.env.CHROME ?? "google-chrome-stable",
  [
    "--headless=new",
    "--disable-gpu",
    "--hide-scrollbars",
    "--mute-audio",
    "--allow-file-access-from-files",
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    "about:blank",
  ],
  { stdio: "ignore" },
);
const chromeExited = new Promise((done) => chrome.on("exit", done));

/** Stop Chrome, remove its profile, exit with `code` */
async function finish(code) {
  chrome.kill();
  await Promise.race([chromeExited, sleep(5000)]);
  rmSync(profile, { recursive: true, force: true });
  process.exit(code);
}
for (const event of ["uncaughtException", "unhandledRejection"])
  process.on(event, (error) => {
    console.error(error);
    finish(2);
  });

let targets = [];
for (let i = 0; i < 100 && !targets.length; i++) {
  try {
    const list = await fetch(`http://127.0.0.1:${port}/json/list`);
    targets = (await list.json()).filter((t) => t.type === "page");
  } catch {
    // Not listening yet
  }
  await sleep(100);
}
if (!targets.length) {
  console.error("Chrome did not start (set CHROME to its binary)");
  await finish(2);
}
const ws = new WebSocket(targets[0].webSocketDebuggerUrl);
await new Promise((done, fail) => {
  ws.onopen = done;
  ws.onerror = fail;
});
let nextId = 0;
const pending = new Map();
ws.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.id && pending.has(message.id)) {
    pending.get(message.id)(message);
    pending.delete(message.id);
  }
};
/** One DevTools command */
const send = (method, params = {}) =>
  new Promise((done) => {
    const id = ++nextId;
    pending.set(id, done);
    ws.send(JSON.stringify({ id, method, params }));
  });
/** The value of `expression` in the page (awaited if a promise) */
async function evaluate(expression) {
  const reply = await send("Runtime.evaluate", {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (reply.result?.exceptionDetails)
    throw new Error(reply.result.exceptionDetails.exception?.description);
  return reply.result?.result?.value;
}

// ------------------------------------------------------------------ render

await send("Page.enable");
await send("Page.navigate", { url: `${pathToFileURL(page)}?render` });
for (let i = 0; i < 100; i++) {
  if (await evaluate("!!window.storyRender").catch(() => false)) break;
  await sleep(100);
}
const fonts = await evaluate("storyRender.ready()");
if (!fonts.every((face) => face.loaded))
  console.warn(
    `fonts not loaded (drawn with fallbacks): ${JSON.stringify(fonts)}`,
  );
const scenes = wanted.length ? wanted : await evaluate("storyRender.scenes");

/** A PNG of scene `id` in `lang` at frame `i`, as bytes */
async function frame(id, lang, i) {
  const url = await evaluate(
    `storyRender.frame(${JSON.stringify(id)}, "${lang}", ${i})`,
  );
  return Buffer.from(url.slice(url.indexOf(",") + 1), "base64");
}

for (const id of scenes) {
  const info = await evaluate(`storyRender.info(${JSON.stringify(id)})`);
  if (!info) {
    console.error(`no scene "${id}"`);
    await finish(1);
  }
  for (const lang of langs) {
    mkdirSync(outDir, { recursive: true });
    if (stills) {
      for (const s of stills) {
        const file = join(outDir, `${id}-${lang}-${s}.png`);
        writeFileSync(file, await frame(id, lang, Math.round(s * info.fps)));
        console.log(file);
      }
      continue;
    }
    const file = join(outDir, `${id}-${lang}.mp4`);
    const ffmpeg = spawn(
      "nice",
      [
        "-n",
        "15",
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "image2pipe",
        "-framerate",
        String(info.fps),
        "-c:v",
        "png",
        "-i",
        "-",
        "-c:v",
        "libx264",
        "-preset",
        "veryslow",
        "-tune",
        "animation",
        "-crf",
        crf,
        "-pix_fmt",
        "yuv420p",
        "-movflags",
        "+faststart",
        "-an",
        file,
      ],
      { stdio: ["pipe", "inherit", "inherit"] },
    );
    const encoded = new Promise((done) => ffmpeg.on("exit", done));
    const started = Date.now();
    for (let i = 0; i < info.frames; i++) {
      const png = await frame(id, lang, i);
      if (!ffmpeg.stdin.write(png))
        await new Promise((done) => ffmpeg.stdin.once("drain", done));
    }
    ffmpeg.stdin.end();
    if ((await encoded) !== 0) {
      console.error(`ffmpeg failed on ${file}`);
      await finish(1);
    }
    const kb = Math.round(statSync(file).size / 1024);
    console.log(
      `${file}: ${info.frames} frames, ${kb} KB, ${((Date.now() - started) / 1000).toFixed(0)} s`,
    );
  }
}
ws.close();
await finish(0);
