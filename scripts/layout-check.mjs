#!/usr/bin/env node
// Layout regression check: loads a running lab in headless Chrome
// (DevTools protocol over a WebSocket, Node 22+, no packages) in every theme,
// in English and Chinese, at desktop width (apps in the top bar and in the
// rail) and phone width, and measures the boxes that broke before:
//
// - the Studio's 3D stage (or the SVG view without WebGL) fills its panel
// - the top bar is one row at 1440 px
// - the Cuttlefish chat bar sits at the bottom of the window
// - no app scrolls the page sideways
// - the guide (How it fits together) fits the window without sideways
//   scrolling, its steps in one row where it is wide
//
// Usage: node scripts/layout-check.mjs <lab url> [--only theme,...]
// e.g. node scripts/layout-check.mjs http://127.0.0.1:8073
// It only reads: the page is loaded, never clicked beyond the app links
// and the guide it opens.
// Exits 1 and lists every failure when a check fails.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const base = args.find((a) => !a.startsWith("--"))?.replace(/\/$/, "");
if (!base) {
  console.error("usage: node scripts/layout-check.mjs <lab url>");
  process.exit(2);
}
try {
  await fetch(`${base}/`);
} catch {
  console.error(`No lab at ${base}`);
  process.exit(2);
}
const onlyAt = args.indexOf("--only");
const THEMES = ["studio", "joy", "telemetry", "salmon"].filter(
  (theme) => onlyAt < 0 || args[onlyAt + 1].split(",").includes(theme),
);
const LANGS = ["en", "zh"];
/** Window sizes and app placement: the one-row top bar is checked at 1440 */
const VIEWPORTS = [
  { name: "desktop-top", width: 1440, height: 900, nav: "top" },
  { name: "desktop-rail", width: 1440, height: 900, nav: "side" },
  { name: "phone", width: 390, height: 844, nav: "side", mobile: true },
];
const APPS = [
  "studio",
  "inspect",
  "cuttlefish",
  "vision",
  "predictor",
  "pipeline",
];

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// ------------------------------------------------------------------ Chrome

const profile = mkdtempSync(join(tmpdir(), "procon-layout-"));
const port = 9400 + Math.floor(Math.random() * 500);
const chrome = spawn(
  process.env.CHROME ?? "google-chrome-stable",
  [
    "--headless=new",
    "--use-angle=swiftshader",
    "--enable-unsafe-swiftshader",
    "--hide-scrollbars",
    "--mute-audio",
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    "about:blank",
  ],
  { stdio: "ignore" },
);

const chromeExited = new Promise((resolve) => chrome.on("exit", resolve));

/** Stop Chrome, remove its profile once it has exited, and exit with `code` */
async function finish(code) {
  chrome.kill();
  await Promise.race([chromeExited, sleep(5000)]);
  rmSync(profile, { recursive: true, force: true });
  process.exit(code);
}
for (const event of ["uncaughtException", "unhandledRejection"]) {
  process.on(event, (error) => {
    console.error(error);
    finish(2);
  });
}

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
await new Promise((resolve, reject) => {
  ws.onopen = resolve;
  ws.onerror = reject;
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
  new Promise((resolve) => {
    const id = ++nextId;
    pending.set(id, resolve);
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

// ---------------------------------------------------------------- measures

/** Runs in the page: the boxes of the open app, as plain numbers */
const MEASURE = `(() => {
  const box = (el) => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    const shown = r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden";
    return shown ? { x: r.left, y: r.top, w: r.width, h: r.height, b: r.bottom, r: r.right } : null;
  };
  const page = document.documentElement;
  const bar = document.querySelector(".topbar");
  const barBox = box(bar);
  // The bar's own row: its children that are inside it (not the rail)
  const rows = [...bar.children]
    .map((el) => [el.className, box(el)])
    .filter(([, b]) => b && barBox && b.y >= barBox.y - 1 && b.b <= barBox.b + 1)
    .map(([name, b]) => ({ name: String(name), top: b.y, bottom: b.b }));
  const canvas = document.getElementById("procon-3d");
  const panelEl = document.querySelector(".p-controller");
  const panel = box(panelEl);
  if (panel) {
    const style = getComputedStyle(panelEl);
    panel.w -= parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
  }
  return {
    viewport: { w: innerWidth, h: innerHeight },
    scrollWidth: page.scrollWidth,
    bar: barBox,
    rows,
    panel,
    stage: box(document.querySelector(".p-controller .stage")),
    view: box(canvas && !canvas.hidden ? canvas : document.getElementById("procon")),
    threeD: !!(canvas && !canvas.hidden),
    chat: box(document.querySelector("#cf-library .p-cf-entry")),
  };
})()`;

/** Runs in the page: opens the guide and measures it, then closes it */
const GUIDE = `(() => {
  const guide = document.getElementById("guide");
  guide.showPopover();
  const r = guide.getBoundingClientRect();
  const tops = new Set([...guide.querySelectorAll(".flow-step")]
    .map((step) => Math.round(step.getBoundingClientRect().top)));
  const shown = r.width > 0 && r.height > 0;
  const m = { x: r.left, y: r.top, w: r.width, h: r.height, r: r.right, b: r.bottom,
    vw: innerWidth, vh: innerHeight, scrollWidth: guide.scrollWidth,
    clientWidth: guide.clientWidth, rows: tops.size };
  guide.hidePopover();
  return shown ? m : null;
})()`;

/** Failures of one measured app, as messages */
function check(app, m, viewport) {
  const failures = [];
  const fail = (text) => failures.push(text);
  // A phone widens its layout viewport to the content, so compare with the
  // window asked for
  if (m.scrollWidth > viewport.width + 1)
    fail(
      `page overflows sideways: ${m.scrollWidth} px wide in ${viewport.width} px`,
    );
  if (app === "studio") {
    if (!m.panel || !m.stage || !m.view)
      fail("Controller panel or stage not shown");
    else {
      const fill = m.stage.w / m.panel.w;
      if (fill < 0.9)
        fail(
          `stage ${m.stage.w.toFixed(0)} px wide in a ${m.panel.w.toFixed(0)} px panel (${(fill * 100).toFixed(0)}%)`,
        );
      if (m.stage.h < m.stage.w * 0.4)
        fail(
          `stage only ${m.stage.h.toFixed(0)} px high for ${m.stage.w.toFixed(0)} px`,
        );
      const view = m.view.w / m.stage.w;
      if (m.threeD && (view < 0.9 || m.view.h / m.stage.h < 0.9))
        fail(
          `3D view ${m.view.w.toFixed(0)}x${m.view.h.toFixed(0)} in a ${m.stage.w.toFixed(0)}x${m.stage.h.toFixed(0)} stage`,
        );
    }
  }
  if (app === "cuttlefish") {
    if (!m.chat) fail("chat bar not shown");
    else if (Math.abs(m.chat.b - m.viewport.h) > 2)
      fail(
        `chat bar ends at ${m.chat.b.toFixed(0)} px, the window at ${m.viewport.h} px`,
      );
  }
  if (viewport.width === 1440 && !viewport.mobile) {
    const tops = m.rows.map((row) => row.top);
    const bottoms = m.rows.map((row) => row.bottom);
    const spread = Math.max(...tops) - Math.min(...tops);
    const tallest = Math.max(...bottoms) - Math.min(...tops);
    if (spread > 12 || tallest > 80)
      fail(
        `top bar not one row: ${m.bar.h.toFixed(0)} px, ` +
          m.rows
            .map((row) => `${row.name.split(" ")[0]}@${row.top.toFixed(0)}`)
            .join(" "),
      );
  }
  return failures;
}

// -------------------------------------------------------------------- run

const failures = [];
let checked = 0;
for (const lang of LANGS) {
  for (const viewport of VIEWPORTS) {
    await send("Emulation.setDeviceMetricsOverride", {
      width: viewport.width,
      height: viewport.height,
      deviceScaleFactor: viewport.mobile ? 2 : 1,
      mobile: !!viewport.mobile,
    });
    for (const theme of THEMES) {
      const url = `${base}/studio?theme=${theme}&layout=auto&nav=${viewport.nav}&rail=compact`;
      // The language is a stored choice; set it before the page reads it
      await send("Page.navigate", { url: `${base}/icons/` });
      await sleep(300);
      await evaluate(
        `localStorage.setItem("procon-lang", "${lang}");
         localStorage.setItem("procon-guide-seen", "1")`,
      );
      await send("Page.navigate", { url });
      await evaluate(
        `new Promise((resolve) => { const done = () => document.fonts.ready.then(resolve);
          document.readyState === "complete" ? done() : addEventListener("load", done); })`,
      );
      // three.js comes from a CDN; give it a moment to replace the SVG
      await evaluate(
        `new Promise((resolve) => { const t0 = performance.now(); (function wait() {
          const c = document.getElementById("procon-3d");
          if ((c && !c.hidden) || performance.now() - t0 > 4000) resolve(); else setTimeout(wait, 100); })(); })`,
      );
      for (const app of APPS) {
        if (app !== "studio") {
          await evaluate(
            `document.querySelector('.app-nav a[data-app="${app}"]').click()`,
          );
        }
        await sleep(app === "studio" ? 300 : 700);
        const m = await evaluate(MEASURE);
        const label = `${theme.padEnd(9)} ${viewport.name.padEnd(12)} ${lang} ${app.padEnd(10)}`;
        const found = check(app, m, viewport);
        checked++;
        if (found.length) {
          for (const text of found) failures.push(`${label} ${text}`);
          console.log(`FAIL ${label} ${found.join("; ")}`);
        } else {
          const note =
            app === "studio"
              ? `stage ${m.stage.w.toFixed(0)}x${m.stage.h.toFixed(0)} in ${m.panel.w.toFixed(0)}${m.threeD ? " (3D)" : " (SVG)"}`
              : app === "cuttlefish"
                ? `chat bar bottom ${m.chat.b.toFixed(0)}/${m.viewport.h}`
                : `width ${m.scrollWidth}/${viewport.width}`;
          console.log(
            `ok   ${label} ${note}${viewport.width === 1440 ? `, bar ${m.bar.h.toFixed(0)} px` : ""}`,
          );
        }
      }
      // The guide over the last app
      const g = await evaluate(GUIDE);
      const label = `${theme.padEnd(9)} ${viewport.name.padEnd(12)} ${lang} guide     `;
      const found = [];
      if (!g) found.push("guide not shown");
      else {
        if (g.x < 0 || g.y < 0 || g.r > g.vw + 1 || g.b > g.vh + 1)
          found.push(
            `guide ${g.w.toFixed(0)}x${g.h.toFixed(0)} at ${g.x.toFixed(0)},${g.y.toFixed(0)} outside ${g.vw}x${g.vh}`,
          );
        if (g.scrollWidth > g.clientWidth + 1)
          found.push(
            `guide scrolls sideways: ${g.scrollWidth}/${g.clientWidth}`,
          );
        if (!viewport.mobile && g.rows !== 1)
          found.push(`guide steps in ${g.rows} rows at ${viewport.width} px`);
      }
      checked++;
      if (found.length) {
        for (const text of found) failures.push(`${label} ${text}`);
        console.log(`FAIL ${label} ${found.join("; ")}`);
      } else {
        console.log(
          `ok   ${label} ${g.w.toFixed(0)}x${g.h.toFixed(0)}, ${g.rows} row(s)`,
        );
      }
    }
  }
}

ws.close();
if (failures.length) {
  console.error(`\n${failures.length} layout failure(s) in ${checked} views:`);
  for (const text of failures) console.error(`  ${text}`);
  await finish(1);
}
console.log(`\nAll ${checked} views pass.`);
await finish(0);
