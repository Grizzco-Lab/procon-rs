// Vision app: detect and track objects in a recorded segment, review the
// boxes frame by frame through our classes (classes.json; the COCO ones
// behind a toggle), and send them to the labels as model boxes. The
// classes panel opens on the labeled dataset. The segment plays in the
// shared player (player.js: the Inkspector's frames, keys, scrubber with
// the processed frames as marks, neighbours) with the boxes as a layer over
// it. Runs after app.js and player.js and uses their helpers ($,
// escapeHtml). Runs go through /api/vision (see src/vision.rs). State lives
// in the hash: #vision/s=<session>&seg=<file>&n=<frame>.
"use strict";

(() => {
  /** How often a running detection is asked about, in ms */
  const POLL_MS = 500;
  /** Frame size of the box coordinates in the SVGs */
  const W = 640;
  const H = 360;
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
    /** Track highlighted in the trail */
    track: null,
    pollTimer: null,
  };

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
              ? `<a href="#inspect/${new URLSearchParams({ s: s.session, seg: s.file, label: 1 })}">${escapeHtml(name)}</a>`
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
    select.value = remembered("model", info.size);
    if (!select.value) select.value = info.size;
    $("v-cpu-wrap").hidden = !info.cuda;
    $("v-cpu").checked = remembered("cpu", "false") === "true";
    $("v-track").checked = remembered("track", "true") === "true";
    $("v-map").value = remembered("map", "person=player");
  }

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
    const t = job?.timings;
    const rows = [
      ["Decode", t?.decode],
      ["Network", t?.network],
      ["Total", t?.total],
    ];
    $("v-timings").innerHTML = rows
      .map(
        ([name, stat]) =>
          `<tr><td>${name}</td><td class="num">${fmt(job?.done ? stat?.mean : null)}</td><td class="num">${fmt(job?.done ? stat?.p95 : null)}</td></tr>`,
      )
      .join("");
    const parts = [];
    if (job?.device) parts.push(`device ${job.device.toUpperCase()}`);
    if (job?.load_ms != null)
      parts.push(`model loaded in ${fmt(job.load_ms)} ms`);
    else if (job?.device) parts.push("model already loaded");
    if (t?.frames_per_s)
      parts.push(`${t.frames_per_s.toFixed(1)} frames/s after the first`);
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
    drawTrail(line);
    const { s, seg } = selected();
    history.replaceState(
      null,
      "",
      `#vision/${new URLSearchParams({ s, seg, n })}`,
    );
    rememberView();
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

  function renderTracks() {
    const { tracks } = vis.results;
    $("v-tracks-note").textContent = tracks.length ? `${tracks.length}` : "";
    const body = $("v-tracks");
    body.replaceChildren();
    if (!tracks.length) {
      body.innerHTML = `<tr><td colspan="5" class="panel-note">No tracks: run with Track on.</td></tr>`;
    }
    for (const t of tracks) {
      const tr = document.createElement("tr");
      tr.innerHTML = `<td class="num">#${t.id}</td><td><i class="v-swatch" style="background:${colorOf(t.class)}"></i>${escapeHtml(labelOf(t.class))}</td><td class="num">${t.frames}</td><td class="num">${t.first}</td><td class="num">${t.last}</td>`;
      if (vis.track === t.id) tr.className = "current";
      tr.onclick = () => {
        vis.track = vis.track === t.id ? null : t.id;
        renderTracks();
        if (vis.track != null) player.go(t.first);
        else drawFrame(player.frame);
      };
      body.append(tr);
    }
  }

  /** Every track's path on screen (box centers over time), the current
   * frame's positions marked when it was processed (`line`) */
  function drawTrail(line) {
    const layer = $("v-trail");
    layer.replaceChildren(
      svg("rect", { class: "v-trail-bg", x: 0, y: 0, width: W, height: H }),
    );
    const frames = vis.results?.frames ?? [];
    const paths = new Map();
    for (const line of frames) {
      for (const b of line.boxes) {
        if (b.id == null) continue;
        const path = paths.get(b.id) ?? { class: b.class, points: [] };
        path.points.push([(b.x + b.w / 2) * W, (b.y + b.h / 2) * H]);
        paths.set(b.id, path);
      }
    }
    for (const [id, path] of paths) {
      const color = colorOf(path.class);
      const focus = vis.track == null || vis.track === id;
      const points = path.points
        .map((p) => p.map((v) => v.toFixed(1)).join(","))
        .join(" ");
      layer.append(
        svg("polyline", {
          points,
          stroke: color,
          class: "v-trail-line",
          opacity: focus ? 0.85 : 0.15,
          "stroke-width": vis.track === id ? 3 : 1.5,
        }),
      );
      const [x, y] = path.points[0];
      layer.append(
        svg("circle", {
          cx: x,
          cy: y,
          r: 2.5,
          fill: color,
          opacity: focus ? 0.9 : 0.2,
        }),
      );
    }
    for (const b of line?.boxes ?? []) {
      if (b.id == null) continue;
      const [x, y] = [(b.x + b.w / 2) * W, (b.y + b.h / 2) * H];
      layer.append(
        svg("circle", {
          class: "v-trail-now",
          cx: x,
          cy: y,
          r: 6,
          stroke: colorOf(b.class),
        }),
        svg("text", { class: "v-trail-id", x: x + 8, y: y + 4 }, `#${b.id}`),
      );
    }
    if (!paths.size) {
      layer.append(
        svg(
          "text",
          { class: "v-trail-empty", x: W / 2, y: H / 2 },
          frames.length ? "No tracks in these results" : "Tracks appear here",
        ),
      );
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
      const link = `#inspect/${new URLSearchParams({ s, seg, n: frame, label: 1 })}`;
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

  // -------------------------------------------------------------- routing

  function rememberView() {
    const hash = location.hash.startsWith("#vision")
      ? location.hash
      : "#vision";
    document.querySelector('.app-nav [data-app="vision"]').href = hash;
    remember("view", hash);
  }

  async function route(state) {
    loadOverview();
    try {
      await Promise.all([loadInfo(), loadSessions()]);
    } catch (error) {
      return showError(error.message);
    }
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

  document.querySelector('.app-nav [data-app="vision"]').href = remembered(
    "view",
    "#vision",
  );
})();
