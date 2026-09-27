// A real Chrome over the DevTools protocol (Node's WebSocket, no
// packages): started on the capture's own profile folder with a debugging
// port, or attached to one already listening there. One page is driven:
// navigations, wheel scrolls, clicks and keys like a person's, and the
// JSON the page itself receives (X's GraphQL API by default; `keep` picks
// another site's answers), read from the network events.

import { spawn } from "node:child_process";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The browser's version record at `port`, or null when nothing answers */
export async function probe(port) {
  try {
    const r = await fetch(`http://127.0.0.1:${port}/json/version`);
    return r.ok ? await r.json() : null;
  } catch {
    return null;
  }
}

/** Starts Chrome on `profile` with the debugging port; resolves once it
 * listens. `headless` runs it without a window (not for X: the login and
 * a challenge need one). */
export async function launch({ binary, profile, port, headless = false }) {
  const args = [
    `--remote-debugging-port=${port}`,
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-sync",
    "--window-size=1280,900",
    "about:blank",
  ];
  if (headless) args.unshift("--headless=new", "--mute-audio");
  const child = spawn(binary, args, { stdio: "ignore" });
  const exited = new Promise((resolve) => child.on("exit", resolve));
  let failed = null;
  child.on("error", (error) => (failed = error));
  for (let i = 0; i < 300; i++) {
    if (failed) throw new Error(`cannot start ${binary}: ${failed.message}`);
    if (child.exitCode !== null) throw new Error(`${binary} exited at start`);
    if (await probe(port)) return { child, exited, pid: child.pid, port };
    await sleep(100);
  }
  child.kill();
  throw new Error(`${binary} did not listen on port ${port}`);
}

/** Asks the browser at `port` to close through DevTools (`Browser.close`),
 * as its own window would: unlike a signal, it first writes the cookies
 * and the rest of the profile to disk */
async function closeBrowser(port) {
  const version = await probe(port);
  if (!version?.webSocketDebuggerUrl) return;
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });
  ws.send(JSON.stringify({ id: 1, method: "Browser.close" }));
}

/** Ends the Chrome we started: `Browser.close`, then SIGTERM after 20 s,
 * then SIGKILL after 5 more. A signal alone loses what the profile had not
 * written yet, such as a login's cookies. */
export async function stop({ child, exited, port }) {
  if (child.exitCode !== null) return;
  const within = (ms) =>
    Promise.race([exited.then(() => true), sleep(ms).then(() => false)]);
  if (port) await closeBrowser(port).catch(() => {});
  if (port && (await within(20000))) return;
  child.kill("SIGTERM");
  if (!(await within(5000))) child.kill("SIGKILL");
}

/** Whether an address is X's GraphQL API, the answers kept by default */
export const isGraphql = (url) => /\/i\/api\/graphql\//.test(url);

/** Opens a new tab and attaches to it. `keep(url)` says which answers'
 * bodies are kept for `drain` (X's GraphQL API by default). */
export async function openPage(port, options = {}) {
  const r = await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, {
    method: "PUT",
  });
  if (!r.ok) throw new Error(`cannot open a tab: ${r.status}`);
  const target = await r.json();
  const page = new Page(target, options);
  await page.connect();
  return page;
}

/** The centre of the first visible element `selector` names, scrolled
 * into view, as a page script: `[x, y]` or null */
function boxOf(selector) {
  return `(() => {
    const els = [...document.querySelectorAll(${JSON.stringify(selector)})];
    const el = els.find((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; });
    if (!el) return null;
    el.scrollIntoView({ block: "center", inline: "nearest" });
    const r = el.getBoundingClientRect();
    return [r.left + r.width / 2, r.top + r.height / 2];
  })()`;
}

/** The centre of the first visible small element whose own text holds
 * one of `words` (and a digit, when `digit`), as a page script */
function textBoxOf(words, digit) {
  return `(() => {
    const words = ${JSON.stringify(words)};
    const els = [...document.querySelectorAll("span, div, a, button, p, li")];
    const el = els.find((e) => {
      const t = (e.innerText || "").trim();
      if (!t || t.length > 24) return false;
      if (!words.some((w) => t.includes(w))) return false;
      if (${digit ? "true" : "false"} && !/\\d/.test(t)) return false;
      const r = e.getBoundingClientRect();
      return r.width > 0 && r.height > 0;
    });
    if (!el) return null;
    el.scrollIntoView({ block: "center", inline: "nearest" });
    const r = el.getBoundingClientRect();
    return [r.left + r.width / 2, r.top + r.height / 2];
  })()`;
}

/** One tab under DevTools control */
export class Page {
  constructor(target, { keep = isGraphql } = {}) {
    this.target = target;
    this.keep = keep;
    this.ws = null;
    this.nextId = 0;
    this.pending = new Map();
    this.listeners = new Map();
    /** Answers received (`keep` accepted their address) and not drained yet */
    this.responses = [];
    /** Requests seen, by id, until they finish */
    this.requests = new Map();
    this.loadWaiters = [];
  }

  async connect() {
    const ws = new WebSocket(this.target.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.onopen = resolve;
      ws.onerror = () => reject(new Error("cannot connect to the tab"));
    });
    this.ws = ws;
    ws.onmessage = (event) => this.receive(JSON.parse(event.data));
    ws.onclose = () => {
      for (const { reject } of this.pending.values())
        reject(new Error("tab closed"));
      this.pending.clear();
    };
    this.on("Network.responseReceived", (p) => {
      this.requests.set(p.requestId, {
        url: p.response.url,
        status: p.response.status,
      });
    });
    this.on("Network.loadingFinished", (p) => this.finished(p.requestId));
    this.on("Network.loadingFailed", (p) => this.requests.delete(p.requestId));
    this.on("Page.loadEventFired", () => {
      for (const resolve of this.loadWaiters.splice(0)) resolve(true);
    });
    await this.send("Network.enable", { maxResourceBufferSize: 50_000_000 });
    await this.send("Page.enable");
  }

  receive(message) {
    if (message.id && this.pending.has(message.id)) {
      const { resolve, reject } = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) reject(new Error(message.error.message));
      else resolve(message.result ?? {});
      return;
    }
    if (message.method) {
      for (const f of this.listeners.get(message.method) ?? [])
        f(message.params ?? {});
    }
  }

  /** One DevTools command */
  send(method, params = {}) {
    return new Promise((resolve, reject) => {
      const id = ++this.nextId;
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  on(method, f) {
    if (!this.listeners.has(method)) this.listeners.set(method, []);
    this.listeners.get(method).push(f);
  }

  /** A finished request: a kept answer's body is read */
  async finished(requestId) {
    const request = this.requests.get(requestId);
    this.requests.delete(requestId);
    if (!request || !this.keep(request.url)) return;
    let json = null;
    try {
      const { body, base64Encoded } = await this.send(
        "Network.getResponseBody",
        { requestId },
      );
      const text = base64Encoded
        ? Buffer.from(body, "base64").toString("utf8")
        : body;
      json = JSON.parse(text);
    } catch {
      // The body is gone or not JSON; the status still tells
    }
    this.responses.push({ url: request.url, status: request.status, json });
  }

  /** The answers received since the last drain */
  drain() {
    return this.responses.splice(0);
  }

  /** Waits until a queued answer satisfies `test` (without draining), at
   * most `timeoutMs`; true when one did */
  async waitFor(test, timeoutMs = 20_000) {
    const end = Date.now() + timeoutMs;
    while (Date.now() < end) {
      if (this.responses.some(test)) return true;
      await sleep(200);
    }
    return this.responses.some(test);
  }

  /** The value of `expression` in the page (awaited if a promise) */
  async evaluate(expression) {
    const reply = await this.send("Runtime.evaluate", {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (reply.exceptionDetails)
      throw new Error(
        reply.exceptionDetails.exception?.description ?? "evaluation failed",
      );
    return reply.result?.value;
  }

  /** Navigates and waits for the load event (at most 30 s), then a moment
   * for the app to draw */
  async goto(url) {
    const loaded = new Promise((resolve) => this.loadWaiters.push(resolve));
    await this.send("Page.navigate", { url });
    await Promise.race([loaded, sleep(30_000)]);
    await sleep(1500 + Math.random() * 1500);
  }

  /** The page's address */
  location() {
    return this.evaluate("location.href");
  }

  /** The start of the page's visible text */
  text() {
    return this.evaluate(
      '(document.body ? document.body.innerText : "").slice(0, 6000)',
    );
  }

  /** Scrolls down like a wheel: a few notches with short gaps, over the
   * element `selector` names (a scrolling pane) or the page */
  async scroll({ selector } = {}) {
    let x;
    let y;
    const point = selector ? await this.evaluate(boxOf(selector)) : null;
    if (point) [x, y] = point;
    else {
      const [w, h] = await this.evaluate("[innerWidth, innerHeight]");
      x = Math.round(w * (0.35 + Math.random() * 0.3));
      y = Math.round(h * (0.3 + Math.random() * 0.4));
    }
    const notches = 2 + Math.floor(Math.random() * 3);
    for (let i = 0; i < notches; i++) {
      await this.send("Input.dispatchMouseEvent", {
        type: "mouseWheel",
        x,
        y,
        deltaX: 0,
        deltaY: 250 + Math.round(Math.random() * 400),
      });
      await sleep(90 + Math.random() * 220);
    }
    await sleep(1200 + Math.random() * 1300);
  }

  /** A left click at a point of the page (`[x, y]`), a little off the
   * exact centre like a hand's; false without a point */
  async clickAt(point) {
    if (!point) return false;
    const x = point[0] + (Math.random() - 0.5) * 8;
    const y = point[1] + (Math.random() - 0.5) * 6;
    for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) {
      const params = { type, x, y };
      if (type !== "mouseMoved")
        Object.assign(params, { button: "left", clickCount: 1 });
      await this.send("Input.dispatchMouseEvent", params);
      await sleep(50 + Math.random() * 60);
    }
    await sleep(800 + Math.random() * 700);
    return true;
  }

  /** Clicks the first visible element `selector` names; false when there
   * is none */
  async click(selector) {
    return this.clickAt(await this.evaluate(boxOf(selector)));
  }

  /** Clicks the first visible small element whose own text holds one of
   * `words` (and a digit, with `digit`: a count); false when there is none */
  async clickText(words, { digit = false } = {}) {
    return this.clickAt(await this.evaluate(textBoxOf(words, digit)));
  }

  /** A key pressed and released (`"Escape"`, 27) */
  async key(key, code) {
    for (const type of ["keyDown", "keyUp"])
      await this.send("Input.dispatchKeyEvent", {
        type,
        key,
        code: key,
        windowsVirtualKeyCode: code,
      });
    await sleep(600 + Math.random() * 600);
  }

  /** Whether the app shows a signed-in account, and its handle: the
   * profile link of the app's own tab bar (X) */
  async signedIn() {
    const href = await this.evaluate(
      `document.querySelector('a[data-testid="AppTabBar_Profile_Link"]')?.getAttribute("href") ?? null`,
    );
    const m = /^\/([A-Za-z0-9_]{1,15})$/.exec(href ?? "");
    return m ? m[1] : null;
  }

  /** The posts drawn on the page, from the DOM: the fallback when no
   * GraphQL answer was seen. Id and author from each article's status
   * link, the text from its text block. */
  domPosts() {
    return this
      .evaluate(`[...document.querySelectorAll('article[data-testid="tweet"]')].map((a) => {
      const link = [...a.querySelectorAll('a[href*="/status/"]')]
        .map((l) => /^\\/([A-Za-z0-9_]{1,15})\\/status\\/(\\d+)$/.exec(l.getAttribute("href")))
        .find(Boolean);
      const text = a.querySelector('[data-testid="tweetText"]')?.innerText ?? "";
      const time = a.querySelector("time")?.getAttribute("datetime") ?? null;
      return link ? { id: link[2], handle: link[1], text, date: time } : null;
    }).filter(Boolean)`);
  }

  async close() {
    try {
      await this.send("Page.close");
    } catch {
      // Already gone
    }
    this.ws?.close();
  }
}
