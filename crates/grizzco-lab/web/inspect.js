// Inkspector app: pick a recorded session, then check its controller labels
// against the video frame by frame in the shared player (player.js: exact
// frames from the lab, overlays, neighbours, keys, labels table). Runs
// next to app.js and stages.js and uses their helpers ($, root, appUrl,
// StageMap), and techniques.js's for the session's technique markers (bands
// on the scrubber, labelled chips under it, a list to jump to and edit them
// in the Session panel); its state lives in
// the address as /inspect/<session>?seg=<file>&n=<frame>&delay=<ms>&pred=<path>
// (and label=1 to open in the labeling mode).

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
  /** The session's technique markers (session.json), host Unix ms */
  markers: [],
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
    writeUrl();
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

/** The state of a view: a segment at a frame, delay and predictions */
function viewState(session, segment, extra = {}) {
  const state = { s: session, seg: segment, ...extra };
  return Object.fromEntries(
    Object.entries(state).filter(([, value]) => value !== "" && value != null),
  );
}

/** The path of a view, see viewState */
const viewUrl = (...view) => appUrl("inspect", viewState(...view));

/** Keep the view in the address, replacing the current history entry */
function writeUrl() {
  const { info, frame, delay, pred } = inspector;
  if (!info) return;
  const state = viewState(info.session, info.segment, {
    n: frame,
    delay,
    pred,
  });
  rememberView(replaceRoute("inspect", state));
}

/** Where the Inkspector's app link leads: back to this view, even after a reload */
function rememberView(url) {
  document.querySelector('.app-nav [data-app="inspect"]').href = url;
  remember("view", url);
}

// ------------------------------------------------------------------ picker

/** List the sessions afresh (sessions get recorded while the page is open) */
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
  const chosen = select.value;
  select.replaceChildren(
    new Option("All sessions", ""),
    ...data.sessions.map((s) => new Option(s.name, s.name)),
  );
  select.value = chosen;
  $("i-sessions-note").textContent = `${data.sessions.length} in ${data.root}`;
  const body = $("i-sessions");
  body.replaceChildren();
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
      navigate(viewUrl(summary.name, seg));
    };
    body.append(tr);
  }
}

function showPicker() {
  framePlayer.close();
  inspector.resume = false;
  rememberView("/inspect");
  inspector.info = null;
  $("inspect-viewer").hidden = true;
  $("inspect-picker").hidden = false;
  $("i-delay-chip").hidden = true;
  $("i-session").value = "";
}

// ------------------------------------------------------------------ viewer

/** Load a segment's info and show it at the address's frame and delay */
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
  inspector.markers = [];
  drawMarkers();
  loadMarkers();
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
  drawStage(`${info.session}/${info.segment}`);
}

/** Links to Gungee's maps of the segment's stage, when a Cuttlefish review
 * of it names the stage */
const sessionStage = new StageMap($("i-stage"));
async function drawStage(ref) {
  if (sessionStage.ref === ref) return;
  sessionStage.ref = ref;
  sessionStage.set("");
  const stage = await stageOfVideo({ kind: "session", ref });
  if (sessionStage.ref === ref) sessionStage.set(stage);
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

/** Hand the labeled frames (label.js's list) and the technique markers to
 * the player's scrubber */
function drawMarks() {
  const { user, model, range } = inspector.marks;
  const spans = markerSpans();
  framePlayer.setMarks({
    ticks: [
      ...user.map((n) => ({ n, kind: "user" })),
      ...model.map((n) => ({ n, kind: "model", short: true })),
    ],
    ranges: [
      ...spans.map(({ a, b }) => ({ a, b, kind: "technique" })),
      ...(range ? [{ a: range[0], b: range[1], kind: "range" }] : []),
    ],
  });
}

// ----------------------------------------------------------------- markers

/** Frame of the segment showing the input at host time `ms`, at the delay
 * in use (the labels' alignment: frame n shows the input from
 * start + n / fps - delay) */
function frameOfMs(ms) {
  const { info, delay } = inspector;
  return Math.round(((ms - info.start_unix_ms + delay) * info.fps) / 1000);
}

/** Host time of the input frame n shows, the reverse of frameOfMs */
function msOfFrame(n) {
  const { info, delay } = inspector;
  return Math.round(info.start_unix_ms + (n * 1000) / info.fps - delay);
}

/** The markers in this segment as frames [a, b] (clipped), with their index */
function markerSpans() {
  const { info, markers } = inspector;
  if (!info?.start_unix_ms) return [];
  const last = info.frames - 1;
  return markers
    .map((marker, i) => ({
      marker,
      i,
      a: frameOfMs(marker.t_start_ms),
      b: frameOfMs(marker.t_end_ms),
    }))
    .filter(({ a, b }) => b >= 0 && a <= last)
    .map((span) => ({
      ...span,
      a: Math.max(0, span.a),
      b: Math.min(last, span.b),
    }))
    .sort((x, y) => x.a - y.a || x.b - y.b);
}

/** A marker's technique as the lists have it, or itself when not listed */
function markerTechnique(marker) {
  return techniques().find((tech) => markerOf(marker, tech)) ?? marker;
}

async function loadMarkers() {
  const { info } = inspector;
  const response = await fetch(
    `/api/inspect/markers?${new URLSearchParams({ s: info.session })}`,
  );
  const data = await response.json();
  if (inspector.info !== info) return;
  if (!response.ok) {
    $("i-markers-note").textContent = data.error;
    return;
  }
  inspector.markers = data.markers;
  drawMarkers();
}

/** Save the session's markers, then show them as saved */
async function saveMarkers(markers) {
  const { info } = inspector;
  const response = await fetch("/api/inspect/markers", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ s: info.session, markers }),
  });
  const data = await response.json();
  if (!response.ok) {
    alert(t("mk.saveError", { error: data.error }));
    return;
  }
  if (inspector.info !== info) return;
  inspector.markers = data.markers;
  // The Studio's checklist and the Pedia read them again
  allMarkers.at = 0;
  drawMarkers();
}

/** A copy of the markers with marker i changed by `change` */
function changedMarkers(i, change) {
  return inspector.markers.map((marker, j) =>
    j === i ? { ...marker, ...change } : marker,
  );
}

/** The markers on the scrubber, in the lane under it and in the list */
function drawMarkers() {
  drawMarks();
  const { info, markers } = inspector;
  const spans = markerSpans();
  const lane = $("i-marker-lane");
  lane.hidden = !spans.length;
  const last = Math.max(1, (info?.frames ?? 1) - 1);
  // Overlapping spans go to rows of their own: each to the first row free
  const rowEnds = [];
  lane.innerHTML = spans
    .map(({ marker, i, a, b }) => {
      let row = rowEnds.findIndex((end) => end <= a);
      if (row < 0) row = rowEnds.length;
      rowEnds[row] = b;
      const name = techniqueNames(markerTechnique(marker))[0];
      const left = (100 * a) / last;
      const width = Math.max(0.4, (100 * (b - a)) / last);
      return `<button type="button" class="marker-chip" data-go="${i}" style="left:${left}%;width:${width}%;top:${row * 20}px" title="${escapeHtml(`${name} · ${a}–${b}`)}">${escapeHtml(name)}</button>`;
    })
    .join("");
  lane.style.height = `${Math.max(1, rowEnds.length) * 20}px`;

  const outside = markers.length - spans.length;
  $("i-markers-note").textContent = markers.length
    ? `${t("mk.note")}${outside ? ` · ${t("mk.elsewhere", { n: outside })}` : ""}`
    : t("mk.none");
  const list = techniques();
  $("i-markers").innerHTML = spans
    .map(({ marker, i, a, b }) => {
      const current = itemKey(markerTechnique(marker));
      const option = (tech) => {
        const [name, alt] = techniqueNames(tech);
        const label = alt ? `${name} · ${alt}` : name;
        const key = itemKey(tech);
        return `<option value="${escapeHtml(key)}" ${key === current ? "selected" : ""}>${escapeHtml(label)}</option>`;
      };
      // The Techniques panel's groups, then the marker's own name when no
      // item of the lists is it
      const options =
        TECH_GROUPS.map((group) => {
          const items = list.filter((tech) => tech.group === group.id);
          if (!items.length) return "";
          return `<optgroup label="${escapeHtml(t(`tech.group.${group.id}`))}">${items.map(option).join("")}</optgroup>`;
        }).join("") +
        (list.some((tech) => itemKey(tech) === current) ? "" : option(marker));
      const seconds = ((b - a) / info.fps).toFixed(1);
      return `<li class="i-marker" data-i="${i}">
        <select class="select" data-field="label" aria-label="${escapeHtml(t("mk.technique"))}">${options}</select>
        <span class="i-marker-frames">
          <input class="select num" type="number" min="0" max="${info.frames - 1}" value="${a}" data-field="a" aria-label="${escapeHtml(t("mk.setStart"))}" />–<input class="select num" type="number" min="0" max="${info.frames - 1}" value="${b}" data-field="b" aria-label="${escapeHtml(t("mk.setEnd"))}" />
          <span class="panel-note">${seconds} s</span>
        </span>
        <span class="i-marker-actions">
          <button type="button" class="mode-toggle" data-act="go">${escapeHtml(t("mk.go"))}</button>
          <button type="button" class="mode-toggle" data-act="start">${escapeHtml(t("mk.setStart"))}</button>
          <button type="button" class="mode-toggle" data-act="end">${escapeHtml(t("mk.setEnd"))}</button>
          <button type="button" class="mode-toggle" data-act="delete">${escapeHtml(t("mk.delete"))}</button>
        </span>
      </li>`;
    })
    .join("");
}

$("i-marker-lane").addEventListener("click", (event) => {
  const chip = event.target.closest("[data-go]");
  if (chip)
    go(frameOfMs(inspector.markers[Number(chip.dataset.go)].t_start_ms));
});

$("i-markers").addEventListener("click", (event) => {
  const button = event.target.closest("[data-act]");
  if (!button) return;
  const i = Number(button.closest("[data-i]").dataset.i);
  const marker = inspector.markers[i];
  const here = msOfFrame(inspector.frame);
  const act = button.dataset.act;
  if (act === "go") go(Math.max(0, frameOfMs(marker.t_start_ms)));
  else if (act === "start")
    saveMarkers(
      changedMarkers(i, {
        t_start_ms: here,
        t_end_ms: Math.max(here, marker.t_end_ms),
      }),
    );
  else if (act === "end")
    saveMarkers(
      changedMarkers(i, {
        t_end_ms: here,
        t_start_ms: Math.min(here, marker.t_start_ms),
      }),
    );
  else if (
    act === "delete" &&
    confirm(t("mk.deleteAsk", { name: marker.label }))
  )
    saveMarkers(inspector.markers.filter((_, j) => j !== i));
});

$("i-markers").addEventListener("change", (event) => {
  const field = event.target.dataset.field;
  if (!field) return;
  const i = Number(event.target.closest("[data-i]").dataset.i);
  const marker = inspector.markers[i];
  if (field === "label") {
    const tech = techniques().find((x) => itemKey(x) === event.target.value);
    if (!tech) return;
    saveMarkers(
      changedMarkers(i, {
        kind: itemKind(tech),
        label: tech.label,
        term: tech.term ?? null,
        item: tech.id ?? null,
      }),
    );
    return;
  }
  const n = parseInt(event.target.value);
  if (!Number.isFinite(n)) return drawMarkers();
  const ms = msOfFrame(n);
  saveMarkers(
    changedMarkers(
      i,
      field === "a"
        ? { t_start_ms: ms, t_end_ms: Math.max(ms, marker.t_end_ms) }
        : { t_end_ms: ms, t_start_ms: Math.min(ms, marker.t_start_ms) },
    ),
  );
});

// A marker added after the fact: the Studio's technique picked, 2 s from
// the frame on screen
$("i-marker-add").addEventListener("click", () => {
  const { info, frame } = inspector;
  if (!info?.start_unix_ms) return;
  const tech = pickedTechnique();
  const end = Math.min(info.frames - 1, frame + Math.round(2 * info.fps));
  saveMarkers([
    ...inspector.markers,
    {
      kind: itemKind(tech),
      label: tech.label,
      term: tech.term ?? null,
      item: tech.id ?? null,
      t_start_ms: msOfFrame(frame),
      t_end_ms: msOfFrame(end),
      created_ms: Date.now(),
    },
  ]);
});
// Redrawn in the page's language, and with Lean's weapons and specials
// once techniques.js has them
for (const name of ["lang-change", "tech-items"]) {
  window.addEventListener(name, () => {
    if (inspector.info) drawMarkers();
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
  drawMarkers();
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
  navigate(name ? viewUrl(name, summary?.segments[0]?.file) : "/inspect");
};
$("i-segment").onchange = (event) => {
  navigate(viewUrl(inspector.info.session, event.target.value));
};
$("i-stick-tol").textContent = STICK_TOLERANCE;
$("i-gyro-tol").textContent = GYRO_TOLERANCE;

/** Show what the address names: a segment, or the picker */
async function routeInspector(state) {
  const session = state.get("s");
  // The picker, or a session not listed yet, lists them again
  if (!session || !inspector.sessions?.some((s) => s.name === session)) {
    await loadSessions();
  }
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
    drawMarkers();
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
$("i-back").onclick = () => navigate("/inspect");
// The last view, for the app link after a reload
document.querySelector('.app-nav [data-app="inspect"]').href = storedView(
  "inspect",
  remembered("view"),
);
