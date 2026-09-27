// Inkspector app: pick a recorded session, then check its controller labels
// against the video frame by frame in the shared player (player.js: exact
// frames from the studio, overlays, neighbours, keys, labels table). Runs
// next to app.js and uses its helpers ($, root); its state lives in the hash
// as #inspect/s=<session>&seg=<file>&n=<frame>&delay=<ms>&pred=<path> (and
// label=1 to open in the labeling mode).

/** Frames per labels request */
const LABEL_CHUNK = 64;
/** Chip level for each source of a session's delay */
const LEVELS = { manual: "good", session: "good", setup: "warning" };

/** Read a remembered Inkspector choice */
function remembered(key, fallback) {
  try {
    return localStorage.getItem(`procon-inspect-${key}`) ?? fallback;
  } catch {
    return fallback;
  }
}

/** Remember an Inkspector choice in this browser */
function remember(key, value) {
  try {
    localStorage.setItem(`procon-inspect-${key}`, value);
  } catch {
    // Storage may be refused; the choice holds until reload
  }
}

const inspector = {
  /** Whether the Inkspector app is shown */
  shown: false,
  sessions: null,
  /** Info of the open segment, or null in the picker */
  info: null,
  /** The frame on screen, as the player reports it */
  frame: 0,
  delay: 0,
  /** Predictions file, "" for none */
  pred: "",
  /** Label chunks (promises of {truth, predictions}) by chunk index */
  labelChunks: new Map(),
  /**
   * Labeled frames marked on the scrubber, set by label.js: sorted frame
   * numbers labeled by a person (`user`) and holding only model boxes
   * (`model`), and a range being followed ([first, last] or null)
   */
  marks: { user: [], model: [], range: null },
};

/** The player in the Frame panel (app.js has `player`, the preview); label.js draws its boxes over its screen */
const framePlayer = new Player({
  screen: $("i-screen"),
  controls: $("i-player-controls"),
  scrubber: $("i-scrubber"),
  strip: $("i-strip"),
  table: $("i-rows"),
  predNote: $("i-pred-note"),
  remember: "inspect",
  neighbours: { radius: 3 },
  onFrame(n) {
    inspector.frame = n;
    writeHash();
    $("i-delay").value = inspector.delay;
    // For the labeling mode (label.js), which draws this frame's boxes
    window.dispatchEvent(new CustomEvent("inspect-frame"));
  },
});

// ----------------------------------------------------------------- helpers

function duration(ms) {
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${rest}` : `${m}:${rest}`;
}

function started(summary) {
  const date = new Date(summary.started_at_unix_ms);
  return isNaN(date) ? summary.name : date.toLocaleString();
}

/** Half the width of an interval, rounded, as "±15" */
function plusMinus(interval) {
  return interval ? ` ±${Math.round((interval[1] - interval[0]) / 2)}` : "";
}

/**
 * The delay the loader applies, with where it comes from: "212 ms ±15 ·
 * session", "≈284 ms ±139 · setup", "210 ms · by hand"; or plainly why
 * there is none
 */
function delayText(calibration) {
  const applied = calibration?.applied;
  if (!applied)
    return `no delay: ${calibration?.reason ?? "not calibrated yet"}`;
  const ms = Math.round(applied.video_delay_ms);
  if (applied.source === "manual") return `${ms} ms · by hand`;
  const approx = applied.source === "setup" ? "≈" : "";
  return `${approx}${ms} ms${plusMinus(applied.interval_ms)} · ${applied.source}`;
}

/** What the source of the applied delay means, for the session tile */
function delaySourceText(calibration) {
  const applied = calibration?.applied;
  const own = calibration?.own;
  if (!applied) return calibration?.reason ?? "not calibrated yet";
  if (applied.source === "manual")
    return own?.video_delay_ms != null
      ? `set by hand; own estimate ${Math.round(own.video_delay_ms)} ms (${own.confidence})`
      : "set by hand";
  if (applied.source === "setup")
    return `this setup's delay${plusMinus(applied.interval_ms)} ms; too little to measure here`;
  return `measured from this session${plusMinus(applied.interval_ms)} ms (${own?.confidence})`;
}

function gameText(settings) {
  if (!settings) return "–";
  const parts = [
    settings.motion_controls
      ? `motion ${settings.motion_sensitivity}`
      : "no motion",
    `stick ${settings.stick_sensitivity}`,
  ];
  if (settings.invert_x) parts.push("invert x");
  if (settings.invert_y) parts.push("invert y");
  return parts.join(" · ");
}

/** An API URL for the open segment */
function api(path, params = {}) {
  const query = new URLSearchParams({
    s: inspector.info.session,
    seg: inspector.info.segment,
    ...params,
  });
  return `/api/inspect/${path}?${query}`;
}

/** The hash of a view: a segment at a frame, delay and predictions */
function hashOf(session, segment, extra = {}) {
  const params = new URLSearchParams({ s: session, seg: segment ?? "" });
  for (const [key, value] of Object.entries(extra)) {
    if (value !== "" && value != null) params.set(key, value);
  }
  return `#inspect/${params}`;
}

function writeHash() {
  const { info, frame, delay, pred } = inspector;
  if (!info) return;
  const hash = hashOf(info.session, info.segment, { n: frame, delay, pred });
  history.replaceState(null, "", hash);
  rememberView(hash);
}

/** Where the Inkspector's app link leads: back to this view, even after a reload */
function rememberView(hash) {
  document.querySelector('.app-nav [data-app="inspect"]').href = hash;
  remember("view", hash);
}

// ------------------------------------------------------------------ picker

async function loadSessions() {
  const response = await fetch("/api/inspect/sessions");
  const data = await response.json();
  if (!response.ok) {
    $("i-sessions-note").textContent = data.error;
    inspector.sessions = [];
    return;
  }
  inspector.sessions = data.sessions;
  const select = $("i-session");
  for (const summary of data.sessions) {
    select.add(new Option(summary.name, summary.name));
  }
  $("i-sessions-note").textContent = `${data.sessions.length} in ${data.root}`;
  const body = $("i-sessions");
  for (const summary of data.sessions) {
    const tr = document.createElement("tr");
    const level = LEVELS[summary.calibration?.applied?.source] ?? "";
    const segments = summary.segments
      .map(
        (s) =>
          `<button type="button" class="mode-toggle" data-seg="${escapeHtml(s.file)}">${escapeHtml(s.file)}${s.sound ? " ♪" : ""}</button>`,
      )
      .join("");
    tr.innerHTML = `
      <td>${escapeHtml(started(summary))}<br><span class="panel-note">${escapeHtml(summary.name)}</span></td>
      <td>${duration(summary.stopped_at_unix_ms - summary.started_at_unix_ms)}</td>
      <td><div class="segments">${segments}</div></td>
      <td>${summary.height}p${Math.round(summary.fps)}${summary.sound ? " · sound" : ""}</td>
      <td>${summary.controller_reports ?? "–"}</td>
      <td>${escapeHtml(gameText(summary.game_settings))}</td>
      <td class="level-${level}">${delayText(summary.calibration)}</td>`;
    tr.onclick = (event) => {
      const seg = event.target.dataset?.seg ?? summary.segments[0]?.file;
      location.hash = hashOf(summary.name, seg);
    };
    body.append(tr);
  }
}

function showPicker() {
  framePlayer.close();
  inspector.resume = false;
  rememberView("#inspect");
  inspector.info = null;
  $("inspect-viewer").hidden = true;
  $("inspect-picker").hidden = false;
  $("i-delay-chip").hidden = true;
  $("i-session").value = "";
}

// ------------------------------------------------------------------ viewer

/** Load a segment's info and show it at the hash's frame and delay */
async function showSegment(session, segment, state) {
  framePlayer.close();
  $("inspect-picker").hidden = true;
  $("inspect-viewer").hidden = false;
  $("i-session").value = session;
  framePlayer.status(`Loading ${session}…`);
  const query = new URLSearchParams({ s: session });
  if (segment) query.set("seg", segment);
  const response = await fetch(`/api/inspect/info?${query}`);
  const info = await response.json();
  if (!response.ok) {
    framePlayer.status(info.error);
    return;
  }
  inspector.info = info;
  inspector.labelChunks = new Map();
  // Marks come with the new segment's labels (label.js)
  inspector.marks = { user: [], model: [], range: null };
  inspector.delay = state.has("delay")
    ? parseFloat(state.get("delay")) || 0
    : info.video_delay_ms;
  inspector.pred = state.get("pred") ?? "";
  $("i-pred").value = inspector.pred;
  drawSession();
  framePlayer.open(
    {
      frames: info.frames,
      fps: info.fps,
      frame: (n) => api("frame", { n }),
      audio: info.sound ? api("audio") : null,
      sound: info.sound,
      labels,
      title: `${info.session} · ${info.segment}`,
    },
    parseInt(state.get("n")) || 0,
  );
  drawMarks();
}

function drawSession() {
  const { info } = inspector;
  const summary = info.summary;
  const segmentSelect = $("i-segment");
  segmentSelect.replaceChildren(
    ...summary.segments.map(
      (s) => new Option(s.file + (s.sound ? " ♪" : ""), s.file),
    ),
  );
  segmentSelect.value = info.segment;
  segmentSelect.hidden = summary.segments.length < 2;

  const chip = $("i-delay-chip");
  chip.hidden = false;
  chip.dataset.level = LEVELS[info.calibration?.applied?.source] ?? "off";
  // Short in the bar; the dot tells the source's confidence, the title the rest
  const applied = info.calibration?.applied;
  chip.querySelector(".chip-text").textContent = applied
    ? `delay ${applied.source === "setup" ? "≈" : ""}${Math.round(applied.video_delay_ms)} ms`
    : "no delay";
  chip.title = `Video delay ${delayText(info.calibration)}: ${delaySourceText(info.calibration)}`;
  $("i-delay-remove").hidden = info.calibration?.applied?.source !== "manual";

  const tile = (label, value, note) =>
    `<div class="tile"><span class="tile-label">${label}</span><span class="tile-value num">${escapeHtml(value)}</span><span class="tile-note">${escapeHtml(note)}</span></div>`;
  const calibration = info.calibration;
  const date = new Date(summary.started_at_unix_ms);
  $("i-session-tiles").innerHTML = [
    tile("Started", date.toLocaleTimeString(), date.toLocaleDateString()),
    tile(
      "Duration",
      duration(summary.stopped_at_unix_ms - summary.started_at_unix_ms),
      `${summary.segments.length} segment(s)`,
    ),
    tile(
      "Video",
      `${summary.height}p${Math.round(info.fps)}`,
      `${info.frames} frames · ${summary.sound ? "sound" : "no sound"}`,
    ),
    tile(
      "Video delay",
      calibration?.applied
        ? `${calibration.applied.source === "setup" ? "≈" : ""}${Math.round(calibration.applied.video_delay_ms)} ms`
        : "–",
      delaySourceText(calibration),
    ),
    tile("Reports", summary.controller_reports ?? "–", summary.name),
    tile(
      "Game settings",
      summary.game_settings
        ? `${summary.game_settings.motion_sensitivity} / ${summary.game_settings.stick_sensitivity}`
        : "–",
      summary.game_settings ? gameText(summary.game_settings) : "not recorded",
    ),
  ].join("");
  $("i-session-note").textContent =
    `1 frame = ${(1000 / info.fps).toFixed(1)} ms`;
}

/** Labels of frame n as [truth, prediction or undefined] */
async function labels(n) {
  const { labelChunks, delay, pred } = inspector;
  const chunk = Math.floor(n / LABEL_CHUNK);
  if (!labelChunks.has(chunk)) {
    const start = chunk * LABEL_CHUNK;
    const url = api("labels", {
      start,
      stop: start + LABEL_CHUNK,
      delay,
      pred,
    });
    labelChunks.set(
      chunk,
      fetch(url).then(async (response) => {
        const data = await response.json();
        if (!response.ok) throw new Error(data.error);
        return data;
      }),
    );
  }
  const data = await labelChunks.get(chunk);
  const i = n - chunk * LABEL_CHUNK;
  return [data.truth[i], data.predictions ? data.predictions[i] : undefined];
}

/** Forget what depends on the delay or the predictions */
function resetLabels() {
  inspector.labelChunks = new Map();
}

/** Hand the labeled frames (label.js's list) to the player's scrubber */
function drawMarks() {
  const { user, model, range } = inspector.marks;
  framePlayer.setMarks({
    ticks: [
      ...user.map((n) => ({ n, kind: "user" })),
      ...model.map((n) => ({ n, kind: "model", short: true })),
    ],
    ranges: range ? [{ a: range[0], b: range[1], kind: "range" }] : [],
  });
}

/** Jump to frame n, pausing playback */
function go(n) {
  if (!inspector.info) return;
  framePlayer.go(n);
}

// ----------------------------------------------------------------- actions

async function random(active) {
  if (!inspector.info) return;
  const url = api("random", {
    delay: inspector.delay,
    active: active ? 1 : 0,
  });
  go((await (await fetch(url)).json()).frame);
}

// Keys act only while the Inkspector shows a segment; the player has the
// transport's (Space, arrows, Home, End, G)
document.addEventListener("keydown", (event) => {
  const tag = event.target.tagName;
  if (!inspector.shown || !inspector.info) return;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  if (event.key.toLowerCase() !== "r") return;
  random(!event.shiftKey);
  event.preventDefault();
});

$("i-delay").addEventListener("change", () => {
  inspector.delay = parseFloat($("i-delay").value) || 0;
  resetLabels();
  go(inspector.frame);
});

/** Save or remove this session's delay set by hand, then show the result */
async function saveDelay(remove) {
  const { info } = inspector;
  if (!info) return;
  const body = remove
    ? { s: info.session, remove: true }
    : { s: info.session, video_delay_ms: parseFloat($("i-delay").value) || 0 };
  const response = await fetch("/api/inspect/delay", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const calibration = await response.json();
  if (!response.ok) {
    alert(`Could not save the delay: ${calibration.error}`);
    return;
  }
  info.calibration = calibration;
  const summary = inspector.sessions?.find((s) => s.name === info.session);
  if (summary) summary.calibration = calibration;
  if (remove && calibration.applied) {
    inspector.delay = calibration.applied.video_delay_ms;
    resetLabels();
    framePlayer.refresh();
  }
  drawSession();
}
$("i-delay-save").onclick = () => saveDelay(false);
$("i-delay-remove").onclick = () => saveDelay(true);
$("i-pred").addEventListener("change", () => {
  inspector.pred = $("i-pred").value.trim();
  resetLabels();
  go(inspector.frame);
});
$("i-random-active").onclick = () => random(true);
$("i-random-any").onclick = () => random(false);
$("i-session").onchange = (event) => {
  const name = event.target.value;
  const summary = inspector.sessions.find((s) => s.name === name);
  location.hash = name ? hashOf(name, summary?.segments[0]?.file) : "#inspect";
};
$("i-segment").onchange = (event) => {
  location.hash = hashOf(inspector.info.session, event.target.value);
};
$("i-stick-tol").textContent = STICK_TOLERANCE;
$("i-gyro-tol").textContent = GYRO_TOLERANCE;

/** Show what the hash names: a segment, or the picker */
async function routeInspector(state) {
  if (!inspector.sessions) await loadSessions();
  const session = state.get("s");
  if (!session) return showPicker();
  const segment = state.get("seg") || null;
  const { info } = inspector;
  const same =
    info && info.session === session && (!segment || info.segment === segment);
  if (!same) return showSegment(session, segment, state);
  $("inspect-picker").hidden = true;
  $("inspect-viewer").hidden = false;
  const delay = state.has("delay")
    ? parseFloat(state.get("delay")) || 0
    : inspector.delay;
  const pred = state.get("pred") ?? "";
  if (delay !== inspector.delay || pred !== inspector.pred) {
    inspector.delay = delay;
    inspector.pred = pred;
    $("i-pred").value = pred;
    resetLabels();
  }
  go(parseInt(state.get("n")) || 0);
  // Back from another app: carry on playing if it was
  if (inspector.resume) {
    inspector.resume = false;
    framePlayer.play();
  }
}

window.addEventListener("app-route", (event) => {
  const { app, state } = event.detail;
  if (inspector.shown && app !== "inspect") {
    // Leaving: remember whether it was playing, to resume on return
    inspector.resume = framePlayer.playing;
    framePlayer.pause();
  }
  inspector.shown = app === "inspect";
  framePlayer.enabled = inspector.shown;
  if (inspector.shown) routeInspector(state);
});
$("i-back").onclick = () => {
  location.hash = "#inspect";
};
// The last view, for the app link after a reload
document.querySelector('.app-nav [data-app="inspect"]').href = remembered(
  "view",
  "#inspect",
);
