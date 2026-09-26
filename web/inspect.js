// Inkspector app: pick a recorded session, then check its controller labels
// against the video frame by frame. Runs next to app.js and uses its helpers
// ($, root, drawInputHud, stickPercent); its state lives in the hash as
// #inspect/s=<session>&seg=<file>&n=<frame>&delay=<ms>&pred=<path>.

/** Stick difference (raw 12-bit units) that counts as a mismatch */
const STICK_TOLERANCE = 256;
/** Gyro difference (degrees over the frame) that counts as a mismatch */
const GYRO_TOLERANCE = 0.5;
/** Neighbours shown on each side of the current frame */
const RADIUS = 3;
/** Frames requested ahead of the current one while playing */
const PREFETCH = 45;
/** Frames per labels request */
const LABEL_CHUNK = 64;
/** Wait after the last scrubber move before seeking, in ms */
const SCRUB_DEBOUNCE_MS = 120;
/** Chip level for each calibration confidence */
const LEVELS = { high: "good", medium: "warning", low: "critical" };
/** Angular rate that fills a gyro bar of the full overlay, in °/s */
const FULL_GYRO_DPS = 300;
/** Button labels of the full overlay, in the order of the label names */
const FULL_KEYS = [
  ["y", "Y"],
  ["x", "X"],
  ["b", "B"],
  ["a", "A"],
  ["sr_right", "SR_R"],
  ["sl_right", "SL_R"],
  ["r", "R"],
  ["zr", "ZR"],
  ["minus", "Minus"],
  ["plus", "Plus"],
  ["r_stick", "RStick"],
  ["l_stick", "LStick"],
  ["home", "Home"],
  ["capture", "Capture"],
  ["down", "Down"],
  ["up", "Up"],
  ["right", "Right"],
  ["left", "Left"],
  ["sr_left", "SR_L"],
  ["sl_left", "SL_L"],
  ["l", "L"],
  ["zl", "ZL"],
];

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
  frame: 0,
  delay: 0,
  /** Predictions file, "" for none */
  pred: "",
  playing: false,
  /** Loaded frame images by frame number */
  images: new Map(),
  /** Label chunks (promises of {truth, predictions}) by chunk index */
  labelChunks: new Map(),
  /** The input overlay drawn over the frame, a copy of the video's */
  hud: null,
  /** The full overlay: every button, both sticks, gyro bars with values */
  full: null,
  /** Overlay style: full, minimal (the video's) or none */
  overlay: remembered("overlay", "full"),
  /** Play the segment's sound, which then sets the frame */
  sound: remembered("sound", "true") === "true",
};

const audio = $("i-audio");

const canvas = $("i-frame");
const context = canvas.getContext("2d");

// ----------------------------------------------------------------- helpers

function escapeHtml(text) {
  return String(text).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c],
  );
}

function clock(seconds) {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds - 60 * minutes).toFixed(3).padStart(6, "0")}`;
}

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

/** A calibrated delay the loader applies: high or medium confidence only */
function usable(calibration) {
  return (
    calibration?.video_delay_ms != null &&
    ["high", "medium"].includes(calibration.confidence)
  );
}

function delayText(calibration) {
  if (!calibration) return "not calibrated";
  // A low-confidence number is a guess the loader ignores; don't show it as a delay
  if (!usable(calibration)) return "uncertain";
  return `${Math.round(calibration.video_delay_ms)} ms · ${calibration.confidence}`;
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
    const level = LEVELS[summary.calibration?.confidence] ?? "";
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
  pause();
  inspector.resume = false;
  rememberView("#inspect");
  inspector.info = null;
  $("inspect-viewer").hidden = true;
  $("inspect-picker").hidden = false;
  $("i-position").hidden = true;
  $("i-delay-chip").hidden = true;
  $("i-session").value = "";
}

// ------------------------------------------------------------------ viewer

/** Load a segment's info and show it at the hash's frame and delay */
async function showSegment(session, segment, state) {
  pause();
  $("inspect-picker").hidden = true;
  $("inspect-viewer").hidden = false;
  $("i-session").value = session;
  const chip = $("i-position");
  chip.hidden = false;
  chip.textContent = `Loading ${session}…`;
  const query = new URLSearchParams({ s: session });
  if (segment) query.set("seg", segment);
  const response = await fetch(`/api/inspect/info?${query}`);
  const info = await response.json();
  if (!response.ok) {
    chip.textContent = info.error;
    return;
  }
  inspector.info = info;
  // A new segment: forget the previous one's sound
  audio.removeAttribute("src");
  delete audio.dataset.source;
  inspector.images = new Map();
  inspector.labelChunks = new Map();
  inspector.delay = state.has("delay")
    ? parseFloat(state.get("delay")) || 0
    : info.video_delay_ms;
  inspector.pred = state.get("pred") ?? "";
  $("i-pred").value = inspector.pred;
  drawSession();
  markSound();
  show(parseInt(state.get("n")) || 0);
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
  chip.dataset.level = LEVELS[info.calibration?.confidence] ?? "off";
  chip.querySelector(".chip-text").textContent =
    `delay ${delayText(info.calibration)}`;

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
      "Calibrated delay",
      usable(calibration)
        ? `${Math.round(calibration.video_delay_ms)} ms`
        : "–",
      !calibration
        ? "not calibrated"
        : usable(calibration)
          ? `${calibration.confidence} · spread ${calibration.spread_ms ?? "–"} ms`
          : calibration.video_delay_ms != null
            ? `uncertain: ${Math.round(calibration.video_delay_ms)} ms guessed, not used`
            : "uncertain: too little aiming to measure",
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

function clamp(n) {
  return Math.max(0, Math.min(inspector.info.frames - 1, n));
}

function frameUrl(n) {
  return api("frame", { n });
}

/** The frame's image, loading it once */
function image(n) {
  const { images } = inspector;
  if (!images.has(n)) {
    const img = new Image();
    img.loaded = new Promise((resolve) => {
      img.onload = () => resolve(true);
      img.onerror = () => resolve(false);
    });
    img.src = frameUrl(n);
    images.set(n, img);
  }
  return images.get(n);
}

/** Request the next frames and forget those far away */
function prefetch(n) {
  const { images, info } = inspector;
  for (let k = n; k < Math.min(info.frames, n + PREFETCH); k++) image(k);
  for (const k of images.keys()) {
    if (k < n - 2 * RADIUS || k > n + 2 * PREFETCH) images.delete(k);
  }
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

function stick(v) {
  return v ? `${v[0].toFixed(0)}, ${v[1].toFixed(0)}` : "";
}

function gyro(v) {
  return v ? v.map((x) => x.toFixed(2)).join(", ") : "";
}

function summaryText(label) {
  if (!label.valid) return "no reports";
  const buttons = label.buttons.join(" ") || "–";
  return `${buttons}<br>L ${stick(label.left_stick)}<br>R ${stick(label.right_stick)}<br>g ${gyro(label.gyro_deg)}`;
}

/** Whether truth and prediction differ in one column */
function differs(key, truth, pred) {
  if (!pred || !truth.valid) return false;
  const a = truth[key];
  const b = pred[key];
  if (b == null) return false;
  if (key === "buttons") return [...a].sort().join() !== [...b].sort().join();
  if (key === "valid") return a !== b;
  const tolerance = key === "gyro_deg" ? GYRO_TOLERANCE : STICK_TOLERANCE;
  return a.some((x, i) => Math.abs(x - b[i]) > tolerance);
}

function cell(key, truth, pred, format) {
  const td = document.createElement("td");
  if (differs(key, truth, pred)) td.className = "mismatch";
  td.innerHTML = format(truth[key]);
  if (pred !== undefined) {
    const value = pred && pred[key] != null ? format(pred[key]) : "–";
    td.innerHTML += `<div class="pred">${value}</div>`;
  }
  return td;
}

function drawScrubber(n) {
  const { frames } = inspector.info;
  const percent = frames > 1 ? (100 * n) / (frames - 1) : 0;
  $("i-scrub-fill").style.width = `${percent}%`;
  $("i-scrub-thumb").style.left = `${percent}%`;
}

/** Draw a frame's true actions over it, as the video's input overlay does */
function drawLabel(label) {
  const { hud, full, overlay } = inspector;
  hud.toggleAttribute("hidden", overlay !== "minimal" || !label.valid);
  full.toggleAttribute("hidden", overlay !== "full");
  $("i-no-reports").hidden = label.valid || overlay === "full";
  if (overlay === "full") return drawFullOverlay(label);
  if (overlay !== "minimal" || !label.valid) return;
  const fps = inspector.info.fps;
  drawInputHud(hud, {
    left: label.left_stick.map(stickPercent),
    right: label.right_stick.map(stickPercent),
    pressed: new Set(label.buttons),
    yaw: label.gyro_deg[2] * fps,
    pitch: label.gyro_deg[1] * fps,
  });
}

/**
 * Build the full overlay (the style of AgentZero's agentzero-overlay): a
 * title band on top, and below the frame both sticks, a grid of every
 * button and bars of the yaw and pitch turned over the frame
 */
function buildFullOverlay() {
  const svg = svgEl("svg", {
    class: "full-hud",
    viewBox: "0 0 640 360",
    "aria-hidden": "true",
  });
  const text = (attrs, content = "") =>
    Object.assign(svgEl("text", attrs), { textContent: content });
  svg.append(
    svgEl("rect", { class: "full-band", width: 640, height: 38 }),
    text({ class: "full-title", x: 8, y: 16, "data-full": "title" }),
    text({ class: "full-alert", x: 8, y: 31, "data-full": "alert" }),
    svgEl("rect", { class: "full-band", y: 250, width: 640, height: 110 }),
  );
  for (const [side, cx] of [
    ["l", 50],
    ["r", 590],
  ]) {
    svg.append(
      svgEl("circle", { class: "full-ring", cx, cy: 305, r: 40 }),
      svgEl("circle", {
        class: "full-dot",
        cx,
        cy: 305,
        r: 5,
        "data-full": `stick-${side}`,
      }),
    );
  }
  const columns = FULL_KEYS.length / 2;
  const box = 440 / columns;
  FULL_KEYS.forEach(([name, label], i) => {
    const x = 100 + (i % columns) * box;
    const y = 256 + Math.floor(i / columns) * 26;
    const key = svgEl("g", { class: "full-key", "data-full-key": name });
    key.append(
      svgEl("rect", { x: x + 1, y, width: box - 2, height: 22 }),
      text({ x: x + box / 2, y: y + 15, "text-anchor": "middle" }, label),
    );
    svg.append(key);
  });
  [
    ["yaw", "yaw (gyro z)", 318],
    ["pitch", "pitch (gyro y)", 338],
  ].forEach(([name, label, y]) => {
    svg.append(
      svgEl("rect", { class: "full-track", x: 100, y, width: 440, height: 12 }),
      svgEl("rect", {
        class: "full-bar",
        x: 320,
        y,
        width: 0,
        height: 12,
        "data-full": name,
      }),
      svgEl("line", { class: "full-mid", x1: 320, x2: 320, y1: y, y2: y + 12 }),
      text({
        class: "full-value",
        x: 104,
        y: y + 10,
        "data-full": `${name}-text`,
        "data-label": label,
      }),
    );
  });
  return svg;
}

/** Draw a frame's label on the full overlay */
function drawFullOverlay(label) {
  const { full, info, frame } = inspector;
  const part = (name) => full.querySelector(`[data-full="${name}"]`);
  part("title").textContent =
    `${info.session} · ${info.segment}  frame ${frame}  ${(frame / info.fps).toFixed(3)} s`;
  part("alert").textContent = label.valid ? "" : "NO REPORTS";
  const pressed = new Set(label.valid ? label.buttons : []);
  for (const key of full.querySelectorAll("[data-full-key]")) {
    key.classList.toggle("on", pressed.has(key.dataset.fullKey));
  }
  for (const [side, stick] of [
    ["l", label.left_stick],
    ["r", label.right_stick],
  ]) {
    const [x, y] = label.valid ? stick.map(stickPercent) : [0, 0];
    const dot = part(`stick-${side}`);
    const cx = side === "l" ? 50 : 590;
    dot.setAttribute("cx", (cx + (x / 100) * 40).toFixed(1));
    dot.setAttribute("cy", (305 - (y / 100) * 40).toFixed(1));
  }
  // Rotation over the frame; a full bar is FULL_GYRO_DPS
  const fullDeg = FULL_GYRO_DPS / info.fps;
  for (const [name, degrees] of [
    ["yaw", label.valid ? label.gyro_deg[2] : 0],
    ["pitch", label.valid ? label.gyro_deg[1] : 0],
  ]) {
    const end = 320 + Math.max(-1, Math.min(1, degrees / fullDeg)) * 220;
    const bar = part(name);
    bar.setAttribute("x", Math.min(320, end).toFixed(1));
    bar.setAttribute("width", Math.abs(end - 320).toFixed(1));
    const value = part(`${name}-text`);
    const sign = degrees < 0 ? "−" : "+";
    value.textContent = `${value.dataset.label} ${sign}${Math.abs(degrees).toFixed(2)}°`;
  }
}

/** Show frame n: picture, labels, position; the strip only if paused */
async function show(n) {
  const { info } = inspector;
  inspector.frame = clamp(n);
  const shown = inspector.frame;
  writeHash();
  $("i-position").textContent =
    `frame ${shown} / ${info.frames - 1} · ${clock(shown / info.fps)}`;
  $("i-delay").value = inspector.delay;
  drawScrubber(shown);
  prefetch(shown);
  if (!inspector.playing) drawStrip();

  const still = () => shown === inspector.frame && info === inspector.info;
  const img = image(shown);
  let rows;
  try {
    rows = await Promise.all(
      Array.from({ length: 2 * RADIUS + 1 }, (_, i) => shown - RADIUS + i)
        .filter((k) => k >= 0 && k < info.frames)
        .map(labels),
    );
  } catch (error) {
    $("i-rows").innerHTML =
      `<tr><td colspan="6" class="level-critical">${escapeHtml(error.message)}</td></tr>`;
    return;
  }
  if ((await img.loaded) && still()) {
    context.drawImage(img, 0, 0, canvas.width, canvas.height);
  }
  if (!still()) return;
  drawTable(rows);
  const current = rows.find(([truth]) => truth.frame === shown);
  if (current) drawLabel(current[0]);
}

function drawStrip() {
  const strip = $("i-strip");
  const { frame, info } = inspector;
  strip.replaceChildren();
  for (let n = frame - RADIUS; n <= frame + RADIUS; n++) {
    const figure = document.createElement("figure");
    if (n >= 0 && n < info.frames) {
      figure.innerHTML = `<img src="${frameUrl(n)}" alt="frame ${n}"><figcaption id="i-cap-${n}">${n}</figcaption>`;
      figure.onclick = () => go(n);
    }
    if (n === frame) figure.className = "current";
    strip.append(figure);
  }
}

function drawTable(rows) {
  const body = $("i-rows");
  body.replaceChildren();
  for (const [truth, pred] of rows) {
    const caption = $(`i-cap-${truth.frame}`);
    if (caption) caption.innerHTML = `${truth.frame}: ${summaryText(truth)}`;
    const tr = document.createElement("tr");
    if (truth.frame === inspector.frame) tr.className = "current";
    tr.onclick = () => go(truth.frame);
    const number = document.createElement("td");
    number.textContent = truth.frame;
    tr.append(
      number,
      cell("valid", truth, pred, (v) => (v == null ? "" : v ? "yes" : "no")),
      cell("buttons", truth, pred, (v) => (v ? v.join(" ") || "–" : "")),
      cell("left_stick", truth, pred, stick),
      cell("right_stick", truth, pred, stick),
      cell("gyro_deg", truth, pred, gyro),
    );
    body.append(tr);
  }
  $("i-pred-note").hidden = !rows.some(([, pred]) => pred !== undefined);
}

/** Jump to frame n, pausing playback */
function go(n) {
  if (!inspector.info) return;
  pause();
  show(n);
}

// ---------------------------------------------------------------- playback

function play() {
  const { info } = inspector;
  if (inspector.playing || !info || inspector.frame >= info.frames - 1) return;
  inspector.playing = true;
  $("i-play").textContent = "❚❚ Pause";
  if (info.sound && inspector.sound) return playWithSound(info);
  let due = performance.now();
  const step = async () => {
    if (!inspector.playing || info !== inspector.info) return;
    if (inspector.frame >= info.frames - 1) return toggle();
    const next = inspector.frame + 1;
    // Wait for the frame rather than skip it: timing stays checkable
    await image(next).loaded;
    if (!inspector.playing || info !== inspector.info) return;
    show(next);
    const interval = 1000 / (info.fps * parseFloat($("i-speed").value));
    due = Math.max(due + interval, performance.now() - interval);
    setTimeout(step, Math.max(0, due - performance.now()));
  };
  step();
}

/**
 * Play the segment's sound from the current frame and let its clock set
 * the frame: frame n is at n / fps in the file, sound included. Frames not
 * decoded in time are skipped, so picture and sound stay together.
 */
async function playWithSound(info) {
  const source = api("audio");
  if (audio.dataset.source !== source) {
    audio.dataset.source = source;
    audio.src = source;
  }
  audio.playbackRate = parseFloat($("i-speed").value);
  audio.preservesPitch = true;
  audio.currentTime = inspector.frame / info.fps;
  try {
    await audio.play();
  } catch (error) {
    // No sound after all (autoplay refused, decoding failed): play silently
    console.warn("Inspector sound:", error);
    inspector.sound = false;
    markSound();
    inspector.playing = false;
    return play();
  }
  const follow = () => {
    if (!inspector.playing || info !== inspector.info) return;
    if (audio.ended) return toggle();
    const n = Math.min(
      info.frames - 1,
      Math.floor(audio.currentTime * info.fps + 1e-6),
    );
    if (n !== inspector.frame) show(n);
    requestAnimationFrame(follow);
  };
  requestAnimationFrame(follow);
}

function pause() {
  if (!inspector.playing) return;
  inspector.playing = false;
  audio.pause();
  $("i-play").textContent = "▶ Play";
}

/** Show whether sound is on, for segments that have it */
function markSound() {
  const button = $("i-sound");
  button.hidden = !inspector.info?.sound;
  button.setAttribute("aria-pressed", String(inspector.sound));
}

function toggle() {
  if (!inspector.playing) return play();
  pause();
  show(inspector.frame);
}

// ---------------------------------------------------------------- scrubber

(() => {
  const scrubber = $("i-scrubber");
  const bubble = $("i-scrub-bubble");
  let timer = null;
  let dragging = false;
  const frameAt = (event) => {
    const box = scrubber.getBoundingClientRect();
    const x = (event.clientX - box.left) / box.width;
    return clamp(
      Math.round(Math.max(0, Math.min(1, x)) * (inspector.info.frames - 1)),
    );
  };
  const preview = (n) => {
    const { frames, fps } = inspector.info;
    drawScrubber(n);
    bubble.hidden = false;
    bubble.style.left = `${(100 * n) / Math.max(1, frames - 1)}%`;
    bubble.textContent = `${n} · ${clock(n / fps)}`;
  };
  scrubber.addEventListener("pointerdown", (event) => {
    if (!inspector.info) return;
    dragging = true;
    scrubber.setPointerCapture(event.pointerId);
    pause();
    preview(frameAt(event));
  });
  scrubber.addEventListener("pointermove", (event) => {
    if (!dragging) return;
    const n = frameAt(event);
    preview(n);
    clearTimeout(timer);
    timer = setTimeout(() => show(n), SCRUB_DEBOUNCE_MS);
  });
  scrubber.addEventListener("pointerup", (event) => {
    if (!dragging) return;
    dragging = false;
    clearTimeout(timer);
    bubble.hidden = true;
    show(frameAt(event));
  });
})();

// ----------------------------------------------------------------- actions

async function random(active) {
  if (!inspector.info) return;
  const url = api("random", {
    delay: inspector.delay,
    active: active ? 1 : 0,
  });
  go((await (await fetch(url)).json()).frame);
}

/** Ask for a frame number or a time (12.5s, 1:02.5) and go there */
function goTo() {
  if (!inspector.info) return;
  const text = prompt("Frame number, or time as 12.5s or 1:02.5");
  if (!text) return;
  const value = text.trim();
  if (/^\d+$/.test(value)) return go(parseInt(value));
  const parts = value.replace(/s$/, "").split(":").map(parseFloat);
  if (parts.some(isNaN)) return;
  const seconds = parts.reduce((total, part) => total * 60 + part, 0);
  go(Math.round(seconds * inspector.info.fps));
}

// Keys act only while the Inkspector shows a segment
document.addEventListener("keydown", (event) => {
  const tag = event.target.tagName;
  if (!inspector.shown || !inspector.info) return;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  const step = event.shiftKey ? 10 : 1;
  if (event.key === " ") toggle();
  else if (event.key === "ArrowLeft") go(inspector.frame - step);
  else if (event.key === "ArrowRight") go(inspector.frame + step);
  else if (event.key.toLowerCase() === "r") random(!event.shiftKey);
  else if (event.key.toLowerCase() === "g") goTo();
  else return;
  event.preventDefault();
});

$("i-delay").addEventListener("change", () => {
  inspector.delay = parseFloat($("i-delay").value) || 0;
  resetLabels();
  go(inspector.frame);
});
$("i-pred").addEventListener("change", () => {
  inspector.pred = $("i-pred").value.trim();
  resetLabels();
  go(inspector.frame);
});
$("i-play").onclick = toggle;
$("i-random-active").onclick = () => random(true);
$("i-random-any").onclick = () => random(false);
$("i-goto").onclick = goTo;
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

// The video's input overlay, copied over the inspected frame
inspector.hud = $("input-hud").cloneNode(true);
inspector.hud.removeAttribute("id");
inspector.hud.dataset.keys = "";
$("i-screen").append(inspector.hud);
inspector.full = buildFullOverlay();
$("i-screen").append(inspector.full);

$("i-overlay").value = inspector.overlay;
$("i-overlay").addEventListener("change", (event) => {
  inspector.overlay = event.target.value;
  remember("overlay", inspector.overlay);
  if (inspector.info) show(inspector.frame);
});
$("i-sound").addEventListener("click", () => {
  inspector.sound = !inspector.sound;
  remember("sound", String(inspector.sound));
  markSound();
  // Restart playback on the other clock
  if (inspector.playing) {
    pause();
    play();
  }
});
$("i-speed").addEventListener("change", () => {
  audio.playbackRate = parseFloat($("i-speed").value);
});

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
    play();
  }
}

window.addEventListener("app-route", (event) => {
  const { app, state } = event.detail;
  if (inspector.shown && app !== "inspect") {
    // Leaving: remember whether it was playing, to resume on return
    inspector.resume = inspector.playing;
    pause();
  }
  inspector.shown = app === "inspect";
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
