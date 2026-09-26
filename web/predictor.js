// Predictor app: run AgentZero's inverse dynamics model (IDM) on a video
// and watch what it predicts, frame by frame, next to the truth when the
// video has a controller recording. Runs after app.js and inspect.js and
// uses their helpers ($, escapeHtml, svgEl, drawInputHud, stickPercent).
// Runs go through /api/predictor (see src/predictor.rs); the video plays
// from /api/cuttlefish/video. State lives in the hash:
// #predictor/key=<video>&ckpt=<checkpoint>&t=<seconds>.
"use strict";

(() => {
  /** How often a running prediction is asked about, in ms */
  const POLL_MS = 1000;
  /** Frames fetched around the playhead for the timeline */
  const CHUNK = 1800;
  /** Button lanes, in controller order */
  const BUTTONS = [
    "zl",
    "l",
    "zr",
    "r",
    "a",
    "b",
    "x",
    "y",
    "up",
    "down",
    "left",
    "right",
    "minus",
    "plus",
    "l_stick",
    "r_stick",
    "home",
    "capture",
  ];
  /** Signal lanes: name, label, value of a label */
  const SIGNALS = [
    ["left_x", "Left stick x", (l) => stick(l.left_stick?.[0])],
    ["left_y", "Left stick y", (l) => stick(l.left_stick?.[1])],
    ["right_x", "Right stick x", (l) => stick(l.right_stick?.[0])],
    ["right_y", "Right stick y", (l) => stick(l.right_stick?.[1])],
    ["gyro_pitch", "Gyro pitch °", (l) => l.gyro_deg?.[1]],
    ["gyro_yaw", "Gyro yaw °", (l) => l.gyro_deg?.[2]],
    ["turn_x", "Camera turn x", (l) => l.camera_turn?.[0]],
    ["turn_y", "Camera turn y", (l) => l.camera_turn?.[1]],
  ];
  const STATES = {
    running: "Predicting",
    done: "Done",
    failed: "Failed",
    cancelled: "Cancelled",
  };

  const pred = {
    /** Whether the app is shown */
    shown: false,
    info: null,
    sessions: null,
    reviews: null,
    /** The current or last run */
    job: null,
    /** Stored runs, newest first */
    runs: [],
    /** The run on screen */
    run: null,
    /** Frame on screen */
    frame: 0,
    /** Loaded labels: { start, pred: [], truth: [] | null } */
    chunk: null,
    loading: null,
    /** Seconds of video the timeline shows */
    span: 10,
    pollTimer: null,
    agreeTimer: null,
    agreeAt: 0,
  };

  const video = $("p-video");
  const canvas = $("p-timeline");

  function stick(raw) {
    return raw == null ? undefined : stickPercent(raw) / 100;
  }

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-predictor-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-predictor-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  async function api(path, body) {
    const response = await fetch(
      `/api/predictor/${path}`,
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          },
    );
    const data = await response.json();
    if (!response.ok) throw new Error(data.error ?? response.statusText);
    return data;
  }

  const clockText = (s) => {
    const m = Math.floor(s / 60);
    return `${m}:${(s - 60 * m).toFixed(1).padStart(4, "0")}`;
  };

  function setChip(text, level = "off") {
    const chip = $("p-chip");
    chip.hidden = !text;
    chip.dataset.level = level;
    chip.querySelector(".chip-text").textContent = text;
  }

  // ------------------------------------------------------------ the form

  /** Folders, checkpoints and what agentzero-predict can do */
  async function loadInfo(refresh = false) {
    if (pred.info && !refresh) return;
    $("p-caps").textContent = "Checking agentzero-predict…";
    pred.info = await api(`info${refresh ? "?refresh=1" : ""}`);
    const { info } = pred;
    const select = $("p-ckpt");
    const kept = select.value || remembered("ckpt", "");
    select.replaceChildren(
      ...info.checkpoints.map(
        (c) =>
          new Option(
            `${c.name} · ${new Date(c.modified_ms).toLocaleString()}`,
            c.name,
          ),
      ),
    );
    if (!info.checkpoints.length) select.add(new Option("No checkpoints", ""));
    if (info.checkpoints.some((c) => c.name === kept)) select.value = kept;
    renderCaps();
  }

  /** The note about the command's options, and the inputs they allow */
  function renderCaps() {
    const { info } = pred;
    if (!info) return;
    const caps = info.capabilities;
    const notes = [];
    if (!info.found)
      notes.push(`No AgentZero in ${info.agentzero} ([predictor] agentzero).`);
    if (caps.error) notes.push(caps.error);
    else if (!caps.video)
      notes.push(
        "This agentzero-predict has no --video mode yet: only sessions (and reviews of a session) can run. Recheck once AgentZero has it.",
      );
    if (!caps.error && !caps.range)
      notes.push("No --start-s/--end-s yet: the whole video is predicted.");
    const caps_el = $("p-caps");
    caps_el.textContent = notes.join(" ");
    caps_el.hidden = !notes.length;
    for (const id of ["p-start", "p-end"]) $(id).disabled = !caps.range;
    $("p-cpu-wrap").hidden = !caps.cpu;
    const gpu = info.gpu;
    $("p-gpu").textContent = gpu
      ? `GPU memory ${(gpu.used_mib / 1024).toFixed(1)} of ${(gpu.total_mib / 1024).toFixed(1)} GiB in use`
      : "";
    checkForm();
  }

  async function loadSessions() {
    if (pred.sessions) return;
    try {
      const response = await fetch("/api/inspect/sessions");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      pred.sessions = data.sessions;
    } catch (error) {
      pred.sessions = [];
      $("p-session").replaceChildren(
        new Option(`No sessions: ${error.message}`, ""),
      );
      return;
    }
    $("p-session").replaceChildren(
      ...pred.sessions.map((s) => new Option(s.name, s.name)),
    );
    const kept = remembered("session", "");
    if (pred.sessions.some((s) => s.name === kept)) $("p-session").value = kept;
    fillSegments();
  }

  function fillSegments() {
    const summary = pred.sessions?.find((s) => s.name === $("p-session").value);
    const select = $("p-segment");
    select.replaceChildren(
      ...(summary?.segments ?? []).map((s) => new Option(s.file, s.file)),
    );
    select.hidden = (summary?.segments.length ?? 0) < 2;
  }

  /** A review of a session's recording, which has controller data */
  const sessionReview = (r) => r?.video.kind === "session" && !r.video.file;

  async function loadReviews() {
    if (pred.reviews) return;
    try {
      const response = await fetch("/api/cuttlefish/reviews");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      // A review started from Cuttlefish's chat may have no video yet
      pred.reviews = data.reviews.filter((r) => r.video);
    } catch (error) {
      pred.reviews = [];
      $("p-review").replaceChildren(
        new Option(`No reviews: ${error.message}`, ""),
      );
      return;
    }
    $("p-review").replaceChildren(
      ...pred.reviews.map((r) => {
        const v = r.video;
        const name =
          v.title ??
          (v.kind === "youtube" ? v.ref : v.ref.split("/").pop()) ??
          r.id;
        return new Option(`${r.id} · ${v.kind} · ${name}`, r.id);
      }),
    );
    if (!pred.reviews.length) $("p-review").add(new Option("No reviews", ""));
  }

  /** Show the inputs of the chosen kind of video */
  function showKind() {
    const kind = $("p-kind").value;
    remember("kind", kind);
    $("p-session-row").hidden = kind !== "session";
    $("p-review").hidden = kind !== "review";
    $("p-file").hidden = kind !== "file";
    if (kind === "session") loadSessions();
    if (kind === "review") loadReviews().then(checkForm);
    checkForm();
  }

  /** The source the form names */
  function source() {
    const kind = $("p-kind").value;
    if (kind === "session")
      return { kind, s: $("p-session").value, seg: $("p-segment").value };
    if (kind === "review") return { kind, id: $("p-review").value };
    return { kind, path: $("p-file").value.trim() };
  }

  /** Whether the chosen video can run with this agentzero-predict */
  function checkForm() {
    const caps = pred.info?.capabilities;
    const src = source();
    const needsVideo =
      src.kind === "file" ||
      (src.kind === "review" &&
        !sessionReview(pred.reviews?.find((r) => r.id === src.id)));
    const blocked = Boolean(caps && !caps.video && needsVideo);
    $("p-run").disabled = blocked || running(pred.job);
    $("p-run").title = blocked
      ? "agentzero-predict has no --video mode yet"
      : "";
  }

  $("p-kind").value = remembered("kind", "session");
  $("p-kind").onchange = showKind;
  $("p-session").onchange = () => {
    remember("session", $("p-session").value);
    fillSegments();
  };
  $("p-review").onchange = checkForm;
  $("p-file").oninput = checkForm;
  $("p-recheck").onclick = () => loadInfo(true).catch(showRunError);

  $("p-form").onsubmit = async (event) => {
    event.preventDefault();
    const number = (id) => {
      const text = $(id).value.trim();
      return text === "" || $(id).disabled ? null : Number(text);
    };
    remember("ckpt", $("p-ckpt").value);
    showRunError(null);
    try {
      pred.job = await api("run", {
        source: source(),
        start_s: number("p-start"),
        end_s: number("p-end"),
        checkpoint: $("p-ckpt").value,
        cpu: $("p-cpu").checked,
      });
    } catch (error) {
      return showRunError(error.message);
    }
    renderJob();
    poll();
  };

  $("p-cancel").onclick = async () => {
    pred.job = await api("cancel", {});
    renderJob();
  };

  function showRunError(message) {
    const el = $("p-error");
    el.hidden = !message;
    el.textContent = message ?? "";
  }

  // ------------------------------------------------------------- the run

  const running = (job) => job?.state === "running";

  async function poll() {
    clearTimeout(pred.pollTimer);
    const before = pred.job;
    try {
      pred.job = await api("job");
    } catch {
      return;
    }
    renderJob();
    if (running(pred.job) && pred.shown && !document.hidden) {
      pred.pollTimer = setTimeout(poll, POLL_MS);
    }
    // Just finished: list it and show it
    if (running(before) && pred.job?.state === "done") {
      await loadRuns();
      location.hash = hashOf(pred.job.key, pred.job.checkpoint, 0);
    }
  }

  function renderJob() {
    const job = pred.job;
    const busy = running(job);
    $("p-cancel").hidden = !busy;
    checkForm();
    renderChip(job);
    const meter = $("p-progress");
    meter.hidden = !job;
    $("p-log-wrap").hidden = !job;
    if (!job) return;
    meter.dataset.level =
      job.state === "failed"
        ? "critical"
        : job.state === "cancelled"
          ? "warning"
          : "ok";
    $("p-state").textContent = STATES[job.state] ?? job.state;
    const share = job.total ? job.done / job.total : busy ? 0 : 1;
    $("p-count").textContent = job.total
      ? `${job.done} / ${job.total}`
      : job.seconds != null
        ? `${job.seconds} s`
        : "";
    meter.querySelector(".meter-fill").style.width = `${100 * share}%`;
    const range =
      job.start_s != null || job.end_s != null
        ? ` · ${job.start_s ?? 0}–${job.end_s ?? "end"} s`
        : "";
    $("p-job-note").textContent =
      `${job.title}${range} · ${job.checkpoint}${job.cpu ? " · CPU" : ""}`;
    if (job.error) {
      const el = $("p-error");
      el.hidden = false;
      el.classList.toggle("p-oom", job.out_of_memory);
      el.textContent = job.out_of_memory
        ? `GPU out of memory. ${job.error}.`
        : job.error;
    } else if (busy) showRunError(null);
    $("p-command").textContent = job.command;
    $("p-log").textContent = job.log.join("\n");
  }

  function renderChip(job) {
    if (!job) return setChip("");
    const text = {
      running: job.total
        ? `IDM: ${job.done}/${job.total}`
        : "IDM: loading the model",
      done: `IDM: done in ${job.seconds} s`,
      failed: job.out_of_memory ? "IDM: GPU out of memory" : "IDM: failed",
      cancelled: "IDM: cancelled",
    }[job.state];
    const level = {
      running: "warning",
      done: "good",
      failed: "critical",
      cancelled: "off",
    }[job.state];
    setChip(text, level);
  }

  // ------------------------------------------------------------ the runs

  async function loadRuns() {
    try {
      pred.runs = (await api("runs")).runs;
    } catch (error) {
      $("p-runs").innerHTML =
        `<tr><td colspan="6" class="level-critical">${escapeHtml(error.message)}</td></tr>`;
      return;
    }
    renderRuns();
  }

  const hashOf = (key, ckpt, t) =>
    `#predictor/${new URLSearchParams({ key, ckpt, t: t.toFixed(2) })}`;

  function renderRuns() {
    const body = $("p-runs");
    body.replaceChildren();
    $("p-runs-note").textContent = pred.runs.length
      ? `${pred.runs.length}`
      : "";
    if (!pred.runs.length) {
      body.innerHTML = `<tr><td colspan="5" class="panel-note">No predictions yet: run one.</td></tr>`;
    }
    for (const run of pred.runs) {
      const tr = document.createElement("tr");
      const range =
        run.start_s != null || run.end_s != null
          ? `${run.start_s ?? 0}–${run.end_s ?? "end"} s`
          : "whole";
      const when = new Date(run.finished_ms ?? run.started_ms);
      tr.innerHTML = `<td>${escapeHtml(run.title)}${run.session ? ' <span class="p-tag">truth</span>' : ""}</td><td>${escapeHtml(run.checkpoint)}</td><td class="num">${range}</td><td class="num">${run.frames}</td><td class="num">${run.seconds ?? "–"} s</td><td class="num">${when.toLocaleString()}</td>`;
      if (pred.run?.key === run.key && pred.run?.checkpoint === run.checkpoint)
        tr.className = "current";
      tr.onclick = () => {
        location.hash = hashOf(run.key, run.checkpoint, 0);
      };
      body.append(tr);
    }
  }

  // ------------------------------------------------------------ the viewer

  /** The input overlay and a small controller, copied from the Studio */
  const hud = $("input-hud").cloneNode(true);
  hud.removeAttribute("id");
  hud.dataset.keys = "";
  $("p-screen").append(hud);

  const mini = $("procon").cloneNode(true);
  for (const el of [mini, ...mini.querySelectorAll("[id]")]) {
    if (!el.closest("defs")) el.removeAttribute("id");
  }
  mini.classList.remove("is-behind", "is-idle");
  mini.classList.add("p-mini");
  mini.style.transform = "";
  mini.setAttribute("aria-label", "Predicted controller");
  $("p-mini").append(mini);
  const miniButtons = [...mini.querySelectorAll("[data-btn]")];
  const miniSticks = {
    l: mini.querySelector('[data-btn="l_stick"]'),
    r: mini.querySelector('[data-btn="r_stick"]'),
  };

  /** Open a stored run */
  function openRun(key, ckpt, t) {
    const run = pred.runs.find((r) => r.key === key && r.checkpoint === ckpt);
    if (!run) {
      $("p-viewer-note").textContent = `No stored run ${key} / ${ckpt}.`;
      return;
    }
    const same = pred.run === run;
    pred.run = run;
    renderRuns();
    $("p-empty").hidden = true;
    video.hidden = false;
    $("p-viewer-note").textContent =
      `${run.title} · ${run.checkpoint} · ${run.fps?.toFixed(2) ?? "?"} fps`;
    $("p-agree-mode").hidden = !run.session;
    if (!same) {
      pred.chunk = null;
      pred.frame = -1;
      video.src = `/api/cuttlefish/video?${new URLSearchParams(run.play)}`;
    }
    const seek = () => {
      if (Math.abs(video.currentTime - t) > 0.05) video.currentTime = t;
      onFrame();
    };
    if (video.readyState >= 1) seek();
    else video.addEventListener("loadedmetadata", seek, { once: true });
  }

  const fps = () => pred.run?.fps || 30;
  const frameAt = (seconds) => Math.max(0, Math.round(seconds * fps()));
  const frameCount = () =>
    Number.isFinite(video.duration) ? Math.floor(video.duration * fps()) : 0;

  /** The labels of frame n, if loaded */
  function labelsAt(n) {
    const chunk = pred.chunk;
    if (!chunk || n < chunk.start || n >= chunk.start + chunk.pred.length)
      return [undefined, undefined];
    const i = n - chunk.start;
    return [chunk.pred[i], chunk.truth?.[i]];
  }

  /** Load the frames around n unless they are loaded */
  async function ensureChunk(n) {
    const chunk = pred.chunk;
    const half = Math.ceil((pred.span * fps()) / 2) + 30;
    const low = Math.max(0, n - half);
    const high = Math.min(frameCount() || Infinity, n + half);
    if (chunk && low >= chunk.start && high <= chunk.start + chunk.pred.length)
      return;
    const start = Math.max(0, n - CHUNK / 2);
    const key = `${pred.run.key}/${pred.run.checkpoint}/${start}`;
    if (pred.loading === key) return;
    pred.loading = key;
    const run = pred.run;
    try {
      const data = await api(
        `labels?${new URLSearchParams({ key: run.key, ckpt: run.checkpoint, start, stop: start + CHUNK })}`,
      );
      if (pred.run !== run) return;
      pred.chunk = { start, pred: data.pred, truth: data.truth };
    } catch (error) {
      $("p-viewer-note").textContent = error.message;
    } finally {
      if (pred.loading === key) pred.loading = null;
    }
    draw();
  }

  /** The video shows another frame: follow it */
  function onFrame(mediaTime) {
    if (!pred.run) return;
    const n = frameAt(mediaTime ?? video.currentTime);
    if (n === pred.frame) return;
    pred.frame = n;
    ensureChunk(n);
    draw();
    scheduleAgreement();
    history.replaceState(
      null,
      "",
      hashOf(pred.run.key, pred.run.checkpoint, n / fps()),
    );
    rememberView();
  }

  /** Follow every presented frame while playing */
  function follow() {
    if (!video.requestVideoFrameCallback) return;
    video.requestVideoFrameCallback((_, meta) => {
      onFrame(meta.mediaTime);
      if (!video.paused && pred.shown) follow();
    });
  }

  video.addEventListener("play", () => {
    $("p-play").textContent = "Pause";
    follow();
  });
  video.addEventListener("pause", () => {
    $("p-play").textContent = "Play";
    onFrame();
  });
  video.addEventListener("seeked", () => onFrame());
  video.addEventListener("timeupdate", () => {
    if (!video.requestVideoFrameCallback) onFrame();
  });

  $("p-play").onclick = () => (video.paused ? video.play() : video.pause());
  const step = (delta) => {
    video.pause();
    const n = Math.max(0, pred.frame + delta);
    video.currentTime = (n + 0.5) / fps();
  };
  $("p-prev").onclick = () => step(-1);
  $("p-next").onclick = () => step(1);
  $("p-span").value = String(pred.span);
  $("p-span").onchange = (event) => {
    pred.span = Number(event.target.value);
    ensureChunk(pred.frame);
    draw();
    scheduleAgreement();
  };
  $("p-overlay").value = remembered("overlay", "pred");
  $("p-overlay").onchange = (event) => {
    remember("overlay", event.target.value);
    draw();
  };

  document.addEventListener("keydown", (event) => {
    if (!pred.shown || !pred.run) return;
    const tag = event.target.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === " ") $("p-play").click();
    else if (event.key === "ArrowLeft") step(event.shiftKey ? -10 : -1);
    else if (event.key === "ArrowRight") step(event.shiftKey ? 10 : 1);
    else return;
    event.preventDefault();
  });

  // ---------------------------------------------------------- drawing

  function draw() {
    if (!pred.run || !pred.shown) return;
    const n = pred.frame;
    const [p, t] = labelsAt(n);
    $("p-frame").textContent = `frame ${n} · ${clockText(n / fps())}`;
    drawOverlay(p, t);
    drawMini(p);
    drawProbs(p, t);
    drawTimeline();
  }

  /** The input overlay: the prediction or the truth */
  function drawOverlay(p, t) {
    const which = $("p-overlay").value;
    const label = which === "truth" ? t : which === "pred" ? p : null;
    const show = Boolean(label && label.valid !== false);
    hud.toggleAttribute("hidden", !show);
    hud.classList.toggle("p-hud-truth", which === "truth");
    if (!show) return;
    drawInputHud(hud, {
      left: (label.left_stick ?? [2048, 2048]).map(stickPercent),
      right: (label.right_stick ?? [2048, 2048]).map(stickPercent),
      pressed: new Set(label.buttons ?? []),
      yaw: (label.gyro_deg?.[2] ?? 0) * fps(),
      pitch: (label.gyro_deg?.[1] ?? 0) * fps(),
    });
  }

  /** The small controller: the predicted buttons and sticks */
  function drawMini(p) {
    const pressed = new Set(p?.buttons ?? []);
    for (const el of miniButtons) {
      el.classList.toggle("on", pressed.has(el.dataset.btn));
    }
    for (const [side, key] of [
      ["l", "left_stick"],
      ["r", "right_stick"],
    ]) {
      const [x, y] = (p?.[key] ?? [2048, 2048]).map(stickPercent);
      miniSticks[side]?.setAttribute(
        "transform",
        `translate(${((x / 100) * 24).toFixed(1)} ${((-y / 100) * 24).toFixed(1)})`,
      );
    }
    mini.classList.toggle("is-idle", !p);
  }

  /** Each button's predicted probability, pressed ones marked */
  function drawProbs(p, t) {
    const probs = p?.button_probs ?? {};
    const truth = new Set(t?.buttons ?? []);
    const shown = BUTTONS.filter(
      (b) => (probs[b] ?? 0) >= 0.05 || truth.has(b),
    );
    $("p-probs").innerHTML = shown.length
      ? shown
          .map((b) => {
            const value = probs[b] ?? 0;
            const on = (p?.buttons ?? []).includes(b);
            return `<div class="p-prob${on ? " on" : ""}"><span>${b.toUpperCase()}</span><i style="width:${(100 * value).toFixed(0)}%"></i><b class="num">${value.toFixed(2)}</b>${truth.has(b) ? '<em title="pressed in the recording">✓</em>' : ""}</div>`;
          })
          .join("")
      : `<p class="panel-note">${p ? "No button above 5%." : "No prediction for this frame."}</p>`;
  }

  /** Theme colors for the canvas */
  function colors() {
    const style = getComputedStyle(canvas);
    const get = (name) => style.getPropertyValue(name).trim();
    return {
      truth: get("--s1") || "#3987e5",
      pred: get("--s2") || "#d95926",
      grid: get("--grid") || "#222",
      axis: get("--axis") || "#444",
      text: get("--text-3") || "#888",
      now: get("--text") || "#fff",
    };
  }

  /** Buttons and signals around the playhead, truth filled, prediction as
   * lines */
  function drawTimeline() {
    const chunk = pred.chunk;
    const width = canvas.clientWidth;
    if (!width) return;
    const n0 = pred.frame;
    const half = (pred.span * fps()) / 2;
    const first = Math.floor(n0 - half);
    const last = Math.ceil(n0 + half);
    const lanes = [];
    const has = (key, label) => label && label[key] != null;
    const frames = [];
    for (let n = first; n <= last; n++) frames.push([n, ...labelsAt(n)]);
    for (const b of BUTTONS) {
      if (
        frames.some(
          ([, p, t]) => p?.buttons?.includes(b) || t?.buttons?.includes(b),
        )
      )
        lanes.push({ kind: "button", name: b, label: b.toUpperCase() });
    }
    for (const [name, label, pick] of SIGNALS) {
      if (
        frames.some(
          ([, p, t]) => (p && pick(p) != null) || (t && pick(t) != null),
        )
      )
        lanes.push({ kind: "signal", name, label, pick });
    }
    const LABEL = 96;
    const BUTTON_H = 14;
    const SIGNAL_H = 38;
    const TOP = 16;
    const height =
      TOP +
      lanes.reduce(
        (h, l) => h + (l.kind === "button" ? BUTTON_H : SIGNAL_H) + 4,
        0,
      ) +
      4;
    const dpr = window.devicePixelRatio || 1;
    canvas.style.height = `${Math.max(height, 60)}px`;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(Math.max(height, 60) * dpr);
    const g = canvas.getContext("2d");
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const c = colors();
    const plotW = width - LABEL - 8;
    const x = (n) => LABEL + ((n - first) / (last - first)) * plotW;
    g.font = "11px Inter, system-ui, sans-serif";
    g.textBaseline = "middle";

    // Seconds along the top
    g.fillStyle = c.text;
    g.strokeStyle = c.grid;
    g.lineWidth = 1;
    for (let s = Math.ceil(first / fps()); s <= last / fps(); s++) {
      const px = x(s * fps());
      g.beginPath();
      g.moveTo(px, TOP - 4);
      g.lineTo(px, height);
      g.stroke();
      // Labels at least 44 px apart
      const perSecond = plotW / pred.span;
      const every = [1, 2, 5, 10, 30].find((n) => n * perSecond >= 44) ?? 60;
      if (s % every === 0 && px < width - 30)
        g.fillText(clockText(s).replace(/\.0$/, ""), px + 2, 7);
    }

    if (!chunk) {
      g.fillText("Loading…", LABEL, TOP + 12);
    } else if (!lanes.length) {
      g.fillText("No predictions here", LABEL, TOP + 12);
    }

    let y = TOP;
    for (const lane of lanes) {
      const h = lane.kind === "button" ? BUTTON_H : SIGNAL_H;
      g.fillStyle = c.text;
      g.fillText(lane.label, 4, y + h / 2);
      if (lane.kind === "button") {
        for (const [n, p, t] of frames) {
          const x0 = x(n - 0.5);
          const w = Math.max(1, x(n + 0.5) - x0);
          // Truth fills the lower half, the prediction's probability grows
          // up from the middle and a pressed prediction marks the top
          if (t?.valid !== false && t?.buttons?.includes(lane.name)) {
            g.fillStyle = c.truth;
            g.globalAlpha = 0.7;
            g.fillRect(x0, y + h / 2, w, h / 2);
            g.globalAlpha = 1;
          }
          if (p) {
            const prob = p.button_probs?.[lane.name] ?? 0;
            g.fillStyle = c.pred;
            g.globalAlpha = 0.35;
            g.fillRect(x0, y + (h / 2) * (1 - prob), w, (h / 2) * prob);
            g.globalAlpha = 1;
            if (p.buttons?.includes(lane.name)) {
              g.fillRect(x0, y, w, 2.5);
            }
          }
        }
      } else {
        // Scale: sticks -1..1, others by the largest value in view
        let scale = 1;
        const stickLane = /^(left|right)_/.test(lane.name);
        if (!stickLane) {
          scale = 0;
          for (const [, p, t] of frames) {
            for (const l of [p, t]) {
              const v = l && lane.pick(l);
              if (v != null) scale = Math.max(scale, Math.abs(v));
            }
          }
          scale = scale || 1;
        }
        const mid = y + h / 2;
        const py = (v) => mid - (v / scale) * (h / 2 - 1);
        g.strokeStyle = c.axis;
        g.beginPath();
        g.moveTo(LABEL, mid);
        g.lineTo(LABEL + plotW, mid);
        g.stroke();
        // Truth: filled to zero
        g.fillStyle = c.truth;
        g.globalAlpha = 0.4;
        for (const [n, , t] of frames) {
          const v = t && t.valid !== false ? lane.pick(t) : null;
          if (v == null) continue;
          const x0 = x(n - 0.5);
          const top = py(v);
          g.fillRect(
            x0,
            Math.min(top, mid),
            Math.max(1, x(n + 0.5) - x0),
            Math.abs(top - mid) || 1,
          );
        }
        g.globalAlpha = 1;
        // Prediction: a line
        g.strokeStyle = c.pred;
        g.lineWidth = 1.5;
        g.beginPath();
        let drawing = false;
        for (const [n, p] of frames) {
          const v = p ? lane.pick(p) : null;
          if (v == null) {
            drawing = false;
            continue;
          }
          if (drawing) g.lineTo(x(n), py(v));
          else g.moveTo(x(n), py(v));
          drawing = true;
        }
        g.stroke();
        g.lineWidth = 1;
        g.fillStyle = c.text;
        g.textAlign = "right";
        g.fillText(
          scale === 1 ? "±1" : `±${scale.toPrecision(2)}`,
          LABEL - 4,
          y + 7,
        );
        g.textAlign = "left";
      }
      y += h + 4;
    }

    // The playhead
    g.strokeStyle = c.now;
    g.lineWidth = 1.5;
    g.beginPath();
    g.moveTo(x(n0), TOP - 6);
    g.lineTo(x(n0), height);
    g.stroke();
    canvas.dataset.first = String(first);
    canvas.dataset.last = String(last);
    canvas.dataset.left = String(LABEL);
    canvas.dataset.plot = String(plotW);
  }

  // Click on the timeline to go there
  canvas.addEventListener("click", (event) => {
    if (!pred.run) return;
    const rect = canvas.getBoundingClientRect();
    const first = Number(canvas.dataset.first);
    const last = Number(canvas.dataset.last);
    const left = Number(canvas.dataset.left);
    const plot = Number(canvas.dataset.plot);
    const share = (event.clientX - rect.left - left) / plot;
    if (share < 0 || share > 1) return;
    const n = Math.round(first + share * (last - first));
    video.currentTime = (Math.max(0, n) + 0.5) / fps();
  });

  new ResizeObserver(() => draw()).observe(canvas);

  // -------------------------------------------------------- agreement

  /** Ask for the agreement of the frames in view (or all), at most once a
   * second while playing */
  function scheduleAgreement() {
    if (!pred.run?.session) {
      $("p-agree").innerHTML =
        `<p class="panel-note">${pred.run ? "No controller recording for this video: predictions only." : "Open a prediction."}</p>`;
      $("p-agree-note").textContent = "";
      return;
    }
    clearTimeout(pred.agreeTimer);
    const wait = Math.max(250, 1000 - (performance.now() - pred.agreeAt));
    pred.agreeTimer = setTimeout(loadAgreement, wait);
  }

  async function loadAgreement() {
    const run = pred.run;
    if (!run?.session || !pred.shown) return;
    pred.agreeAt = performance.now();
    const all = $("p-agree-mode").value === "all";
    const half = Math.round((pred.span * fps()) / 2);
    const start = all ? 0 : Math.max(0, pred.frame - half);
    const stop = all ? Math.max(1, frameCount()) : pred.frame + half;
    let agreement;
    try {
      agreement = await api(
        `agreement?${new URLSearchParams({ key: run.key, ckpt: run.checkpoint, start, stop })}`,
      );
    } catch (error) {
      $("p-agree").innerHTML =
        `<p class="notice">${escapeHtml(error.message)}</p>`;
      return;
    }
    if (pred.run !== run) return;
    renderAgreement(agreement, start, stop);
  }

  $("p-agree-mode").onchange = () => loadAgreement();

  function renderAgreement(a, start, stop) {
    $("p-agree-note").textContent =
      `${a.frames} frames · ${clockText(start / fps())}–${clockText(stop / fps())}`;
    const value = (v, digits = 2) => (v == null ? "–" : v.toFixed(digits));
    const level = (v) =>
      v == null
        ? ""
        : v >= 0.7
          ? "level-good"
          : v >= 0.4
            ? "level-warning"
            : "level-critical";
    const buttons = a.buttons
      .filter((b) => b.truth + b.pred > 0)
      .map(
        (b) =>
          `<tr><td>${b.name.toUpperCase()}</td><td class="num ${level(b.f1)}">${value(b.f1)}</td><td class="num">${b.truth}</td><td class="num">${b.pred}</td></tr>`,
      )
      .join("");
    const names = Object.fromEntries(SIGNALS.map(([n, l]) => [n, l]));
    const signals = a.signals
      .map(
        (s) =>
          `<tr><td>${escapeHtml(names[s.name] ?? s.name)}</td><td class="num ${level(s.r)}">${value(s.r)}</td><td class="num" colspan="2">${s.n ? `${s.n} frames` : s.name.startsWith("turn") ? "no truth" : "–"}</td></tr>`,
      )
      .join("");
    $("p-agree").innerHTML = `
      <table class="data-table p-agree-table">
        <thead><tr><th>Button</th><th>F1</th><th>Truth</th><th>Predicted</th></tr></thead>
        <tbody>${buttons || '<tr><td colspan="4" class="panel-note">No button pressed here.</td></tr>'}</tbody>
        <thead><tr><th>Signal</th><th>r</th><th colspan="2"></th></tr></thead>
        <tbody>${signals}</tbody>
      </table>`;
  }

  // -------------------------------------------------------------- routing

  function rememberView() {
    const hash = location.hash.startsWith("#predictor")
      ? location.hash
      : "#predictor";
    document.querySelector('.app-nav [data-app="predictor"]').href = hash;
    remember("view", hash);
  }

  async function route(state) {
    // Reading the command's options takes seconds; the viewer does not wait
    loadInfo().catch((error) => showRunError(error.message));
    showKind();
    poll();
    if (!pred.runs.length) await loadRuns();
    const key = state.get("key");
    const ckpt = state.get("ckpt");
    if (key && ckpt) openRun(key, ckpt, Number(state.get("t")) || 0);
    else draw();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const was = pred.shown;
    pred.shown = app === "predictor";
    if (pred.shown) route(state);
    else if (was) {
      clearTimeout(pred.pollTimer);
      clearTimeout(pred.agreeTimer);
      video.pause();
    }
  });

  document.addEventListener("visibilitychange", () => {
    if (!pred.shown) return;
    if (document.hidden) video.pause();
    else poll();
  });

  document.querySelector('.app-nav [data-app="predictor"]').href = remembered(
    "view",
    "#predictor",
  );
})();
