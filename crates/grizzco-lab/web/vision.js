// Vision app: detect and track objects in a recorded segment, review the
// boxes frame by frame through our classes (classes.json; the COCO ones
// behind a toggle), and send them to the labels as model boxes. The
// classes panel opens on the labeled dataset. The segment plays in the
// shared player (player.js: the Inkspector's frames, keys, scrubber with
// the processed frames as marks, neighbours) with the boxes as a layer over
// it; the Tracks panel draws the tracks over the frames too. Runs after
// app.js, player.js and stages.js and uses their helpers ($, escapeHtml,
// appUrl, t, StageMap, stageOfVideo). Runs go through /api/vision (see
// src/vision.rs); the Salmon Run detector is AgentZero's service, which the
// lab calls and can start (src/vision/detector.rs), with its state, training
// and how weak it still is in a card under the model choice.
// State lives in the address: /vision/<session>?seg=<file>&n=<frame>.
"use strict";

(() => {
  /** How often a running detection is asked about, in ms */
  const POLL_MS = 500;
  /** Frame size of the box coordinates in the SVGs */
  const W = 640;
  const H = 360;
  /** Seconds of trail on each side of the playhead */
  const TRAIL_S = 2;
  /** Frames kept for the trails' picture and the crops */
  const TRAIL_IMAGES = 40;
  /** Crops of a highlighted track at most, and their size in pixels */
  const CROPS = 12;
  const CROP_PX = 72;
  /** Class colors, picked by a hash of the name */
  const PALETTE = [
    "#ff5c8a",
    "#ffd23f",
    "#4fb3ff",
    "#8bd450",
    "#ff8a3d",
    "#b86bff",
    "#20c7a8",
    "#f5c518",
    "#6a8cff",
    "#ff6fd8",
    "#3dd6d0",
    "#e0564a",
  ];
  const STATES = {
    loading: "Loading the model",
    running: "Detecting",
    done: "Done",
    failed: "Failed",
    cancelled: "Cancelled",
  };

  const vis = {
    /** Whether the app is shown */
    shown: false,
    info: null,
    sessions: null,
    /** The current or last run */
    job: null,
    /** Results of the selected segment: {run, frames, classes, tracks} */
    results: null,
    /** Session/segment of the results on screen */
    loadedKey: null,
    /** Session/segment open in the player */
    playerKey: null,
    /** Frames in the selected segment, once known */
    frames: null,
    /** Track highlighted in the table and the trails */
    track: null,
    /** The results' tracks by id, see indexTracks */
    trackIndex: new Map(),
    /** The trails' latest drawing: an older one's picture is dropped */
    trailToken: null,
    pollTimer: null,
    /** The Salmon Run detector's last status (GET detector), or null */
    detector: null,
    /** Frames labeled by people so far and the target, from the overview */
    labeled: null,
  };

  /** Longest wait for a detector started from the page, in ms */
  const DETECTOR_START_MS = 120000;

  /** The shared player: the segment's frames, the boxes drawn over them */
  const player = new Player({
    screen: $("v-screen"),
    controls: $("v-player-controls"),
    scrubber: $("v-scrubber"),
    strip: $("v-strip"),
    remember: "vision",
    neighbours: { radius: 3 },
    onFrame: drawFrame,
  });

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-vision-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-vision-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  async function api(path, body) {
    const response = await fetch(
      `/api/vision/${path}`,
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

  /** Our classes by name, from the dataset overview */
  const ourClasses = new Map();

  const labelOf = (name) => ourClasses.get(name)?.label ?? name;

  // ---------------------------------------------------- dataset overview

  /** Our classes with their labeled boxes, and the labeled segments */
  async function loadOverview() {
    let overview;
    try {
      overview = await api("overview");
    } catch (error) {
      $("v-dataset-classes").innerHTML =
        `<tr><td colspan="3" class="level-critical">${escapeHtml(error.message)}</td></tr>`;
      return;
    }
    ourClasses.clear();
    for (const c of overview.classes) if (!c.unknown) ourClasses.set(c.name, c);
    const { labeled_frames: labeled, target_frames: target } = overview;
    vis.labeled = { labeled, target };
    renderDetector();
    $("v-target-text").textContent = `${labeled} / ${target}`;
    $("v-target-frames").textContent = target;
    const meter = $("v-target");
    meter.dataset.level = labeled >= target ? "ok" : "warning";
    meter.querySelector(".meter-fill").style.width =
      `${Math.min(100, (100 * labeled) / target)}%`;
    $("v-dataset-classes").innerHTML = overview.classes
      .map(
        (c) =>
          `<tr class="${c.user + c.model ? "" : "v-none"}"><td><i class="v-swatch" style="background:${c.color ?? colorOf(c.name)}"></i>${escapeHtml(c.label)}${c.unknown ? ' <span class="p-tag">not in classes.json</span>' : ""}</td><td class="num">${c.user}</td><td class="num">${c.model}</td></tr>`,
      )
      .join("");
    if (!overview.classes.length) {
      $("v-dataset-classes").innerHTML =
        `<tr><td colspan="3" class="panel-note">No classes.json yet: open the Inkspector's Label mode.</td></tr>`;
    }
    $("v-dataset-segments").innerHTML = overview.segments.length
      ? overview.segments
          .map((s) => {
            const name = `${s.session} · ${s.file ?? s.stem}`;
            const link = s.file
              ? `<a href="${escapeHtml(appUrl("inspect", { s: s.session, seg: s.file, label: 1 }))}">${escapeHtml(name)}</a>`
              : escapeHtml(name);
            return `<tr><td>${link}</td><td class="num">${s.labeled} of ${s.frames}</td><td class="num">${s.user} + ${s.model} model</td></tr>`;
          })
          .join("")
      : `<tr><td colspan="3" class="panel-note">Nothing labeled yet.</td></tr>`;
    $("v-dataset-dir").textContent = overview.dir;
    if (vis.results) drawFrame(player.frame);
  }

  /** The classes panel's two views: the dataset or the run on screen */
  function showTab(tab) {
    remember("tab", tab);
    const dataset = tab === "dataset";
    $("v-tab-dataset").setAttribute("aria-pressed", String(dataset));
    $("v-tab-run").setAttribute("aria-pressed", String(!dataset));
    $("v-dataset").hidden = !dataset;
    $("v-run-classes").hidden = dataset;
    $("v-send-form").hidden = dataset;
    $("v-sent").hidden = dataset || !$("v-sent").innerHTML;
    $("v-classes-note").hidden = dataset;
  }

  $("v-tab-dataset").onclick = () => showTab("dataset");
  $("v-tab-run").onclick = () => showTab("run");

  function colorOf(name) {
    const ours = ourClasses.get(name)?.color;
    if (ours) return ours;
    let hash = 0;
    for (const c of name) hash = (hash * 31 + c.charCodeAt(0)) >>> 0;
    return PALETTE[hash % PALETTE.length];
  }

  const fmt = (ms) => (ms == null ? "–" : ms.toFixed(ms < 10 ? 1 : 0));

  function setChip(text, level = "off") {
    const chip = $("v-chip");
    chip.hidden = !text;
    chip.dataset.level = level;
    chip.querySelector(".chip-text").textContent = text;
  }

  const selected = () => ({
    s: $("v-session").value,
    seg: $("v-segment").value,
  });

  // ------------------------------------------------------------ the form

  async function loadInfo() {
    if (vis.info) return;
    vis.info = await api("info");
    const { info } = vis;
    const select = $("v-model");
    const speed = { n: "fastest", s: "balanced", m: "slowest" };
    select.replaceChildren(
      ...info.sizes.map(
        (size) =>
          new Option(`YOLOv8${size} · COCO (${speed[size] ?? size})`, size),
      ),
    );
    if (info.custom) {
      const name = info.custom.weights.split("/").pop();
      select.add(new Option(`${name} · own weights`, "custom"));
    }
    select.add(new Option(t("v.det.model"), info.detector.model));
    select.value = remembered("model", info.size);
    if (!select.value) select.value = info.size;
    $("v-cpu").checked = remembered("cpu", "false") === "true";
    $("v-track").checked = remembered("track", "true") === "true";
    $("v-map").value = remembered("map", "person=player");
    modelChanged();
  }

  // --------------------------------------------- the Salmon Run detector

  /** Whether the model chosen is the Salmon Run detector */
  const salmon = () => $("v-model").value === vis.info?.detector.model;

  /** The CPU choice (the service picks its device; candle only with
   * CUDA) and the detector's card follow the model */
  function modelChanged() {
    $("v-cpu-wrap").hidden = !vis.info?.cuda && !salmon();
    $("v-detector").hidden = !salmon();
    if (salmon()) checkDetector();
    else renderDetector();
  }

  $("v-model").onchange = () => {
    remember("model", $("v-model").value);
    modelChanged();
  };

  /** Ask the lab whether the detector answers, and show it */
  async function checkDetector() {
    if (!vis.detector) setDetector(t("v.det.checking"), "off");
    try {
      vis.detector = await api("detector");
    } catch (error) {
      vis.detector = { ok: false, error: error.message };
    }
    renderDetector();
    return vis.detector.ok;
  }

  function setDetector(text, level) {
    const chip = $("v-det-chip");
    chip.dataset.level = level;
    chip.querySelector(".chip-text").textContent = text;
    chip.title = text;
  }

  /** The detector's card: state, how to start it, its training, and how
   * weak it is until enough frames are labeled */
  function renderDetector(note) {
    if (!salmon()) return;
    const status = vis.detector;
    const health = status?.health;
    const down = status && !status.ok;
    if (status) {
      setDetector(
        t(down ? "v.det.down" : health?.busy ? "v.det.busy" : "v.det.ready"),
        down ? "critical" : health?.busy ? "warning" : "good",
      );
    }
    $("v-det-start").hidden = !(down && status.can_start);
    $("v-det-note").innerHTML =
      note ??
      (down && status.url
        ? t("v.det.downNote", {
            url: escapeHtml(status.url),
            command: escapeHtml(status.command),
            how: status.can_start ? t("v.det.downHow") : "",
          })
        : "");
    renderFacts(health, status?.saved_ms);
    const weak = $("v-det-weak");
    const counts = vis.labeled;
    weak.hidden = !counts || counts.labeled >= counts.target;
    if (counts) {
      $("v-det-weak-text").textContent = t("v.det.weak", counts);
      $("v-det-progress").textContent = t("v.det.progress", counts);
    }
  }

  /** What /health says about the checkpoint: where, how good, on what */
  function renderFacts(health, savedMs) {
    const facts = $("v-det-facts");
    facts.hidden = !health;
    if (!health) return;
    const trained = health.trained ?? {};
    const best = trained.best;
    const split = trained.split ?? {};
    const gib =
      health.free_gpu_bytes != null
        ? t("v.det.gpuFree", {
            gib: (health.free_gpu_bytes / 2 ** 30).toFixed(1),
          })
        : null;
    const rows = [
      ["v.det.checkpoint", health.model],
      [
        "v.det.map50",
        best?.map50 != null
          ? t("v.det.map50Value", {
              map50: best.map50.toFixed(3),
              epoch: best.epoch ?? "–",
            })
          : "–",
      ],
      [
        "v.det.trained",
        split.train != null
          ? t("v.det.trainedValue", { train: split.train, val: split.val ?? 0 })
          : "–",
      ],
      ["v.det.saved", savedMs ? new Date(savedMs).toLocaleString(lang()) : "–"],
      [
        "v.det.device",
        [
          health.loaded ? health.device?.toUpperCase() : t("v.det.notLoaded"),
          gib,
        ]
          .filter(Boolean)
          .join(" · "),
      ],
      ["v.det.classes", health.classes?.length || "–"],
    ];
    facts.innerHTML = rows
      .map(
        ([key, value]) =>
          `<dt>${escapeHtml(t(key))}</dt><dd>${escapeHtml(String(value))}</dd>`,
      )
      .join("");
  }

  /** The page's language, for dates */
  const lang = () => document.documentElement.lang || undefined;

  /** Start the detector with the configured command and wait for it */
  async function startDetector() {
    const button = $("v-det-start");
    button.disabled = true;
    try {
      await api("detector/start", {});
      renderDetector(escapeHtml(t("v.det.starting")));
      const until = performance.now() + DETECTOR_START_MS;
      while (performance.now() < until) {
        await new Promise((resolve) => setTimeout(resolve, 1000));
        if (!salmon() || !vis.shown) return;
        const ok = await checkDetector();
        if (ok) return;
        const started = vis.detector.started;
        if (started?.startsWith("exited")) {
          return renderDetector(
            t("v.det.exited", { state: escapeHtml(started) }),
          );
        }
        renderDetector(escapeHtml(t("v.det.starting")));
      }
      renderDetector(escapeHtml(t("v.det.timeout")));
    } catch (error) {
      renderDetector(escapeHtml(error.message));
    } finally {
      button.disabled = false;
    }
  }

  $("v-det-start").onclick = startDetector;
  $("v-det-check").onclick = () => checkDetector();
  // The labeling progress: the Classes panel's dataset view
  $("v-det-progress").onclick = () => {
    showTab("dataset");
    $("v-dataset").scrollIntoView({ behavior: "smooth", block: "nearest" });
  };

  async function loadSessions() {
    if (vis.sessions) return;
    const select = $("v-session");
    try {
      const response = await fetch("/api/inspect/sessions");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      vis.sessions = data.sessions;
    } catch (error) {
      vis.sessions = [];
      select.replaceChildren(new Option(`No sessions: ${error.message}`, ""));
      return;
    }
    select.replaceChildren(
      ...vis.sessions.map((s) => new Option(s.name, s.name)),
    );
    if (!vis.sessions.length) select.add(new Option("No sessions", ""));
  }

  const summaryOf = (name) => vis.sessions?.find((s) => s.name === name);

  /** Frame n of a segment, from the Inkspector */
  const frameUrl = (s, seg, n) =>
    `/api/inspect/frame?${new URLSearchParams({ s, seg, n })}`;

  function fillSegments() {
    const summary = summaryOf($("v-session").value);
    const select = $("v-segment");
    select.replaceChildren(
      ...(summary?.segments ?? []).map((s) => new Option(s.file, s.file)),
    );
    select.hidden = (summary?.segments.length ?? 0) < 2;
    describeRange();
  }

  /** What the range covers, in frames and seconds */
  function describeRange() {
    const start = Number($("v-start").value) || 0;
    const step = Math.max(1, Number($("v-step").value) || 1);
    const count = Math.max(1, Number($("v-count").value) || 1);
    const last = start + (count - 1) * step;
    const fps = summaryOf($("v-session").value)?.fps;
    const every =
      step === 1 ? "every frame" : `every ${step}${ordinal(step)} frame`;
    const seconds = fps ? ` · ${((last - start + 1) / fps).toFixed(1)} s` : "";
    const total = vis.frames;
    const past =
      total == null
        ? ""
        : start >= total
          ? ` · the segment has only ${total} frames`
          : last >= total
            ? ` · stops at the end (${total} frames)`
            : ` of ${total}`;
    const span = $("v-span");
    span.textContent = `${count} frames, ${every}: ${start}–${last}${seconds}${past}`;
    span.classList.toggle("level-critical", total != null && start >= total);
  }

  /** The segment's frame count, from the Inkspector */
  async function loadFrameCount(s, seg) {
    vis.frames = null;
    try {
      const response = await fetch(
        `/api/inspect/info?${new URLSearchParams({ s, seg })}`,
      );
      const info = await response.json();
      if (response.ok && selected().seg === seg) vis.frames = info.frames;
    } catch {
      // The note simply leaves the count out
    }
    describeRange();
  }

  const ordinal = (n) =>
    [11, 12, 13].includes(n % 100)
      ? "th"
      : ({ 1: "st", 2: "nd", 3: "rd" }[n % 10] ?? "th");

  $("v-session").onchange = () => {
    fillSegments();
    openSegment();
  };
  $("v-segment").onchange = () => openSegment();
  for (const id of ["v-start", "v-step", "v-count"]) {
    $(id).oninput = describeRange;
  }

  $("v-form").onsubmit = async (event) => {
    event.preventDefault();
    const { s, seg } = selected();
    if (!s || !seg) return;
    showError(null);
    // The detector's card says why and has the button to start it
    if (salmon() && !(await checkDetector()))
      return showError(t("v.det.needsService"));
    remember("model", $("v-model").value);
    remember("cpu", String($("v-cpu").checked));
    remember("track", String($("v-track").checked));
    try {
      vis.job = await api("run", {
        s,
        seg,
        start: Number($("v-start").value) || 0,
        step: Number($("v-step").value) || 1,
        count: Number($("v-count").value) || 1,
        model: $("v-model").value,
        cpu: $("v-cpu").checked,
        track: $("v-track").checked,
      });
    } catch (error) {
      return showError(error.message);
    }
    renderJob();
    poll();
  };

  $("v-cancel").onclick = async () => {
    vis.job = await api("cancel", {});
    renderJob();
  };

  function showError(message) {
    const el = $("v-error");
    el.hidden = !message;
    el.textContent = message ?? "";
  }

  // ------------------------------------------------------------- the run

  const running = (job) => job && ["loading", "running"].includes(job.state);

  async function poll() {
    clearTimeout(vis.pollTimer);
    const before = vis.job;
    try {
      vis.job = await api("job");
    } catch {
      return;
    }
    renderJob();
    const job = vis.job;
    if (running(job) && vis.shown && !document.hidden) {
      vis.pollTimer = setTimeout(poll, POLL_MS);
    }
    // Just finished on the segment on screen: show its results
    const { s, seg } = selected();
    if (running(before) && !running(job) && job.s === s && job.seg === seg) {
      loadResults(s, seg);
    }
    // The detector's state after a run of it (busy, or gone)
    if (running(before) && !running(job) && salmon()) checkDetector();
  }

  /** The run the panel shows: the current one while it runs or when it is
   * about the selected segment, else the one of the results on screen */
  function shownJob() {
    const job = vis.job;
    const { s, seg } = selected();
    if (running(job) || (job && job.s === s && job.seg === seg)) return job;
    return vis.results?.run ?? job;
  }

  /** The run's progress, device and timings, and the top bar's chip */
  function renderJob() {
    const busy = running(vis.job);
    $("v-run").disabled = busy;
    $("v-cancel").hidden = !busy;
    renderChip(vis.job);
    const job = shownJob();
    const meter = $("v-progress");
    meter.hidden = !job;
    showError(job?.error);
    $("v-device").textContent = job?.device
      ? `on ${job.device.toUpperCase()}`
      : "";
    renderTimings(job);
    if (!job) return;
    meter.dataset.level =
      job.state === "failed"
        ? "critical"
        : job.state === "cancelled"
          ? "warning"
          : "ok";
    $("v-state").textContent = STATES[job.state] ?? job.state;
    $("v-count-text").textContent = `${job.done} / ${job.count}`;
    meter.querySelector(".meter-fill").style.width =
      `${(100 * job.done) / Math.max(1, job.count)}%`;
    $("v-job-note").textContent =
      `${job.s} · ${job.seg} · ${job.model_name}${job.track ? " + tracking" : ""} · ${job.boxes} boxes`;
  }

  /** The top bar's chip: the current run, if any */
  function renderChip(job) {
    if (!job) return setChip("");
    const chipText = {
      loading: "Vision: loading model",
      running: `Vision: ${job.done}/${job.count}`,
      done: `Vision: done, ${job.done} frames`,
      failed: "Vision: failed",
      cancelled: `Vision: cancelled at ${job.done}`,
    }[job.state];
    const level = {
      loading: "warning",
      running: "warning",
      done: "good",
      failed: "critical",
      cancelled: "off",
    }[job.state];
    setChip(chipText, level);
  }

  function renderTimings(job) {
    const timings = job?.timings;
    const rows = [
      ["Decode", timings?.decode],
      ["Network", timings?.network],
      ["Total", timings?.total],
    ];
    $("v-timings").innerHTML = rows
      .map(
        ([name, stat]) =>
          `<tr><td>${name}</td><td class="num">${fmt(job?.done ? stat?.mean : null)}</td><td class="num">${fmt(job?.done ? stat?.p95 : null)}</td></tr>`,
      )
      .join("");
    const parts = [];
    if (job?.device_reason)
      parts.push(
        t("v.det.reason", {
          device: job.device.toUpperCase(),
          reason: job.device_reason,
        }),
      );
    else if (job?.device) parts.push(`device ${job.device.toUpperCase()}`);
    if (job?.load_ms != null)
      parts.push(`model loaded in ${fmt(job.load_ms)} ms`);
    else if (job?.device) parts.push("model already loaded");
    if (timings?.frames_per_s)
      parts.push(`${timings.frames_per_s.toFixed(1)} frames/s after the first`);
    $("v-timing-note").textContent = parts.join(" · ");
  }

  // --------------------------------------------------------- the results

  /** Open the selected segment in the player and show its last results */
  async function openSegment(frame) {
    const { s, seg } = selected();
    remember("session", s);
    remember("segment", seg);
    if (!s || !seg) return;
    await loadFrameCount(s, seg);
    if (selected().seg !== seg || selected().s !== s) return;
    const key = `${s}/${seg}`;
    if (vis.playerKey !== key && vis.frames != null) {
      vis.playerKey = key;
      vis.track = null;
      loadStage(s, seg);
      player.open(
        {
          frames: vis.frames,
          fps: summaryOf(s)?.fps || 30,
          frame: (n) => frameUrl(s, seg, n),
          title: `${s} · ${seg}`,
        },
        frame ?? 0,
      );
    } else if (frame != null) player.go(frame);
    loadResults(s, seg, frame);
  }

  async function loadResults(s, seg, frame) {
    let results;
    const all = $("v-all").checked ? "1" : "0";
    try {
      results = await api(
        `results?${new URLSearchParams({ s, seg, map: $("v-map").value, all })}`,
      );
    } catch (error) {
      $("v-results-note").textContent = error.message;
      return;
    }
    const now = selected();
    if (now.s !== s || now.seg !== seg) return;
    vis.results = results;
    vis.loadedKey = `${s}/${seg}`;
    vis.track = null;
    const { run, frames } = results;
    $("v-empty").hidden = frames.length > 0;
    // The processed frames are marks on the scrubber
    player.setMarks({
      ticks: frames.map((line) => ({ n: line.frame, kind: "model" })),
    });
    const hidden = Object.values(results.hidden ?? {}).reduce(
      (sum, n) => sum + n,
      0,
    );
    $("v-results-note").textContent = [
      run
        ? `${run.model_name} · ${frames.length} frames · ${new Date(run.started_ms).toLocaleString()}`
        : frames.length
          ? `${frames.length} frames`
          : "",
      hidden ? `${hidden} boxes of other classes hidden` : "",
    ]
      .filter(Boolean)
      .join(" · ");
    // The form starts from the run on screen, to run it again or change it
    if (run && !running(vis.job)) {
      $("v-start").value = run.start;
      $("v-step").value = run.step;
      $("v-count").value = run.count;
      describeRange();
    }
    renderClasses();
    renderTracks();
    renderJob();
    $("v-sent").hidden = true;
    $("v-sent").innerHTML = "";
    // The processed frame at or after the wanted one, if any
    const wanted = frame ?? player.frame;
    const line =
      frames.find((f) => f.frame >= wanted) ?? frames[frames.length - 1];
    if (line && line.frame !== player.frame) player.go(line.frame);
    else drawFrame(player.frame);
  }

  /** The results' line of frame n, if it was processed */
  const lineAt = (n) => vis.results?.frames.find((f) => f.frame === n) ?? null;

  /** The player shows frame n: its boxes, the trail, the hash */
  function drawFrame(n) {
    const frames = vis.results?.frames ?? [];
    const line = lineAt(n);
    const index = frames.indexOf(line);
    $("v-frame-chip").textContent = frames.length
      ? line
        ? `${index + 1}/${frames.length} processed`
        : "not processed"
      : "–";
    const ms = line?.ms;
    $("v-frame-note").textContent = line
      ? `${line.boxes.length} boxes` +
        (ms
          ? ` · decode ${fmt(ms.decode)} · network ${fmt(ms.network)} ms`
          : "")
      : "";
    drawBoxes(line?.boxes ?? []);
    drawTrail(n);
    const { s, seg } = selected();
    rememberView(replaceRoute("vision", { s, seg, n }));
  }

  /** The processed frame before (-1) or after (+1) the current one */
  function processed(direction) {
    const frames = vis.results?.frames ?? [];
    const n = player.frame;
    const line =
      direction < 0
        ? [...frames].reverse().find((f) => f.frame < n)
        : frames.find((f) => f.frame > n);
    if (line) player.go(line.frame);
  }

  const svgNs = "http://www.w3.org/2000/svg";
  function svg(tag, attrs, text) {
    const el = document.createElementNS(svgNs, tag);
    for (const [key, value] of Object.entries(attrs))
      el.setAttribute(key, value);
    if (text != null) el.textContent = text;
    return el;
  }

  function drawBoxes(boxes) {
    const layer = $("v-boxes");
    const scores = $("v-scores").checked;
    const trackedOnly = $("v-tracked").checked;
    layer.replaceChildren();
    for (const box of boxes) {
      if (trackedOnly && box.id == null) continue;
      const color = colorOf(box.class);
      const [x, y, w, h] = [box.x * W, box.y * H, box.w * W, box.h * H];
      const dim = vis.track != null && box.id !== vis.track;
      const group = svg("g", { class: "v-box", opacity: dim ? 0.35 : 1 });
      group.append(
        svg("rect", {
          x,
          y,
          width: w,
          height: h,
          stroke: color,
          "stroke-dasharray": box.id == null ? "4 3" : "none",
        }),
      );
      const label = [
        labelOf(box.class),
        scores && box.score != null ? box.score.toFixed(2) : null,
        box.id != null ? `#${box.id}` : null,
      ]
        .filter(Boolean)
        .join(" ");
      const ty = y > 14 ? y - 3 : y + h + 11;
      group.append(
        svg("rect", {
          class: "v-tag",
          x,
          y: ty - 10,
          width: label.length * 5.6 + 6,
          height: 13,
          fill: color,
        }),
        svg("text", { x: x + 3, y: ty }, label),
      );
      layer.append(group);
    }
  }

  function renderClasses() {
    const { classes, frames } = vis.results;
    $("v-classes").innerHTML = classes.length
      ? classes
          .map(
            (c) =>
              `<tr><td><i class="v-swatch" style="background:${colorOf(c.class)}"></i>${escapeHtml(labelOf(c.class))}</td><td class="num">${c.boxes}</td><td class="num">${c.frames}</td><td class="num">${c.mean_score.toFixed(2)}</td></tr>`,
          )
          .join("")
      : `<tr><td colspan="4" class="panel-note">${frames.length ? "No objects found." : "No results yet."}</td></tr>`;
    const boxes = classes.reduce((sum, c) => sum + c.boxes, 0);
    $("v-classes-note").textContent = frames.length
      ? `${boxes} boxes in ${frames.length} frames`
      : "";
  }

  // ---------------------------------------------------------- the tracks

  /** Each track's boxes by frame, from the results: id → {id, class,
   * points: [{f, x, y, box}]} with x, y the box center (fractions) */
  function indexTracks() {
    const index = new Map();
    for (const line of vis.results?.frames ?? []) {
      for (const box of line.boxes) {
        if (box.id == null) continue;
        let track = index.get(box.id);
        if (!track) {
          track = { id: box.id, class: box.class, points: [] };
          index.set(box.id, track);
        }
        track.points.push({
          f: line.frame,
          x: box.x + box.w / 2,
          y: box.y + box.h / 2,
          box,
        });
      }
    }
    vis.trackIndex = index;
  }

  function renderTracks() {
    const { tracks } = vis.results;
    indexTracks();
    $("v-tracks-note").textContent = tracks.length ? `${tracks.length}` : "";
    const body = $("v-tracks");
    body.replaceChildren();
    if (!tracks.length) {
      body.innerHTML = `<tr><td colspan="5" class="panel-note">${escapeHtml(t("v.tracks.none"))}</td></tr>`;
    }
    for (const track of tracks) {
      const tr = document.createElement("tr");
      tr.innerHTML = `<td class="num">#${track.id}</td><td><i class="v-swatch" style="background:${colorOf(track.class)}"></i>${escapeHtml(labelOf(track.class))}</td><td class="num">${track.frames}</td><td class="num">${track.first}</td><td class="num">${track.last}</td>`;
      if (vis.track === track.id) tr.className = "current";
      tr.onclick = () => selectTrack(vis.track === track.id ? null : track.id);
      body.append(tr);
    }
    drawCrops();
  }

  /** Highlight a track (null: none): the player goes to its first frame */
  function selectTrack(id) {
    vis.track = id;
    renderTracks();
    const first = vis.trackIndex.get(id)?.points[0]?.f;
    if (first != null) player.go(first);
    else drawFrame(player.frame);
  }

  /** Recent frames, for the trails' picture and the crops */
  const images = new Map();
  function frameImage(n) {
    const { s, seg } = selected();
    const url = frameUrl(s, seg, n);
    let img = images.get(url);
    if (!img) {
      img = new Image();
      img.loaded = new Promise((resolve) => {
        img.onload = () => resolve(true);
        img.onerror = () => resolve(false);
      });
      img.src = url;
      images.set(url, img);
      // The oldest go first
      if (images.size > TRAIL_IMAGES) images.delete(images.keys().next().value);
    }
    return img;
  }

  /** The canvas at the device's resolution, in frame units (W × H) */
  function trailContext() {
    const canvas = $("v-trail");
    const scale = Math.min(2, window.devicePixelRatio || 1);
    if (canvas.width !== W * scale) {
      canvas.width = W * scale;
      canvas.height = H * scale;
    }
    const ctx = canvas.getContext("2d");
    ctx.setTransform(scale, 0, 0, scale, 0, 0);
    return ctx;
  }

  /** The tracks over a frame: short trails around the playhead (n), or the
   * highlighted track's whole path over its middle frame */
  function drawTrail(n) {
    const canvas = $("v-trail");
    canvas.hidden = vis.frames == null;
    if (canvas.hidden) return;
    const focus = vis.trackIndex?.get(vis.track) ?? null;
    const background = focus
      ? focus.points[Math.floor(focus.points.length / 2)].f
      : n;
    const token = (vis.trailToken = {});
    const img = focus ? frameImage(background) : player.image(n);
    img.loaded.then((ok) => {
      if (vis.trailToken !== token) return;
      const ctx = trailContext();
      ctx.fillStyle = "#000000";
      ctx.fillRect(0, 0, W, H);
      if (ok) {
        ctx.drawImage(img, 0, 0, W, H);
        // Dimmed, so the trails stand out
        ctx.fillStyle = "rgb(0 0 0 / 0.35)";
        ctx.fillRect(0, 0, W, H);
      }
      if (focus) drawWholeTrack(ctx, focus, n, background);
      else drawRecentTrails(ctx, n);
    });
    const fps = player.fps || 30;
    $("v-trail-note").textContent = focus
      ? t("v.tracks.focus", {
          id: focus.id,
          label: labelOf(focus.class),
          n: focus.points.length,
          first: focus.points[0].f,
          last: focus.points[focus.points.length - 1].f,
          middle: background,
        })
      : vis.trackIndex?.size
        ? t("v.tracks.window", {
            s: TRAIL_S,
            frames: Math.round(TRAIL_S * fps),
          })
        : t(vis.results?.frames.length ? "v.tracks.none" : "v.tracks.empty");
  }

  /** A trail's line from point to point, faded by `alpha(point)` */
  function strokePath(ctx, points, color, width, alpha) {
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    for (let i = 1; i < points.length; i++) {
      const [a, b] = [points[i - 1], points[i]];
      ctx.globalAlpha = alpha(b);
      ctx.beginPath();
      ctx.moveTo(a.x * W, a.y * H);
      ctx.lineTo(b.x * W, b.y * H);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;
  }

  /** A track's id next to a point, readable on any picture */
  function tag(ctx, point, text, color) {
    const [x, y] = [point.x * W, point.y * H];
    ctx.font = "600 11px ui-monospace, monospace";
    ctx.lineWidth = 3;
    ctx.strokeStyle = "rgb(0 0 0 / 0.75)";
    ctx.fillStyle = "#ffffff";
    ctx.strokeText(text, x + 8, y + 4);
    ctx.fillText(text, x + 8, y + 4);
    ctx.beginPath();
    ctx.arc(x, y, 4, 0, 2 * Math.PI);
    ctx.fillStyle = color;
    ctx.fill();
  }

  /** Every track's trail within TRAIL_S of frame n: brightest at the
   * playhead, fading with the time away from it; the id where it is now
   * (or was last seen within the window) */
  function drawRecentTrails(ctx, n) {
    const span = TRAIL_S * (player.fps || 30);
    const alpha = (p) => Math.max(0.12, 1 - Math.abs(p.f - n) / span);
    for (const track of vis.trackIndex.values()) {
      const points = track.points.filter((p) => Math.abs(p.f - n) <= span);
      if (!points.length) continue;
      const color = colorOf(track.class);
      strokePath(ctx, points, color, 2.5, alpha);
      // The latest point up to the playhead, else the first after it
      const now = points.filter((p) => p.f <= n).pop() ?? points[0];
      ctx.globalAlpha = alpha(now);
      tag(ctx, now, `#${track.id}`, color);
      ctx.globalAlpha = 1;
    }
  }

  /** The highlighted track's whole path, its box on the picture's frame and
   * a ring where it is at the playhead (n) */
  function drawWholeTrack(ctx, track, n, background) {
    const color = colorOf(track.class);
    strokePath(ctx, track.points, color, 3, () => 0.95);
    const first = track.points[0];
    const last = track.points[track.points.length - 1];
    ctx.beginPath();
    ctx.arc(first.x * W, first.y * H, 3, 0, 2 * Math.PI);
    ctx.fillStyle = color;
    ctx.fill();
    ctx.beginPath();
    ctx.arc(last.x * W, last.y * H, 5, 0, 2 * Math.PI);
    ctx.lineWidth = 2;
    ctx.strokeStyle = color;
    ctx.stroke();
    const shown = track.points.find((p) => p.f === background);
    if (shown) {
      const b = shown.box;
      ctx.lineWidth = 2;
      ctx.strokeStyle = color;
      ctx.strokeRect(b.x * W, b.y * H, b.w * W, b.h * H);
    }
    const now = track.points.filter((p) => p.f <= n).pop();
    if (now && now !== shown) {
      ctx.setLineDash([4, 3]);
      ctx.strokeStyle = "#ffffff";
      ctx.lineWidth = 1.5;
      const b = now.box;
      ctx.strokeRect(b.x * W, b.y * H, b.w * W, b.h * H);
      ctx.setLineDash([]);
    }
    tag(ctx, shown ?? first, `#${track.id} ${labelOf(track.class)}`, color);
  }

  /** The highlighted track's boxes as small crops, spread over its frames:
   * what it followed; a crop goes to its frame */
  function drawCrops() {
    const box = $("v-crops");
    const track = vis.trackIndex?.get(vis.track);
    box.hidden = !track;
    box.replaceChildren();
    if (!track) return;
    const { points } = track;
    const count = Math.min(CROPS, points.length);
    for (let i = 0; i < count; i++) {
      const p =
        points[Math.round((i * (points.length - 1)) / Math.max(1, count - 1))];
      const button = document.createElement("button");
      button.type = "button";
      button.className = "v-crop";
      button.title = t("v.tracks.cropTitle", { n: p.f });
      const canvas = document.createElement("canvas");
      canvas.width = CROP_PX;
      canvas.height = CROP_PX;
      const caption = document.createElement("span");
      caption.className = "num";
      caption.textContent = p.f;
      button.append(canvas, caption);
      button.onclick = () => player.go(p.f);
      box.append(button);
      const img = frameImage(p.f);
      img.loaded.then((ok) => {
        if (!ok) return;
        // A square around the box, a little larger than it
        const { x, y, w, h } = p.box;
        const side =
          Math.max(w * img.naturalWidth, h * img.naturalHeight) * 1.3;
        const cx = (x + w / 2) * img.naturalWidth;
        const cy = (y + h / 2) * img.naturalHeight;
        canvas
          .getContext("2d")
          .drawImage(
            img,
            cx - side / 2,
            cy - side / 2,
            side,
            side,
            0,
            0,
            CROP_PX,
            CROP_PX,
          );
      });
    }
  }

  $("v-prev").onclick = () => processed(-1);
  $("v-next").onclick = () => processed(1);
  $("v-scores").onchange = () => drawFrame(player.frame);
  $("v-tracked").onchange = () => drawFrame(player.frame);

  /** Load the results again, through our classes or all of them */
  const reloadResults = () => {
    const { s, seg } = selected();
    if (s && seg) loadResults(s, seg);
  };
  $("v-all").onchange = reloadResults;
  $("v-map").onchange = () => {
    remember("map", $("v-map").value);
    reloadResults();
  };

  // The player has Space and the arrows; N and P jump between processed
  // frames
  document.addEventListener("keydown", (event) => {
    if (!vis.shown || !vis.results?.frames.length) return;
    const tag = event.target.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "n") processed(1);
    else if (key === "p") processed(-1);
    else return;
    event.preventDefault();
  });

  // ------------------------------------------------------- send to labels

  $("v-send-form").onsubmit = async (event) => {
    event.preventDefault();
    const { s, seg } = selected();
    const out = $("v-sent");
    const map = $("v-map").value;
    remember("map", map);
    $("v-send").disabled = true;
    try {
      const sent = await api("send", { s, seg, map });
      const dropped = Object.entries(sent.dropped)
        .map(([c, n]) => `${escapeHtml(c)} ${n}`)
        .join(", ");
      const frame = lineAt(player.frame)?.frame ?? sent.first ?? 0;
      const link = escapeHtml(
        appUrl("inspect", { s, seg, n: frame, label: 1 }),
      );
      out.innerHTML = `
        <p><b>${sent.boxes}</b> boxes written: ${sent.added} frames added, ${sent.replaced} replaced (model boxes only), ${sent.kept} kept (labeled by a person).</p>
        ${dropped ? `<p class="panel-note">Left out, not in classes.json: ${dropped}.</p>` : ""}
        <p class="panel-note path">${escapeHtml(sent.file)}</p>
        <a class="btn btn-small" href="${link}">Open frame ${frame} in the Inkspector's Label mode</a>`;
      out.hidden = false;
      loadOverview();
    } catch (error) {
      out.innerHTML = `<p class="notice">${escapeHtml(error.message)}</p>`;
      out.hidden = false;
    } finally {
      $("v-send").disabled = false;
    }
  };

  // ------------------------------------------------------------ the stage

  /** The segment's stage (from a Cuttlefish review of this segment, or
   * picked here, not saved): Gungee's map of it for reference, credited,
   * and the links to his viewers */
  const stageMap = new StageMap($("v-stage"), () => {}, { picture: true });

  async function loadStage(s, seg) {
    stageMap.set("");
    const stage = await stageOfVideo({ kind: "session", ref: `${s}/${seg}` });
    const now = selected();
    if (now.s === s && now.seg === seg) stageMap.set(stage);
  }

  window.addEventListener("lang-change", () => {
    const option = [...$("v-model").options].find(
      (o) => o.value === vis.info?.detector.model,
    );
    if (option) option.textContent = t("v.det.model");
    renderDetector();
    if (!vis.results) return;
    renderTracks();
    drawFrame(player.frame);
  });

  // -------------------------------------------------------------- routing

  /** The last view, for the app link after leaving or a reload */
  function rememberView(url) {
    document.querySelector('.app-nav [data-app="vision"]').href = url;
    remember("view", url);
  }

  async function route(state) {
    loadOverview();
    try {
      await Promise.all([loadInfo(), loadSessions()]);
    } catch (error) {
      // Aborted: the app was left, and routes again when shown
      return isAbort(error) || showError(error.message);
    }
    if (salmon()) checkDetector();
    const s = state.get("s") ?? remembered("session", "");
    const select = $("v-session");
    if (s && summaryOf(s)) select.value = s;
    fillSegments();
    const seg = state.get("seg") ?? remembered("segment", "");
    if (seg && [...$("v-segment").options].some((o) => o.value === seg)) {
      $("v-segment").value = seg;
    }
    const n = state.has("n") ? Number(state.get("n")) : undefined;
    const now = selected();
    const loaded = vis.results && vis.loadedKey === `${now.s}/${now.seg}`;
    if (!loaded) openSegment(n);
    else if (n != null && n !== player.frame) player.go(n);
    poll();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    vis.shown = app === "vision";
    player.enabled = vis.shown;
    if (vis.shown) route(state);
    else {
      clearTimeout(vis.pollTimer);
      player.pause();
    }
  });

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) player.pause();
    else if (vis.shown) poll();
  });

  showTab(remembered("tab", "dataset"));

  document.querySelector('.app-nav [data-app="vision"]').href = storedView(
    "vision",
    remembered("view"),
  );
})();
