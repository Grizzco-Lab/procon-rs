// ProCon Studio dashboard: live controller view, recording controls, stats
"use strict";

const $ = (id) => document.getElementById(id);

// ---------------------------------------------------------------- formatting

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 1536 -> "1.5 KB" (decimal units, like `df -H`) */
function formatBytes(bytes) {
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < UNITS.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** 83000 -> "00:01:23" */
function formatClock(ms) {
  const total = Math.floor(ms / 1000);
  const pad = (n) => String(n).padStart(2, "0");
  return `${pad(Math.floor(total / 3600))}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}

/** Rough duration for "time left" notes */
function formatSpan(seconds) {
  if (seconds >= 2 * 86400) return `${Math.round(seconds / 86400)} days`;
  if (seconds >= 2 * 3600) return `${Math.round(seconds / 3600)} hours`;
  return `${Math.max(1, Math.round(seconds / 60))} min`;
}

/** Signed integer with a real minus sign, e.g. "−12" */
const signed = (n) => (n < 0 ? `−${Math.abs(n)}` : String(n));

// ------------------------------------------------------ style proposal picker

// Style and layout are remembered per browser, so a phone and a desktop can differ
const root = document.documentElement;

function setView(key, value) {
  root.dataset[key] = value;
  try {
    localStorage.setItem(`procon-${key}`, value);
  } catch {
    // Private windows may refuse storage; the choice still applies until reload
  }
  markView();
}

function markView() {
  for (const button of document.querySelectorAll("[data-pick]")) {
    button.setAttribute(
      "aria-pressed",
      String(button.dataset.pick === root.dataset.theme),
    );
  }
  $("pick-phone").setAttribute(
    "aria-pressed",
    String(root.dataset.layout === "phone"),
  );
}

for (const button of document.querySelectorAll("[data-pick]")) {
  button.addEventListener("click", () => setView("theme", button.dataset.pick));
}
$("pick-phone").addEventListener("click", () =>
  setView("layout", root.dataset.layout === "phone" ? "auto" : "phone"),
);
markView();

// ----------------------------------------------------------- controller view

const procon = $("procon");
const buttonEls = new Map();
for (const el of procon.querySelectorAll("[data-btn]")) {
  const list = buttonEls.get(el.dataset.btn) ?? [];
  list.push(el);
  buttonEls.set(el.dataset.btn, list);
}

/** Raw 12-bit stick axis to -100..100, same scale as the console dumper */
const stickPercent = (raw) =>
  Math.max(-100, Math.min(100, Math.round(((raw - 2048) / 2048) * 100)));

/** Uncalibrated gyro raw units to degrees per second */
const GYRO_DPS = 0.07;

/** Max stick cap travel inside its well, in SVG units */
const STICK_TRAVEL = 24;

const BATTERY = ["Empty", "Critical", "Low", "Medium", "Full"];

// Controller view options, remembered per browser
const view = { splatoon: false, gain: 0, threeD: true };
try {
  view.splatoon = localStorage.getItem("procon-splatoon") === "true";
  view.gain = Number(localStorage.getItem("procon-gain") ?? 0) || 0;
  view.threeD = localStorage.getItem("procon-3d") !== "false";
} catch {
  // Storage may be unavailable; use the defaults
}

function saveView() {
  try {
    localStorage.setItem("procon-splatoon", String(view.splatoon));
    localStorage.setItem("procon-gain", String(view.gain));
    localStorage.setItem("procon-3d", String(view.threeD));
  } catch {
    // Keep the choice for this page only
  }
}

function markControllerView() {
  $("splatoon-toggle").setAttribute("aria-pressed", String(view.splatoon));
  $("splatoon-hint").hidden = !view.splatoon;
  $("gain-control").hidden = !view.splatoon;
  $("splatoon-gain").value = String(view.gain);
  $("gain-value").textContent = signed(view.gain);
  // 3D needs three.js from the CDN; the SVG stays as the fallback
  const threeD = view.threeD && Boolean(window.procon3d);
  $("view-3d").setAttribute("aria-pressed", String(threeD));
  $("view-3d").disabled = !window.procon3d;
  $("procon-3d").hidden = !threeD;
  procon.classList.toggle("is-behind", threeD);
}

$("splatoon-toggle").addEventListener("click", () => {
  view.splatoon = !view.splatoon;
  saveView();
  markControllerView();
});
$("splatoon-gain").addEventListener("input", (event) => {
  view.gain = Number(event.target.value);
  saveView();
  markControllerView();
});
$("view-3d").addEventListener("click", () => {
  view.threeD = !view.threeD;
  saveView();
  markControllerView();
});
window.addEventListener("procon3d-ready", markControllerView);
markControllerView();

/** Hamilton product of (w, x, y, z) quaternions */
function qmul([aw, ax, ay, az], [bw, bx, by, bz]) {
  return [
    aw * bw - ax * bx - ay * by - az * bz,
    aw * bx + ax * bw + ay * bz - az * by,
    aw * by - ax * bz + ay * bw + az * bx,
    aw * bz + ax * by - ay * bx + az * bw,
  ];
}

/** Rotation of `degrees` about a unit axis */
function qaxis(axis, degrees) {
  const half = (degrees * Math.PI) / 360;
  const s = Math.sin(half);
  return [Math.cos(half), axis[0] * s, axis[1] * s, axis[2] * s];
}

/** Same axis, angle multiplied by `factor` */
function qscale([w, x, y, z], factor) {
  const s = Math.sqrt(Math.max(0, 1 - w * w));
  if (s < 1e-6) return [1, 0, 0, 0];
  const angle = 2 * Math.acos(Math.min(1, Math.max(-1, w))) * factor;
  return qaxis([x / s, y / s, z / s], (angle * 180) / Math.PI);
}

/** CSS rotation for a (w, x, y, z) quaternion */
function rotation([w, x, y, z]) {
  const s = Math.sqrt(Math.max(0, 1 - w * w));
  if (s < 1e-6) return "none";
  const angle = 2 * Math.acos(Math.min(1, Math.max(-1, w)));
  return `rotate3d(${x / s}, ${y / s}, ${z / s}, ${angle}rad)`;
}

/**
 * How the controller view is turned, in the CSS frame
 *
 * Splatoon mode shows the tracked pose, scaled by the sensitivity setting
 * (each step is 2^(1/2.5), so -5..+5 is 1/4x..4x). Otherwise the view tilts
 * with angular velocity, as tuned on real hardware.
 */
function viewRotation(gyro, orientation) {
  if (view.splatoon && orientation) {
    return qscale(orientation, 2 ** (view.gain / 2.5));
  }
  const scale = 0.05;
  return qmul(
    qmul(
      qaxis([1, 0, 0], gyro.gyro_y * scale),
      qaxis([0, 1, 0], gyro.gyro_x * scale),
    ),
    qaxis([0, 0, 1], -gyro.gyro_z * scale),
  );
}

function renderState(state, orientation) {
  for (const [name, els] of buttonEls) {
    const on = Boolean(state.buttons[name]);
    for (const el of els) el.classList.toggle("on", on);
  }

  for (const [side, stick] of [
    ["l", state.left_stick],
    ["r", state.right_stick],
  ]) {
    const x = stickPercent(stick.x);
    const y = stickPercent(stick.y);
    const dx = (x / 100) * STICK_TRAVEL;
    const dy = (-y / 100) * STICK_TRAVEL;
    $(`pc-stick-${side}`).setAttribute(
      "transform",
      `translate(${dx.toFixed(1)} ${dy.toFixed(1)})`,
    );
    $(`stick-${side}`).textContent = `x ${signed(x)} · y ${signed(y)}`;
  }

  const gyro = state.gyro[0] ?? { gyro_x: 0, gyro_y: 0, gyro_z: 0 };
  const turn = viewRotation(gyro, orientation);
  procon.style.transform = rotation(turn);
  if (!$("procon-3d").hidden) window.procon3d.update(state, turn);
  $("gyro-x").textContent = signed(Math.round(gyro.gyro_x * GYRO_DPS));
  $("gyro-y").textContent = signed(Math.round(gyro.gyro_y * GYRO_DPS));
  $("gyro-z").textContent = signed(Math.round(gyro.gyro_z * GYRO_DPS));

  // Upper bits are the level in steps of two, the lowest bit means charging
  const level = Math.min(4, state.battery_level >> 1);
  const charging = (state.battery_level & 1) === 1;
  const battery = $("battery");
  battery.dataset.level = String(level);
  battery.querySelector(".battery-text").textContent =
    BATTERY[level] + (charging ? " · charging" : "");
}

// ---------------------------------------------------------------- gyro chart

const chart = {
  svg: $("gyro-chart"),
  tip: $("gyro-tip"),
  windowMs: 5000,
  /** Samples of { t, v: [x, y, z] } in °/s, oldest first */
  samples: [],
  /** Pointer x in pixels while hovering, or null */
  hoverX: null,
};

const SVG_NS = "http://www.w3.org/2000/svg";
const svgEl = (tag, attrs) => {
  const el = document.createElementNS(SVG_NS, tag);
  for (const [key, value] of Object.entries(attrs)) el.setAttribute(key, value);
  return el;
};

// Static parts: gridlines, axis labels, one line per axis, crosshair
chart.grid = [0, 1, 2].map(() =>
  chart.svg.appendChild(svgEl("line", { class: "grid" })),
);
chart.ticks = [0, 1, 2].map(() =>
  chart.svg.appendChild(svgEl("text", { class: "tick" })),
);
chart.lines = [1, 2, 3].map((n) =>
  chart.svg.appendChild(svgEl("polyline", { class: `series s${n}` })),
);
chart.cross = chart.svg.appendChild(svgEl("line", { class: "crosshair" }));
chart.dots = [1, 2, 3].map((n) =>
  chart.svg.appendChild(svgEl("circle", { class: `dot s${n}`, r: 4 })),
);

chart.svg.addEventListener("pointermove", (event) => {
  chart.hoverX = event.clientX - chart.svg.getBoundingClientRect().left;
});
chart.svg.addEventListener("pointerleave", () => {
  chart.hoverX = null;
});

function pushGyro(now, gyro) {
  const last = chart.samples[chart.samples.length - 1];
  // One sample per display frame is plenty
  if (last && now - last.t < 15) return;
  chart.samples.push({
    t: now,
    v: [gyro.gyro_x * GYRO_DPS, gyro.gyro_y * GYRO_DPS, gyro.gyro_z * GYRO_DPS],
  });
}

/** Round a range up to 1, 2 or 5 times a power of ten */
function niceCeil(value) {
  const power = 10 ** Math.floor(Math.log10(value));
  return [1, 2, 5, 10].map((m) => m * power).find((step) => step >= value);
}

function drawChart(now) {
  const { svg, samples } = chart;
  const width = svg.clientWidth;
  const height = svg.clientHeight;
  if (!width || !height) return;
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);

  while (samples.length && samples[0].t < now - chart.windowMs - 100)
    samples.shift();

  const labelWidth = 36;
  const plotWidth = width - labelWidth;
  const pad = 8;
  let peak = 50;
  for (const sample of samples)
    for (const v of sample.v) peak = Math.max(peak, Math.abs(v));
  const range = niceCeil(peak);
  const xOf = (t) =>
    labelWidth + ((t - (now - chart.windowMs)) / chart.windowMs) * plotWidth;
  const yOf = (v) => pad + ((range - v) / (2 * range)) * (height - 2 * pad);

  [range, 0, -range].forEach((value, i) => {
    const y = yOf(value).toFixed(1);
    chart.grid[i].setAttribute("x1", labelWidth);
    chart.grid[i].setAttribute("x2", width);
    chart.grid[i].setAttribute("y1", y);
    chart.grid[i].setAttribute("y2", y);
    chart.grid[i].classList.toggle("baseline", value === 0);
    chart.ticks[i].setAttribute("x", labelWidth - 6);
    chart.ticks[i].setAttribute("y", y);
    chart.ticks[i].textContent = signed(value);
  });

  chart.lines.forEach((line, axis) => {
    let points = "";
    for (const sample of samples) {
      points += `${xOf(sample.t).toFixed(1)},${yOf(sample.v[axis]).toFixed(1)} `;
    }
    line.setAttribute("points", points);
  });

  // Crosshair snaps to the sample nearest the pointer
  let hovered = null;
  if (chart.hoverX !== null && chart.hoverX > labelWidth && samples.length) {
    const t =
      now -
      chart.windowMs +
      ((chart.hoverX - labelWidth) / plotWidth) * chart.windowMs;
    hovered = samples.reduce((best, s) =>
      Math.abs(s.t - t) < Math.abs(best.t - t) ? s : best,
    );
  }
  chart.cross.style.display = hovered ? "" : "none";
  chart.dots.forEach((dot) => (dot.style.display = hovered ? "" : "none"));
  chart.tip.hidden = !hovered;
  if (hovered) {
    const x = xOf(hovered.t);
    chart.cross.setAttribute("x1", x);
    chart.cross.setAttribute("x2", x);
    chart.cross.setAttribute("y1", 0);
    chart.cross.setAttribute("y2", height);
    chart.dots.forEach((dot, axis) => {
      dot.setAttribute("cx", x);
      dot.setAttribute("cy", yOf(hovered.v[axis]));
    });
    const ago = ((now - hovered.t) / 1000).toFixed(1);
    chart.tip.replaceChildren(
      ...["X", "Y", "Z"].map((name, axis) => {
        const row = document.createElement("div");
        row.className = `tip-row s${axis + 1}`;
        const value = document.createElement("b");
        value.textContent = `${signed(Math.round(hovered.v[axis]))}°/s`;
        row.append(document.createElement("i"), value, ` ${name}`);
        return row;
      }),
      Object.assign(document.createElement("div"), {
        className: "tip-note",
        textContent: `${ago} s ago`,
      }),
    );
    const left = Math.min(x + 12, width - chart.tip.offsetWidth - 4);
    chart.tip.style.left = `${Math.max(labelWidth, left)}px`;
  }
}

// ------------------------------------------------------------------ recorder

const recorder = {
  state: "idle",
  /** Elapsed ms reported by the server and when it arrived, for a smooth clock */
  elapsedMs: 0,
  receivedAt: 0,
  busy: false,
};

const REC_LABEL = { idle: "Idle", recording: "Recording", paused: "Paused" };

function renderRecorder(status) {
  recorder.state = status.state;
  recorder.elapsedMs = status.elapsed_ms;
  recorder.receivedAt = performance.now();

  const badge = $("rec-badge");
  badge.dataset.state = status.state;
  badge.textContent = REC_LABEL[status.state];
  document.body.dataset.rec = status.state;

  setButtons();
  $("btn-pause-text").textContent =
    status.state === "paused" ? "Resume" : "Pause";

  // Keep what the user typed until it is applied; a running session shows its real prefix
  const active = status.state !== "idle";
  const input = $("dir-input");
  if (active) delete input.dataset.edited;
  if (!input.dataset.edited) input.value = status.prefix;

  // While idle, show the folder the next session gets
  const shown = active ? status.session : status.next;
  $("rec-file-label").textContent = active ? "Session folder" : "Next session";
  $("rec-file").textContent = `${shown}/`;
  $("rec-file").title = shown;

  if (status.error) showError(status.error);
  updateClock(performance.now());
}

function updateClock(now) {
  let elapsed = recorder.elapsedMs;
  if (recorder.state === "recording") elapsed += now - recorder.receivedAt;
  const text = formatClock(elapsed);
  const clock = $("rec-clock");
  if (clock.textContent !== text) clock.textContent = text;

  const chip = $("chip-rec");
  chip.dataset.state = recorder.state;
  const chipText =
    recorder.state === "idle"
      ? "Not recording"
      : `${recorder.state === "paused" ? "Paused" : "REC"} ${text}`;
  const chipLabel = chip.querySelector(".chip-text");
  if (chipLabel.textContent !== chipText) chipLabel.textContent = chipText;
}

/** Enable only the actions that make sense in the current state */
function setButtons() {
  const active = recorder.state !== "idle";
  $("btn-record").disabled = recorder.busy || active;
  $("btn-pause").disabled = recorder.busy || !active;
  $("btn-stop").disabled = recorder.busy || !active;
  // The prefix and video input can only change between sessions
  const form = $("dir-form");
  form.elements.prefix.disabled = active;
  form.querySelector("button").disabled = recorder.busy || active;
  $("video-input").disabled = recorder.busy || active;
  $("video-height").disabled = recorder.busy || active;
  $("video-fps").disabled = recorder.busy || active;
}

function showError(message) {
  const notice = $("rec-error");
  notice.hidden = !message;
  notice.textContent = message ?? "";
}

async function sendCommand(body) {
  recorder.busy = true;
  setButtons();
  try {
    const response = await fetch("/api/command", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const text = await response.text();
    let reply;
    try {
      reply = JSON.parse(text);
    } catch {
      reply = { error: text || `HTTP ${response.status}` };
    }
    if (!response.ok) throw new Error(reply.error ?? `HTTP ${response.status}`);
    showError(null);
    renderRecorder(reply.recorder);
    return true;
  } catch (error) {
    showError(error.message);
    return false;
  } finally {
    recorder.busy = false;
    setButtons();
  }
}

$("btn-record").addEventListener("click", () =>
  sendCommand({ action: "start" }),
);
$("btn-pause").addEventListener("click", () =>
  sendCommand({ action: recorder.state === "paused" ? "resume" : "pause" }),
);
$("btn-stop").addEventListener("click", () => sendCommand({ action: "stop" }));
$("dir-input").addEventListener("input", (event) => {
  event.target.dataset.edited = "true";
});
$("dir-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const input = $("dir-input");
  if (await sendCommand({ action: "set_prefix", prefix: input.value })) {
    delete input.dataset.edited;
    input.blur();
  }
});

// --------------------------------------------------------------------- video

const video = {
  /** Object URL of the frame on screen, revoked when the next one arrives */
  url: null,
  live: false,
};

$("video-input").addEventListener("change", (event) =>
  sendCommand({ action: "set_video_input", input: event.target.value }),
);
// Smaller or slower recordings save disk; they apply from the next recording
for (const id of ["video-height", "video-fps"]) {
  $(id).addEventListener("change", () =>
    sendCommand({
      action: "set_video_quality",
      height: Number($("video-height").value),
      fps: Number($("video-fps").value),
    }),
  );
}

function showPreview(blob) {
  if (!video.live) return;
  const img = $("video-img");
  const previous = video.url;
  video.url = URL.createObjectURL(blob);
  img.src = video.url;
  if (previous) URL.revokeObjectURL(previous);
}

function renderVideo(status) {
  // Rebuild the input list only when it changes, so an open menu stays open
  const select = $("video-input");
  const options = [{ id: "", name: "No video" }, ...status.inputs];
  const key = JSON.stringify(options);
  if (select.dataset.key !== key) {
    select.dataset.key = key;
    select.replaceChildren(
      ...options.map(({ id, name }) => new Option(name, id)),
    );
  }
  select.value = status.input ?? "";

  video.live = status.live;
  $("video-img").hidden = !status.live;
  $("no-signal").hidden = status.live;
  if (!status.input) {
    $("video-title").textContent = "No video source";
    $("video-message").textContent = "Pick an input above";
  } else if (status.error) {
    $("video-title").textContent = "Video input unavailable";
    // A busy capture card is almost always OBS holding it
    $("video-message").textContent = status.error.includes("busy")
      ? "The device is busy. Close OBS or anything else using it."
      : status.error;
  } else {
    $("video-title").textContent = "Starting video…";
    $("video-message").textContent = status.input;
  }
  // Keep an open menu alone; only follow the server when not being edited
  for (const [id, value] of [
    ["video-height", status.record_height],
    ["video-fps", status.record_fps],
  ]) {
    const select = $(id);
    if (document.activeElement !== select) select.value = String(value);
  }
}

// ---------------------------------------------------------------- status tick

/** Meter severity by fraction used */
const levelOf = (used) =>
  used >= 0.95 ? "critical" : used >= 0.85 ? "warning" : "ok";

function renderMeter(id, used, text) {
  const meter = $(id);
  meter.dataset.level = levelOf(used);
  meter.querySelector(".meter-fill").style.width =
    `${(used * 100).toFixed(1)}%`;
  meter.querySelector(".meter-value").textContent = text;
}

function setChip(id, level, text, title = "") {
  const chip = $(id);
  chip.dataset.level = level;
  chip.title = title;
  chip.querySelector(".chip-text").textContent = text;
}

function renderStatus(status) {
  const link = status.link;
  const offset = `${link.clock_offset_ms >= 0 ? "+" : "−"}${Math.abs(link.clock_offset_ms)} ms`;
  setChip(
    "chip-link",
    link.connected ? "good" : "critical",
    link.connected ? "Proxy connected" : "Proxy not connected",
    `${link.address}${link.connected ? `, host clock ${offset} vs proxy` : ""}`,
  );
  const input = link.connected && link.input_rate > 0;
  setChip(
    "chip-controller",
    input ? "good" : "critical",
    input ? "Controller input" : "No controller input",
  );
  $("input-rate").textContent = input
    ? `${link.input_rate.toFixed(1)} Hz · clock ${offset}`
    : "No input";
  procon.classList.toggle("is-idle", !input);
  $("procon-3d").classList.toggle("is-idle", !input);

  renderRecorder(status.recorder);
  renderVideo(status.video);
  // Rates per second and per hour, next to what has been written so far
  const { rates, sizes } = status;
  for (const stream of ["controller", "video"]) {
    $(`rate-${stream}`).textContent = `${formatBytes(rates[stream])}/s`;
    $(`note-${stream}`).textContent =
      `${formatBytes(rates[stream] * 3600)}/h · ${formatBytes(sizes[stream])} so far`;
  }
  const writeRate = rates.controller + rates.video;
  $("size-session").textContent = formatBytes(sizes.controller + sizes.video);
  $("note-session").textContent =
    writeRate > 0
      ? `${formatBytes(writeRate * 3600)}/h in total`
      : "controller and video";
  if (sizes.all_sessions !== null) {
    $("size-all").textContent = formatBytes(sizes.all_sessions);
  }
  $("dropped-note").textContent =
    link.dropped > 0
      ? `⚠ ${link.dropped.toLocaleString("en-US")} frames dropped`
      : "No frames dropped";

  const disk = status.disk;
  if (disk.total > 0) {
    const used = 1 - disk.free / disk.total;
    renderMeter(
      "meter-disk",
      used,
      `${formatBytes(disk.free)} free of ${formatBytes(disk.total)}`,
    );
    const level = levelOf(used);
    const left =
      writeRate > 0
        ? `Room for about ${formatSpan(disk.free / writeRate)} at this rate`
        : "";
    $("disk-note").textContent =
      level === "ok" ? left : `⚠ Low disk space. ${left}`;
  }

  const memory = status.memory;
  if (memory.total > 0) {
    renderMeter(
      "meter-mem",
      1 - memory.available / memory.total,
      `${formatBytes(memory.available)} free of ${formatBytes(memory.total)}`,
    );
  }
}

// ----------------------------------------------------------------- websocket

let latestState = null;
let latestOrientation = null;

function markOffline() {
  setChip("chip-link", "off", "Dashboard offline, reconnecting…");
  setChip("chip-controller", "off", "No controller input");
  procon.classList.add("is-idle");
  $("procon-3d").classList.add("is-idle");
}

function connect() {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(`${scheme}://${location.host}/ws`);
  socket.binaryType = "blob";
  socket.onmessage = (event) => {
    // Binary messages are video preview JPEGs
    if (event.data instanceof Blob) {
      showPreview(event.data.slice(0, event.data.size, "image/jpeg"));
      return;
    }
    const message = JSON.parse(event.data);
    if (message.type === "state") {
      latestState = message.state;
      latestOrientation = message.orientation;
      if (latestState.gyro.length)
        pushGyro(performance.now(), latestState.gyro[0]);
    } else if (message.type === "status") {
      renderStatus(message);
    }
  };
  socket.onclose = () => {
    markOffline();
    setTimeout(connect, 1500);
  };
}

// Render at display rate no matter how fast reports arrive
function frame(now) {
  if (latestState) {
    renderState(latestState, latestOrientation);
    latestState = null;
  }
  drawChart(now);
  updateClock(now);
  requestAnimationFrame(frame);
}

connect();
requestAnimationFrame(frame);
