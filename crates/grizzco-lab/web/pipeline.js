// Pipeline app: the GPUs and the experiment queue, live. The queue is a
// file the agents keep, AgentZero's runs/queue.json (agentzero-queue writes
// it; AgentZero's README has the format): what runs, what waits in the order
// it runs, why each entry runs (the question it answers) and what came out.
// Two GPUs take its entries: this host's (Linux) and the win11 VM's, whose
// runner (agentzero-win11 run) runs the entries it takes there (host: win11)
// and writes the VM's GPU to runs/win11/gpu.json. The lab samples both GPUs,
// the CPU and the machine every 5 s and follows each entry's processes here
// or its runner there, and its run folder (src/pipeline.rs). The page shows:
//
// - Running: a card per running entry, labelled with the GPU it runs on,
//   with its progress and ETA (or, once its training ended or its step
//   stopped moving, what it does now and for how long), the loss curve of
//   its run folder, its latest validation and copycat scores, its processes
//   and its log; also entries whose processes run although the queue still
//   says queued, entries the queue says run but whose processes are gone,
//   and entries on the VM whose runner has gone quiet;
// - Machine: this host's GPU, CPU and memory and the VM's GPU as tiles with
//   the last half hour, and each GPU's processes with their entries;
// - GPU timeline: per GPU, busy % (this host's with its CPU over it),
//   memory, and lanes of what ran on it when (each bar with its job's CPU),
//   over the last 1 to 12 hours; stretches without a reading shaded;
// - Queue: the waiting entries in the order they run, what each waits for
//   and which runner takes it next, reordered by dragging the handle (or
//   ↑ ↓ on it), written back as priorities (POST order), which agents
//   follow (`agentzero-queue next`);
// - Results and History: what came out, newest first, and entries whose run
//   folder reached its last step (or stopped early) before the queue said
//   so;
// - a banner over it all while the Proxmox pool every VM's disk lives on
//   (rpool) runs low or has no fresh reading (state's `storage`; the top
//   bar's chip in app.js says the same in every app).
//
// Timeline rows (GET timeline, and state's samples) are arrays: 0 t, 1 busy
// %, 2 memory MiB, 3 the queue's jobs' memory, 4 °C, 5 W, 6 CPU %, 7 RAM
// MiB, 8 the remote GPU's busy %, 9 its memory, 10 its °C, 11 its W, 12
// load, 13 {entry id: its CPU in cores}, 14 {disk: free GB} (pve:rpool,
// linux:/, win11:C:, those read); null where nothing was read.
//
// It polls while shown (the state every 5 s, the running entries' curves
// every 10 s) and stops while another app is shown or the tab is hidden.
// Runs after app.js and uses its helpers ($, t, i18nLocale, setChip-like
// chips of its own). Every text from the queue goes in with textContent.
"use strict";

(() => {
  /** How often the state is asked for, in ms */
  const POLL_MS = 5000;
  /** How often a running entry's curve is read again, in ms */
  const CURVE_MS = 30000;
  /** How often the whole timeline window is read again, in ms */
  const TIMELINE_MS = 5 * 60 * 1000;
  /** How often an open log is read again, in ms */
  const LOG_MS = 5000;
  /** The timeline's windows, in minutes */
  const RANGES = [60, 180, 360, 720];
  /** Minutes of the machine the tiles' sparklines show */
  const SPARK_MINUTES = 30;
  /** Finished entries the results list shows */
  const RESULTS = 8;
  /** History rows shown before "Show all" */
  const HISTORY = 12;
  /** Most lanes of entries under the timeline */
  const MAX_LANES = 7;
  /** Validation scores shown for a run, in this order when present */
  const SCORES = [
    "button_f1",
    "onset_f1",
    "turn_corr_x",
    "turn_corr_y",
    "gyro_corr",
    "stick_bin_accuracy",
  ];
  /** Copycat scores of AgentZero's policy (what it gets right beyond
   * repeating the present), the headline ones, each with the companions
   * its tooltip lists */
  const COPYCAT = [
    [
      "keyframe_button_acc",
      ["keyframe_onset_recall", "keyframe_release_recall"],
    ],
    [
      "anticipation_left_x",
      ["anticipation_left_y", "anticipation_turn_x", "anticipation_turn_y"],
    ],
    ["turn_corr_x_500ms", ["turn_corr_y_500ms"]],
    ["press_f1", ["frame_f1_tolerant", "onset_f1_wide", "hold_iou"]],
  ];
  /** A step unmoved this long means another phase, if the lab does not say */
  const STALL_MS = 5 * 60 * 1000;
  const SVG_NS = "http://www.w3.org/2000/svg";

  const pl = {
    /** Whether the app is shown */
    shown: false,
    /** The last state (GET state) */
    state: null,
    error: null,
    timer: null,
    /** A poll under way */
    polling: false,
    /** The timeline: its window, rows [t, util, mem, jobs, temp, power,
     * cpu, ram] and when each entry was seen running */
    minutes: remembered("range", 180),
    samples: [],
    spans: {},
    timelineAt: 0,
    samplingSince: null,
    /** Run folders' curves by entry id: {data, at, pending} */
    runs: new Map(),
    /** Logs open by entry id: {lines, at} */
    logs: new Map(),
    /** Result items opened */
    openResults: new Set(),
    /** Whether the whole history is shown */
    allHistory: false,
    /** A drag of a queued entry under way */
    drag: null,
    /** The waiting order while it is being saved (ids), its timer, whether
     * a save is under way, and what the queue's note says meanwhile */
    order: null,
    orderTimer: null,
    saving: false,
    orderNote: "",
    /** Opened results' details by entry id, kept so their charts stay */
    details: new Map(),
    /** The remote GPU's name, kept while its runner cannot read it */
    remoteName: "",
  };

  /** Whether a list's content changed since it was drawn: its signature,
   * with the page's language, against the one kept on its box */
  function changed(box, content) {
    const signature = JSON.stringify([i18nLang(), content]);
    if (box.dataset.signature === signature) return false;
    box.dataset.signature = signature;
    return true;
  }

  // Charts redraw at the width of their box
  const sized = new ResizeObserver((entries) => {
    for (const { target } of entries) {
      const width = Math.round(target.clientWidth);
      if (width && target.dataset.width !== String(width)) {
        target.dataset.width = String(width);
        target.redraw?.();
      }
    }
  });

  function remembered(key, fallback) {
    try {
      const value = Number(localStorage.getItem(`procon-pipeline-${key}`));
      return RANGES.includes(value) ? value : fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-pipeline-${key}`, String(value));
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  async function api(path, body) {
    const response = await fetch(
      `/api/pipeline/${path}`,
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

  // ------------------------------------------------------------ elements

  /** An HTML element with attributes and children (strings as text) */
  function el(tag, attrs = {}, ...kids) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (value == null || value === false) continue;
      if (key === "class") node.className = value;
      else if (key === "text") node.textContent = value;
      else if (key === "style") node.style.cssText = value;
      else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? "" : value);
    }
    for (const kid of kids.flat()) {
      if (kid == null || kid === false) continue;
      node.append(kid instanceof Node ? kid : document.createTextNode(kid));
    }
    return node;
  }

  /** An SVG element with attributes and children */
  function svg(tag, attrs = {}, ...kids) {
    const node = document.createElementNS(SVG_NS, tag);
    for (const [key, value] of Object.entries(attrs)) {
      if (value != null && value !== false) node.setAttribute(key, value);
    }
    for (const kid of kids.flat()) {
      if (kid == null || kid === false) continue;
      node.append(kid instanceof Node ? kid : document.createTextNode(kid));
    }
    return node;
  }

  // ------------------------------------------------------------ formatting

  const locale = () => i18nLocale();

  /** A number in the page's language, `digits` after the point */
  function num(value, digits = 0) {
    if (value == null || !Number.isFinite(value)) return "–";
    return value.toLocaleString(locale(), {
      minimumFractionDigits: digits,
      maximumFractionDigits: digits,
    });
  }

  /** MiB as GiB with one decimal */
  const gib = (mib) => `${num(mib / 1024, 1)} GiB`;

  /** Bytes as GiB with one decimal */
  const gibBytes = (bytes) => gib(bytes / 2 ** 20);

  /** 13:05 (the day too when not today) */
  function clock(ms, withDay = false) {
    if (ms == null) return "–";
    const date = new Date(ms);
    const time = date.toLocaleTimeString(locale(), {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
    const today = new Date().toDateString() === date.toDateString();
    if (today && !withDay) return time;
    const day = date.toLocaleDateString(locale(), {
      month: "short",
      day: "numeric",
    });
    return `${day} ${time}`;
  }

  /** A time axis's label: 13:05, or the date where a day starts */
  function tickLabel(ms) {
    const date = new Date(ms);
    if (date.getHours() === 0 && date.getMinutes() === 0)
      return date.toLocaleDateString(locale(), {
        month: "short",
        day: "numeric",
      });
    return date.toLocaleTimeString(locale(), {
      hour: "2-digit",
      minute: "2-digit",
      hour12: false,
    });
  }

  /** A duration: "1 h 12 min", "12 min", "45 s" */
  function duration(ms) {
    if (ms == null || !Number.isFinite(ms)) return "–";
    const s = Math.max(0, Math.round(ms / 1000));
    if (s < 60) return t("pl.dur.s", { s });
    const m = Math.round(s / 60);
    if (m < 60) return t("pl.dur.m", { m });
    return t("pl.dur.h", { h: Math.floor(m / 60), m: m % 60 });
  }

  /** A duration as a ticking clock: 1:02:05 or 2:05 */
  function stopwatch(ms) {
    const s = Math.max(0, Math.floor(ms / 1000));
    const pad = (n) => String(n).padStart(2, "0");
    const h = Math.floor(s / 3600);
    const m = Math.floor(s / 60) % 60;
    return h ? `${h}:${pad(m)}:${pad(s % 60)}` : `${m}:${pad(s % 60)}`;
  }

  /** "5 min ago", "in 2 h" */
  function ago(ms) {
    if (ms == null) return "–";
    const seconds = (ms - Date.now()) / 1000;
    const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: "auto" });
    const abs = Math.abs(seconds);
    if (abs < 45) return rtf.format(Math.round(seconds), "second");
    if (abs < 3600) return rtf.format(Math.round(seconds / 60), "minute");
    if (abs < 86400 * 2) return rtf.format(Math.round(seconds / 3600), "hour");
    return rtf.format(Math.round(seconds / 86400), "day");
  }

  /** Round a range up to 1, 2 or 5 times a power of ten */
  function niceCeil(value) {
    if (!(value > 0)) return 1;
    const power = 10 ** Math.floor(Math.log10(value));
    return [1, 2, 2.5, 5, 10].map((m) => m * power).find((v) => v >= value);
  }

  /** Clean ticks between lo and hi, about `count` of them */
  function ticks(lo, hi, count) {
    const span = hi - lo;
    if (!(span > 0)) return [lo];
    const step = niceCeil(span / Math.max(1, count));
    const out = [];
    for (let v = Math.ceil(lo / step) * step; v <= hi + step * 1e-9; v += step)
      out.push(Number(v.toPrecision(12)));
    return out;
  }

  /** Steps as 1.2k, 30k */
  function steps(value) {
    if (value == null) return "–";
    if (Math.abs(value) >= 1000) {
      const k = value / 1000;
      return `${num(k, Number.isInteger(k) || k >= 100 ? 0 : 1)}k`;
    }
    return num(value);
  }

  // ------------------------------------------------------------ the entries

  /** Whether an entry's work runs: its processes here, or, on another
   * machine, its runner says so; null while that runner's file tells
   * nothing of it (stale, or older than the entry's start) */
  function aliveOf(entry) {
    const remote = entry.remote;
    if (remote) return remote.running || (remote.known ? false : null);
    return Boolean(entry.live?.pids?.length);
  }

  /** Whether a run is over: its trainer closed it, or all its steps ran */
  const completeOf = (p) =>
    Boolean(p && (p.ended || (p.total != null && p.step >= p.total)));

  /** Whether a run ended before its last step (no better validation) */
  const earlyOf = (p) =>
    Boolean(p?.ended && p.total != null && p.step < p.total);

  /**
   * What the page makes of an entry: its place (running, waiting, finished)
   * and its state word, from the queue's status and what the lab saw:
   * processes alive (`live`, or its runner's word on another machine), and
   * a run folder that ended or reached its last step
   */
  function view(entry) {
    const alive = aliveOf(entry);
    const progress = entry.progress;
    const complete = completeOf(progress);
    const ended = earlyOf(progress) ? "early" : "finished";
    const status = entry.status;
    if (alive)
      return {
        place: "running",
        state: status === "running" ? "running" : "detected",
      };
    // Said to run, nothing of it runs: over if its run ended; unknown while
    // its machine's runner is quiet; else something to look at
    if (status === "running") {
      if (complete) return { place: "finished", state: ended };
      return { place: "running", state: alive === null ? "unknown" : "gone" };
    }
    // Waiting, yet its run folder has rows and nothing of it runs: it ran
    // (to the end, stopped early, or broke off) before the queue was told
    if ((status === "queued" || status === "paused") && progress)
      return { place: "finished", state: complete ? ended : "stopped" };
    if (status === "queued" || status === "paused")
      return { place: "waiting", state: status };
    // Done, failed, or a word of the owner's own: over
    return { place: "finished", state: status ?? "done" };
  }

  /**
   * What a run does now: `steps` (training, counted), `stalled` (live, its
   * step unmoved for a while), `after` (live, its training over: another
   * step of its job runs), `ended` (over, nothing runs), or null without
   * progress to read
   */
  function phaseOf(entry, state) {
    const p = entry.progress;
    if (!p) return null;
    const live = state === "running" || state === "detected";
    if (p.ended) return live ? "after" : "ended";
    const stall = pl.state?.stall_ms ?? STALL_MS;
    if (live && p.moved_ms != null && Date.now() - p.moved_ms > stall)
      return "stalled";
    return completeOf(p) && !live ? "ended" : "steps";
  }

  /** A GPU's name without its maker's words: "RTX 4080 SUPER" */
  const gpuName = (name) =>
    (name ?? "").replace(/^NVIDIA\s+/i, "").replace(/^GeForce\s+/i, "");

  /** A machine and its GPU in words: "Linux · RTX 4070 SUPER" */
  function machineTitle(host, gpu) {
    const name = gpuName(gpu);
    if (!host) return t("pl.where.local", { gpu: name || "GPU" });
    return name ? t("pl.where.remote", { host, gpu: name }) : host;
  }

  /** The remote GPU's name, or the last one seen */
  const remoteGpu = () => pl.state?.remote?.gpu?.name || pl.remoteName;

  /** Where an entry runs, in words: its machine and GPU, or the CPU */
  function whereOf(entry) {
    if (entry.remote)
      return machineTitle(
        entry.remote.host,
        pl.state?.remote?.host === entry.remote.host ? remoteGpu() : "",
      );
    if (entry.device === "cpu") return t("pl.device.cpu");
    return machineTitle(null, pl.state?.gpu?.name);
  }

  /** A device in words: "either GPU", "win11 GPU", "CPU" */
  function deviceWord(device) {
    if (!device) return null;
    const key = `pl.device.${device}`;
    const word = t(key);
    if (word !== key) return word;
    const host = device.startsWith("gpu:") ? device.slice(4) : null;
    return host ? t("pl.device.host", { host }) : device;
  }

  /** When an entry ended, as well as the page knows */
  const endedAt = (entry) =>
    entry.ended_ms ??
    entry.seen?.last_ms ??
    entry.progress?.updated_ms ??
    entry.started_ms ??
    null;

  /** How long an entry took, from its start (the file's, else when the
   * lab first saw it) to its end: "≥ 5 min" when the lab only saw it
   * from its own start */
  function tookOf(entry) {
    const ended = endedAt(entry);
    const began = entry.started_ms ?? entry.seen?.first_ms;
    if (ended == null || began == null || ended < began) return null;
    const partial =
      entry.started_ms == null &&
      pl.samplingSince != null &&
      began - pl.samplingSince < 15000;
    return `${partial ? "≥ " : ""}${duration(ended - began)}`;
  }

  /** When an entry started, as well as the page knows */
  const startedAt = (entry) =>
    entry.live?.since_ms ?? entry.started_ms ?? entry.seen?.first_ms ?? null;

  /** The groups of the queue in name order: each takes a colour slot */
  function groupSlots(entries) {
    const names = [...new Set(entries.map((e) => e.group).filter(Boolean))];
    names.sort();
    return new Map(names.map((name, i) => [name, (i % 8) + 1]));
  }

  /** The group's chip: its name on its colour's mark */
  function groupChip(entry, slots) {
    if (!entry.group) return null;
    return el(
      "span",
      {
        class: "pl-group",
        style: `--g: var(--pl-c${slots.get(entry.group) ?? 1})`,
        title: t("pl.group", { group: entry.group }),
      },
      entry.group,
    );
  }

  /** A state in words; a status the page does not know as it is */
  function stateWord(state) {
    const key = `pl.state.${state}`;
    const word = t(key);
    return word === key ? String(state) : word;
  }

  /** The state's badge: icon and word */
  function stateBadge(state, note) {
    return el(
      "span",
      { class: "pl-state", "data-state": state, title: note ?? null },
      el("i", { "aria-hidden": "true" }),
      stateWord(state),
    );
  }

  // ------------------------------------------------------------ polling

  async function poll() {
    clearTimeout(pl.timer);
    // One poll at a time: the one under way sets the next
    if (pl.polling || !pl.shown || document.hidden) return;
    pl.polling = true;
    try {
      if (!pl.samples.length || Date.now() - pl.timelineAt > TIMELINE_MS)
        await loadTimeline();
      const since = pl.samples.length
        ? pl.samples[pl.samples.length - 1][0]
        : 0;
      const state = await api(
        `state${pl.samples.length ? `?since=${since}` : ""}`,
      );
      pl.error = null;
      pl.state = state;
      if (state.remote?.gpu?.name) pl.remoteName = state.remote.gpu.name;
      addSamples(state.samples ?? []);
      pl.samplingSince = state.sampling_since_ms ?? pl.samplingSince;
      render();
      refreshCurves();
      refreshLogs();
    } catch (error) {
      pl.error = error.message;
      renderError();
    }
    pl.polling = false;
    clearTimeout(pl.timer);
    if (pl.shown && !document.hidden) pl.timer = setTimeout(poll, POLL_MS);
  }

  /** The whole timeline window, averaged down by the lab */
  async function loadTimeline() {
    const data = await api(`timeline?minutes=${pl.minutes}`);
    pl.samples = data.samples;
    pl.spans = data.spans ?? {};
    pl.samplingSince = data.sampling_since_ms;
    pl.timelineAt = Date.now();
  }

  /** New samples at the end of the window; the oldest leave it */
  function addSamples(rows) {
    const last = pl.samples.length ? pl.samples[pl.samples.length - 1][0] : 0;
    for (const row of rows) if (row[0] > last) pl.samples.push(row);
    const from = Date.now() - pl.minutes * 60000;
    while (pl.samples.length && pl.samples[0][0] < from) pl.samples.shift();
  }

  /** Read the run folders of the running entries again, and of the
   * finished ones opened in Results once */
  function refreshCurves() {
    const entries = pl.state?.queue.entries ?? [];
    for (const entry of entries) {
      if (!entry.run_dir) continue;
      const { place } = view(entry);
      const wanted =
        place === "running" ||
        (place === "finished" && pl.openResults.has(entry.id));
      if (!wanted) continue;
      const known = pl.runs.get(entry.id);
      const stale =
        !known ||
        (place === "running" && Date.now() - known.at > CURVE_MS - 500);
      if (stale && !known?.pending) loadCurve(entry.id);
    }
  }

  async function loadCurve(id) {
    const known = pl.runs.get(id) ?? { data: null, at: 0 };
    known.pending = true;
    pl.runs.set(id, known);
    try {
      known.data = await api(`run?id=${encodeURIComponent(id)}`);
      known.error = null;
    } catch (error) {
      known.error = error.message;
    }
    known.at = Date.now();
    known.pending = false;
    for (const box of document.querySelectorAll(
      `[data-curve="${CSS.escape(id)}"]`,
    ))
      box.redraw?.();
    for (const box of document.querySelectorAll(
      `[data-scores="${CSS.escape(id)}"]`,
    ))
      box.redraw?.();
  }

  /** Logs are read while one is open (`open`) or a card shows its last
   * line (`tail`); the others are forgotten */
  function refreshLogs() {
    for (const [id, log] of pl.logs) {
      if (!log.open && !log.tail) pl.logs.delete(id);
      else if (!log.pending && Date.now() - log.at > LOG_MS - 500) loadLog(id);
    }
  }

  /** The log kept for an entry, made when missing */
  function logOf(id) {
    let log = pl.logs.get(id);
    if (!log) {
      log = { lines: null, at: 0, open: false, tail: false };
      pl.logs.set(id, log);
    }
    return log;
  }

  async function loadLog(id) {
    const log = logOf(id);
    log.pending = true;
    try {
      const data = await api(`log?id=${encodeURIComponent(id)}`);
      log.lines = data.lines;
      log.path = data.path;
      log.modified = data.modified_ms;
      log.error = null;
    } catch (error) {
      log.error = error.message;
    }
    log.at = Date.now();
    log.pending = false;
    for (const pre of document.querySelectorAll(
      `[data-log="${CSS.escape(id)}"]`,
    ))
      fillLog(pre, log);
    for (const box of document.querySelectorAll(
      `[data-last="${CSS.escape(id)}"]`,
    ))
      showLastLine(box, log);
  }

  function fillLog(pre, log) {
    if (log.error) pre.textContent = log.error;
    else if (!log.lines) pre.textContent = t("pl.log.loading");
    else pre.textContent = log.lines.join("\n") || " ";
    // Keep the newest line in view, unless the reader scrolled up
    if (!pre.dataset.scrolled) pre.scrollTop = pre.scrollHeight;
  }

  // ------------------------------------------------------------ render

  function render() {
    const state = pl.state;
    if (!state) return;
    const entries = state.queue.entries;
    const slots = groupSlots(entries);
    renderRunning(entries, slots);
    renderMachine(state);
    renderTimeline();
    if (!pl.drag && !pl.order) renderQueue(entries, slots);
    renderResults(entries, slots);
    renderHistory(entries, slots);
    renderChip(state);
    renderStorage(state.storage);
  }

  /** The banner over the page while the Proxmox pool every VM's disk lives
   * on runs low, or has no fresh reading; its title lists every disk
   * (storageLines in app.js) */
  function renderStorage(storage) {
    const banner = $("pl-storage");
    const level = storage?.level;
    banner.hidden = !level || level === "ok";
    if (banner.hidden) return;
    const pool = storage.pools.find((p) => p.name === storage.watched);
    banner.dataset.level = level === "critical" ? "critical" : "warning";
    banner.textContent = [
      pool
        ? t(`pl.storage.banner.${level}`, {
            pool: pool.name,
            host: storage.host,
            free: formatBytes(pool.free),
            size: formatBytes(pool.size),
            cap: Math.round(pool.cap),
          })
        : t("pl.storage.banner.unknown", {
            pool: storage.watched,
            host: storage.host,
            error: storage.error ?? t("pl.storage.noAnswer"),
          }),
      t("pl.storage.banner.why"),
      // Chinese sentences follow one another without a space
    ].join(i18nLang() === "zh" ? "" : " ");
    banner.title = storageLines(storage).join("\n");
  }

  function renderError() {
    const note = $("pl-now-note");
    note.textContent = t("pl.offline", { error: pl.error });
    note.classList.add("level-critical");
  }

  function renderChip(state) {
    const chip = $("pl-chip");
    const running = state.queue.entries.filter((e) => aliveOf(e));
    const gone = state.queue.entries.filter((e) => view(e).state === "gone");
    const remote = state.remote;
    const parts = [];
    if (state.gpu?.util != null)
      parts.push(t("pl.chip.gpu", { util: num(state.gpu.util) }));
    if (remote?.fresh && remote.gpu?.util != null)
      parts.push(
        t("pl.chip.remote", { host: remote.host, util: num(remote.gpu.util) }),
      );
    parts.push(
      running.length
        ? t("pl.chip.running", { n: running.length })
        : t("pl.chip.idle"),
    );
    chip.hidden = false;
    chip.dataset.level = gone.length
      ? "warning"
      : running.length
        ? "good"
        : "off";
    chip.querySelector(".chip-text").textContent = parts.join(" · ");
    chip.title = [
      state.gpu?.name,
      remote ? machineTitle(remote.host, remoteGpu()) : null,
      ...running.map((e) => `${e.id}: ${e.title}`),
    ]
      .filter(Boolean)
      .join("\n");
  }

  // ------------------------------------------------------------ running

  /** The running cards, kept by entry so charts and open logs stay */
  function renderRunning(entries, slots) {
    const box = $("pl-running");
    const running = entries.filter((e) => view(e).place === "running");
    // Those that run first, the ones to look at after them
    const doubtful = (e) => ["gone", "unknown"].includes(view(e).state);
    running.sort(
      (a, b) =>
        doubtful(a) - doubtful(b) || (startedAt(a) ?? 0) - (startedAt(b) ?? 0),
    );
    const note = $("pl-now-note");
    note.classList.remove("level-critical");
    note.textContent = running.length
      ? t("pl.now.count", { n: running.length })
      : "";
    if (!running.length) {
      for (const card of box.querySelectorAll(".pl-run"))
        sized.unobserve(card.parts.chart.plot);
      const next = entries
        .filter((e) => view(e).place === "waiting" && e.next_on?.length)
        .sort((a, b) => (a.rank ?? 1e9) - (b.rank ?? 1e9))[0];
      box.replaceChildren(
        el(
          "div",
          { class: "pl-idle" },
          svg(
            "svg",
            {
              class: "app-icon pl-idle-icon",
              viewBox: "0 0 24 24",
              "aria-hidden": "true",
            },
            svg("use", { href: "/icons/app-pipeline.svg#i" }),
          ),
          el("p", { class: "pl-idle-text", text: t("pl.idle") }),
          pl.state.gpu?.util >= 50
            ? el("p", {
                class: "pl-idle-next",
                text: t("pl.idleBusy", { util: num(pl.state.gpu.util) }),
              })
            : null,
          next
            ? el("p", {
                class: "pl-idle-next",
                text: t("pl.idleNext", { title: `${next.id}: ${next.title}` }),
              })
            : null,
        ),
      );
      return;
    }
    const cards = new Map(
      [...box.querySelectorAll(".pl-run")].map((c) => [c.dataset.id, c]),
    );
    const kept = [];
    for (const entry of running) {
      const card = cards.get(entry.id) ?? runCard(entry.id);
      cards.delete(entry.id);
      fillRunCard(card, entry, slots);
      kept.push(card);
    }
    for (const gone of cards.values()) sized.unobserve(gone.parts.chart.plot);
    box.replaceChildren(...kept);
  }

  /** A card's frame: parts filled on every state, the chart and log kept */
  function runCard(id) {
    const card = el("article", { class: "pl-run", "data-id": id });
    // Its parts, kept apart from the element's own properties (title...)
    card.parts = {
      head: el("header", { class: "pl-run-head" }),
      title: el("h3", { class: "pl-run-title" }),
      why: el("p", { class: "pl-why" }),
      warn: el("p", { class: "pl-run-warn" }),
      progress: el("div", { class: "pl-progress" }),
      chart: curveBox(id),
      scores: scoresBox(id),
      stats: el("div", { class: "pl-run-stats" }),
      lastLine: el("p", { class: "pl-last-line" }),
      more: el("div", { class: "pl-run-more" }),
    };
    card.append(...Object.values(card.parts));
    return card;
  }

  function fillRunCard(card, entry, slots) {
    const part = card.parts;
    const { state } = view(entry);
    card.dataset.state = state;
    const since = startedAt(entry);
    const clockSpan = el("span", {
      class: "pl-elapsed num",
      "data-since": since ?? "",
      title: since ? t("pl.startedAt", { clock: clock(since, true) }) : null,
    });
    clockSpan.textContent = since ? stopwatch(Date.now() - since) : "";
    part.head.replaceChildren(
      stateBadge(
        state,
        state === "detected"
          ? t("pl.state.detectedNote", { status: entry.status })
          : null,
      ),
      groupChip(entry, slots),
      el("code", { class: "pl-id", text: entry.id }),
      el("span", {
        class: "pl-where",
        "data-remote": entry.remote ? "1" : null,
        text: whereOf(entry),
        title: entry.device
          ? t("pl.where.device", { device: deviceWord(entry.device) })
          : null,
      }),
      entry.owner ? el("span", { class: "pl-meta", text: entry.owner }) : null,
      clockSpan,
    );
    part.title.textContent = entry.title || entry.id;
    part.why.textContent = entry.why;
    part.why.hidden = !entry.why;
    part.warn.textContent = warningOf(entry, state);
    part.warn.hidden = !part.warn.textContent;
    // A quiet runner that still runs is busy (copying a job's data over);
    // one that stopped is worth a warning
    const runnerDown = state === "unknown" && !pl.state?.remote?.runner;
    part.warn.dataset.level =
      state === "gone" || runnerDown ? "warning" : "note";
    fillProgress(part.progress, entry, state);
    part.chart.hidden = !entry.run_dir;
    if (entry.run_dir) part.scores.redraw();
    else part.scores.hidden = true;
    fillStats(part.stats, entry);
    fillLastLine(part.lastLine, entry, state);
    fillMore(part.more, entry);
  }

  /** What a card warns of: nothing of it runs, its runner has gone quiet,
   * it runs unmarked */
  function warningOf(entry, state) {
    const host = entry.remote?.host;
    const file = pl.state?.remote?.host === host ? pl.state.remote : null;
    if (state === "gone" && host)
      return file?.job
        ? t("pl.state.goneRemoteJob", { host, job: file.job })
        : t("pl.state.goneRemote", { host });
    if (state === "gone") return t("pl.state.goneNote");
    // A fresh file older than the entry: its runner took it and copies its
    // code and data over before it writes again
    if (state === "unknown" && file?.fresh)
      return t("pl.state.unknownStarting", { host });
    if (state === "unknown") {
      const said = file?.at_ms
        ? t("pl.state.unknownSince", {
            host,
            clock: clock(file.at_ms),
            ago: ago(file.at_ms),
          })
        : t("pl.state.unknownNever", { host });
      return file && !file.runner
        ? `${said} ${t("pl.state.runnerStopped")}`
        : said;
    }
    if (state === "detected")
      return t("pl.state.detectedNote", { status: entry.status });
    return "";
  }

  /** The log's last line under a live entry with no step to count, or
   * whose training is over or stalled: often the best word on what it does */
  function fillLastLine(box, entry, state) {
    const live = state === "running" || state === "detected";
    const phase = phaseOf(entry, state);
    const wanted = Boolean(
      live && entry.log && (!phase || phase === "after" || phase === "stalled"),
    );
    box.hidden = !wanted;
    if (!wanted) {
      if (pl.logs.has(entry.id)) pl.logs.get(entry.id).tail = false;
      return;
    }
    box.dataset.last = entry.id;
    const log = logOf(entry.id);
    log.tail = true;
    if (!log.lines && !log.pending) loadLog(entry.id);
    showLastLine(box, log);
  }

  function showLastLine(box, log) {
    const last = [...(log.lines ?? [])].reverse().find((line) => line.trim());
    box.replaceChildren(
      el("span", {
        class: "pl-stat-label",
        text: log.modified
          ? t("pl.log.lastAgo", { ago: ago(log.modified) })
          : t("pl.log.last"),
      }),
      el("code", { text: last ?? "…", title: last ?? null }),
    );
  }

  /** The bar, the step and the ETA; or, once the training is over or its
   * step has stopped moving, what runs now and for how long */
  function fillProgress(box, entry, state) {
    const p = entry.progress;
    const live = state === "running" || state === "detected";
    const phase = phaseOf(entry, state);
    if (phase === "after" || phase === "stalled" || phase === "ended")
      return fillPhase(box, p, phase, live);
    const fraction =
      p?.total > 0 ? Math.min(1, Math.max(0, p.step / p.total)) : null;
    const bar = el(
      "div",
      {
        class: "pl-bar",
        "data-live": live ? "1" : null,
        "data-unknown": fraction == null ? "1" : null,
        role: "progressbar",
        "aria-valuemin": "0",
        "aria-valuemax": "100",
        "aria-valuenow": fraction == null ? null : Math.round(fraction * 100),
      },
      el("div", {
        class: "pl-fill",
        style: fraction == null ? "" : `width: ${(fraction * 100).toFixed(2)}%`,
      }),
    );
    const facts = [];
    if (fraction != null)
      facts.push(el("b", { class: "pl-pct", text: `${num(fraction * 100)}%` }));
    if (p) {
      const counted =
        p.source === "log"
          ? t("pl.progress.items", { done: num(p.step), total: num(p.total) })
          : p.total != null
            ? t("pl.progress.steps", { step: num(p.step), total: num(p.total) })
            : t("pl.progress.step", { step: num(p.step) });
      facts.push(el("span", { text: counted }));
      const perStep = p.rate > 0 ? 1 / p.rate : p.s_per_step;
      if (p.source === "metrics" && perStep > 0)
        facts.push(
          el("span", {
            text:
              perStep < 1
                ? t("pl.progress.rate", { ms: num(perStep * 1000) })
                : t("pl.progress.rateS", { s: num(perStep, 1) }),
          }),
        );
      if (p.lr != null)
        facts.push(el("span", { text: `lr ${p.lr.toExponential(1)}` }));
      if (p.source === "log" && p.rate > 0)
        facts.push(
          el("span", {
            text: t("pl.progress.perMin", { n: num(p.rate * 60) }),
          }),
        );
    } else if (live) {
      facts.push(el("span", { class: "pl-dim", text: t("pl.progress.none") }));
    }
    const eta = p?.eta_ms ?? (live ? entry.eta_ms : null);
    if (live && eta)
      facts.push(
        el(
          "span",
          { class: "pl-eta", "data-eta": eta },
          el("b", { text: t("pl.progress.eta", { clock: clock(eta) }) }),
          " ",
          el("span", {
            class: "pl-left",
            text: t("pl.progress.left", { left: duration(eta - Date.now()) }),
          }),
        ),
      );
    if (!live && p?.updated_ms)
      facts.push(
        el("span", {
          class: "pl-dim",
          text: t("pl.progress.written", { ago: ago(p.updated_ms) }),
        }),
      );
    box.replaceChildren(bar, el("div", { class: "pl-progress-row" }, facts));
  }

  /**
   * A run no longer counting steps: `after` (its training is over and
   * another step of its job runs: a band that keeps going, and for how
   * long), `stalled` (the bar where its step stopped, and for how long) or
   * `ended` (the bar where it stopped, early or at its last step)
   */
  function fillPhase(box, p, phase, live) {
    const fraction =
      p.total > 0 ? Math.min(1, Math.max(0, p.step / p.total)) : null;
    const counted = phase !== "after" && fraction != null;
    const bar = el(
      "div",
      {
        class: "pl-bar",
        "data-phase": phase,
        "data-live": phase === "after" ? "1" : null,
        "data-unknown": counted ? null : "1",
        role: "progressbar",
        "aria-valuemin": "0",
        "aria-valuemax": "100",
        "aria-valuenow": counted ? Math.round(fraction * 100) : null,
      },
      el("div", {
        class: "pl-fill",
        style: counted ? `width: ${(fraction * 100).toFixed(2)}%` : "",
      }),
    );
    const since = p.moved_ms != null ? duration(Date.now() - p.moved_ms) : "";
    const steps = { step: num(p.step), total: num(p.total) };
    const how =
      p.total == null ? "At" : earlyOf(p) ? "Early" : completeOf(p) ? "" : "At";
    const facts = [];
    if (phase === "after") {
      facts.push(
        el("b", {
          class: "pl-phase",
          text: since
            ? t("pl.phase.after", { time: since })
            : t("pl.phase.afterNow"),
        }),
        el("span", { text: t(`pl.phase.trained${how}`, steps) }),
      );
    } else if (phase === "stalled") {
      if (fraction != null)
        facts.push(
          el("b", { class: "pl-pct", text: `${num(fraction * 100)}%` }),
        );
      facts.push(
        el("span", {
          text:
            p.source === "log"
              ? t("pl.progress.items", { done: steps.step, total: steps.total })
              : t("pl.progress.steps", steps),
        }),
        el("b", {
          class: "pl-phase",
          text: t("pl.phase.stalled", { time: since }),
        }),
      );
    } else {
      facts.push(
        el("b", { class: "pl-phase", text: t(`pl.phase.ended${how}`, steps) }),
      );
    }
    if (p.best_step != null && phase !== "stalled")
      facts.push(
        el("span", { text: t("pl.phase.best", { step: num(p.best_step) }) }),
      );
    if (!live && p.updated_ms)
      facts.push(
        el("span", {
          class: "pl-dim",
          text: t("pl.progress.written", { ago: ago(p.updated_ms) }),
        }),
      );
    box.replaceChildren(bar, el("div", { class: "pl-progress-row" }, facts));
  }

  /** Its processes: GPU memory, CPU, RAM; on another machine, that
   * machine's GPU as its runner reads it */
  function fillStats(box, entry) {
    const live = entry.live;
    const cells = [];
    const cell = (label, value, title) =>
      el(
        "span",
        { class: "pl-stat", title: title ?? null },
        el("span", { class: "pl-stat-label", text: label }),
        el("b", { class: "num", text: value }),
      );
    const remote = pl.state?.remote;
    if (entry.remote?.running && remote?.host === entry.remote.host) {
      const gpu = remote.gpu;
      const whole = t("pl.proc.wholeGpu", { host: remote.host });
      if (gpu?.util != null)
        cells.push(cell(t("pl.proc.gpuBusy"), `${num(gpu.util)}%`, whole));
      if (gpu?.mem_used_mib != null)
        cells.push(
          cell(
            t("pl.proc.gpu"),
            `${num(gpu.mem_used_mib / 1024, 1)} / ${gib(gpu.mem_total_mib ?? 0)}`,
            whole,
          ),
        );
      cells.push(
        cell(
          t("pl.proc.procs"),
          num(remote.processes.length),
          remote.processes.map((p) => `${p.name} ${p.pid ?? ""}`).join(", "),
        ),
        cell(t("pl.proc.read"), ago(remote.at_ms)),
      );
    }
    if (live) {
      if (live.gpu_mib > 0)
        cells.push(cell(t("pl.proc.gpu"), gib(live.gpu_mib)));
      cells.push(cell(t("pl.proc.cpu"), `${num(live.cpu_percent)}%`));
      cells.push(cell(t("pl.proc.ram"), gibBytes(live.rss_bytes)));
      cells.push(
        cell(
          t("pl.proc.procs"),
          num(live.pids.length),
          `${t("pl.proc.found", { how: t(`pl.found.${live.found_by}`) })}: ${live.pids.join(", ")}`,
        ),
      );
    }
    box.replaceChildren(...cells);
    box.hidden = !cells.length;
  }

  /** The log's last lines, the recipe, notes and files, each folded; built
   * again only when they change, so an open log keeps its place */
  function fillMore(box, entry) {
    const args = pl.runs.get(entry.id)?.data?.args;
    const inputs = [
      entry.log,
      entry.run_dir,
      entry.pgid,
      entry.match,
      entry.host,
      entry.host_pid,
      entry.command,
      entry.notes,
      Boolean(args),
    ];
    if (!changed(box, inputs)) return;
    const open = new Set(
      [...box.querySelectorAll("details[open]")].map((d) => d.dataset.part),
    );
    const parts = [];
    if (entry.log) {
      const log = pl.logs.get(entry.id);
      const pre = el("pre", { class: "pl-log", "data-log": entry.id });
      pre.addEventListener("scroll", () => {
        const bottom = pre.scrollHeight - pre.clientHeight - pre.scrollTop < 8;
        if (bottom) delete pre.dataset.scrolled;
        else pre.dataset.scrolled = "1";
      });
      const details = el(
        "details",
        { "data-part": "log", open: open.has("log") || Boolean(log?.open) },
        el("summary", { text: t("pl.log") }),
        pre,
      );
      details.addEventListener("toggle", () => {
        const kept = logOf(entry.id);
        kept.open = details.open;
        if (details.open) {
          if (kept.lines) fillLog(pre, kept);
          if (!kept.pending) loadLog(entry.id);
        }
      });
      if (log) fillLog(pre, log);
      parts.push(details);
    }
    if (args && typeof args === "object") {
      parts.push(
        el(
          "details",
          { "data-part": "args", open: open.has("args") },
          el("summary", { text: t("pl.recipe") }),
          el(
            "dl",
            { class: "pl-args" },
            Object.entries(args).flatMap(([key, value]) => [
              el("dt", { text: key }),
              el("dd", {
                text:
                  typeof value === "object"
                    ? JSON.stringify(value)
                    : String(value),
              }),
            ]),
          ),
        ),
      );
    }
    const files = [
      entry.run_dir ? [t("pl.files.run"), entry.run_dir] : null,
      entry.log ? [t("pl.files.log"), entry.log] : null,
      entry.pgid ? ["pgid", String(entry.pgid)] : null,
      entry.match ? [t("pl.files.match"), entry.match] : null,
      entry.host_pid && entry.host
        ? [t("pl.files.hostPid", { host: entry.host }), String(entry.host_pid)]
        : null,
      entry.command ? [t("pl.files.command"), entry.command] : null,
    ].filter(Boolean);
    if (entry.notes?.length || files.length)
      parts.push(
        el(
          "details",
          { "data-part": "notes", open: open.has("notes") },
          el("summary", {
            text: entry.notes?.length
              ? t("pl.notesCount", { n: entry.notes.length })
              : t("pl.files"),
          }),
          notesList(entry),
          el(
            "dl",
            { class: "pl-args" },
            files.flatMap(([key, value]) => [
              el("dt", { text: key }),
              el("dd", { text: value }),
            ]),
          ),
        ),
      );
    box.replaceChildren(...parts);
    box.hidden = !parts.length;
  }

  function notesList(entry) {
    if (!entry.notes?.length) return null;
    return el(
      "ul",
      { class: "pl-notes" },
      entry.notes.map((note) =>
        el(
          "li",
          {},
          el("time", { text: clock(note.at_ms, true) }),
          el("span", { text: note.text }),
        ),
      ),
    );
  }

  // Elapsed clocks and ETAs tick between polls
  setInterval(() => {
    if (!pl.shown || document.hidden) return;
    for (const span of document.querySelectorAll(".pl-elapsed[data-since]")) {
      const since = Number(span.dataset.since);
      if (since) span.textContent = stopwatch(Date.now() - since);
    }
    for (const span of document.querySelectorAll(".pl-eta[data-eta]")) {
      const left = span.querySelector(".pl-left");
      if (left)
        left.textContent = t("pl.progress.left", {
          left: duration(Number(span.dataset.eta) - Date.now()),
        });
    }
  }, 1000);

  // ------------------------------------------------------------ loss curve

  /** The loss curve of an entry's run folder: train, validation and
   * held-out losses by step, the steps still to run shaded */
  function curveBox(id) {
    const box = el("figure", { class: "pl-curve", "data-curve": id });
    box.key = el("figcaption", { class: "pl-legend-row" });
    box.plot = el("div", { class: "pl-curve-plot" });
    box.tip = el("div", { class: "chart-tip pl-tip", hidden: true });
    box.plot.append(box.tip);
    box.append(box.key, box.plot);
    box.hover = null;
    box.drawn = false;
    box.redraw = () => drawCurve(box, id);
    box.plot.addEventListener("pointermove", (event) => {
      const rect = box.plot.getBoundingClientRect();
      box.hover = event.clientX - rect.left;
      drawCurve(box, id);
    });
    box.plot.addEventListener("pointerleave", () => {
      box.hover = null;
      drawCurve(box, id);
    });
    sized.observe(box.plot);
    box.plot.redraw = box.redraw;
    return box;
  }

  /** A run's series: train loss, then validation losses by split */
  function curveSeries(data) {
    const series = [];
    const train = data.train
      .filter((row) => row[1] != null)
      .map((row) => [row[0], row[1]]);
    if (train.length)
      series.push({
        key: "train",
        label: t("pl.curve.train"),
        points: train,
        cls: "s1",
      });
    const splits = [...new Set(data.val.map((row) => row.split))].filter(
      (split) => split === "val" || split.startsWith("val_"),
    );
    splits.sort((a, b) =>
      a === "val" ? -1 : b === "val" ? 1 : a < b ? -1 : 1,
    );
    splits.slice(0, 2).forEach((split, i) => {
      const points = data.val
        .filter((row) => row.split === split && row.values.loss_total != null)
        .map((row) => [row.step, row.values.loss_total]);
      if (!points.length) return;
      series.push({
        key: split,
        label:
          split === "val"
            ? t("pl.curve.val")
            : t("pl.curve.split", { name: split.slice(4) }),
        points,
        cls: i === 0 ? "s2" : "s3",
        dots: true,
      });
    });
    return series;
  }

  function drawCurve(box, id) {
    const known = pl.runs.get(id);
    const width = Math.round(box.plot.clientWidth);
    if (!width) return;
    const data = known?.data;
    const series = data ? curveSeries(data) : [];
    if (!series.length) {
      box.key.replaceChildren();
      const text = known?.error
        ? known.error
        : !data
          ? t("pl.curve.loading")
          : t("pl.curve.none");
      const empty = box.plot.querySelector(".pl-curve-empty");
      if (empty) empty.textContent = text;
      else
        box.plot.replaceChildren(
          el("p", { class: "pl-curve-empty", text }),
          box.tip,
        );
      return;
    }
    // The legend: a line key per series, its last value
    box.key.replaceChildren(
      ...series.map((s) =>
        el(
          "span",
          { class: `key ${s.cls}` },
          el("i"),
          s.label,
          el("b", { text: num(s.points[s.points.length - 1][1], 3) }),
        ),
      ),
    );
    const height = width < 420 ? 150 : 190;
    const pad = { l: 44, r: 14, t: 10, b: 24 };
    const w = width - pad.l - pad.r;
    const h = height - pad.t - pad.b;
    const lastStep = Math.max(
      ...series.map((s) => s.points[s.points.length - 1][0]),
    );
    const total = Math.max(data.total ?? 0, lastStep, 1);
    // The first steps' high losses would flatten the rest: the scale
    // starts after the first 3% of the run
    const all = series.flatMap((s) => s.points);
    const settled = all.filter(([step]) => step >= total * 0.03);
    const values = (settled.length > 4 ? settled : all).map(([, v]) => v);
    let lo = Math.min(...values);
    let hi = Math.max(...values);
    const margin = (hi - lo || Math.abs(hi) || 1) * 0.1;
    lo -= margin;
    hi += margin;
    const x = (step) => pad.l + (step / total) * w;
    const y = (v) => pad.t + ((hi - v) / (hi - lo)) * h;
    const clipId = `pl-clip-${id.replace(/[^a-z0-9_-]/gi, "")}`;
    // Whether it trains now: then the steps to its total are still to run
    const entry = pl.state?.queue.entries.find((e) => e.id === id);
    const { state } = entry ? view(entry) : {};
    const training =
      entry &&
      (state === "running" || state === "detected") &&
      phaseOf(entry, state) === "steps";
    const ended = Boolean(entry?.progress?.ended);
    const root = svg("svg", {
      class: "pl-svg",
      width,
      height,
      viewBox: `0 0 ${width} ${height}`,
      role: "img",
      "aria-label": t("pl.curve.aria", {
        series: series
          .map((s) => `${s.label} ${num(s.points[s.points.length - 1][1], 3)}`)
          .join(", "),
      }),
    });
    root.append(
      svg(
        "defs",
        {},
        svg(
          "clipPath",
          { id: clipId },
          svg("rect", { x: pad.l, y: pad.t, width: w, height: h }),
        ),
      ),
    );
    // Steps still to run, unless the run ended before them
    if (lastStep < total && !ended)
      root.append(
        svg("rect", {
          class: "pl-future",
          x: x(lastStep),
          y: pad.t,
          width: x(total) - x(lastStep),
          height: h,
        }),
      );
    for (const v of ticks(lo, hi, Math.max(2, Math.floor(h / 36)))) {
      if (v < lo || v > hi) continue;
      root.append(
        svg("line", {
          class: "grid",
          x1: pad.l,
          x2: pad.l + w,
          y1: y(v),
          y2: y(v),
        }),
        svg(
          "text",
          { class: "tick", x: pad.l - 6, y: y(v) },
          num(v, Math.abs(hi - lo) < 0.5 ? 2 : 1),
        ),
      );
    }
    const stepTicks = ticks(0, total, Math.max(2, Math.floor(w / 90)));
    // The run's last step closes the axis when no tick is near it
    if (x(total) - x(stepTicks[stepTicks.length - 1]) > 44)
      stepTicks.push(total);
    for (const v of stepTicks) {
      root.append(
        svg("text", { class: "tick tick-x", x: x(v), y: height - 6 }, steps(v)),
      );
    }
    root.append(
      svg("line", {
        class: "grid baseline",
        x1: pad.l,
        x2: pad.l + w,
        y1: pad.t + h,
        y2: pad.t + h,
      }),
    );
    const lines = svg("g", { "clip-path": `url(#${clipId})` });
    for (const s of series) {
      const d = s.points
        .map(
          ([step, v], i) =>
            `${i ? "L" : "M"}${x(step).toFixed(1)},${y(v).toFixed(1)}`,
        )
        .join("");
      const path = svg("path", { class: `series ${s.cls}`, d });
      // The first drawing traces itself in
      if (
        !box.drawn &&
        !matchMedia("(prefers-reduced-motion: reduce)").matches
      ) {
        path.setAttribute("pathLength", "1");
        path.classList.add("pl-draw-in");
      }
      lines.append(path);
      if (s.dots && s.points.length <= 60)
        for (const [step, v] of s.points)
          lines.append(
            svg("circle", {
              class: `dot ${s.cls}`,
              cx: x(step),
              cy: y(v),
              r: 3,
            }),
          );
    }
    root.append(lines);
    // The newest point of the train loss, breathing while it trains
    const train = series[0];
    const [lastX, lastV] = train.points[train.points.length - 1];
    if (training && lastV >= lo && lastV <= hi)
      root.append(
        svg("circle", {
          class: `pl-now ${train.cls}`,
          cx: x(lastX),
          cy: y(lastV),
          r: 4,
        }),
      );
    // The crosshair: the train row nearest the pointer, and each series'
    // value at or before it
    box.tip.hidden = true;
    if (box.hover != null && box.hover >= pad.l && box.hover <= pad.l + w) {
      const step = ((box.hover - pad.l) / w) * total;
      const nearest = train.points.reduce((best, p) =>
        Math.abs(p[0] - step) < Math.abs(best[0] - step) ? p : best,
      );
      if (Math.abs(nearest[0] - step) <= total * 0.05) {
        const cx = x(nearest[0]);
        root.append(
          svg("line", {
            class: "crosshair",
            x1: cx,
            x2: cx,
            y1: pad.t,
            y2: pad.t + h,
          }),
        );
        const rows = series.map((s) => {
          const at =
            s === train
              ? nearest
              : [...s.points].reverse().find((p) => p[0] <= nearest[0]);
          if (at && at[1] >= lo && at[1] <= hi)
            root.append(
              svg("circle", {
                class: `dot ${s.cls}`,
                cx: x(at[0]),
                cy: y(at[1]),
                r: 4,
              }),
            );
          return el(
            "div",
            { class: `tip-row ${s.cls}` },
            el("i"),
            el("b", { text: at ? num(at[1], 3) : "–" }),
            ` ${s.label}${at && s !== train ? ` @ ${steps(at[0])}` : ""}`,
          );
        });
        const lr = data.train.find((row) => row[0] === nearest[0])?.[2];
        box.tip.replaceChildren(
          ...rows,
          el("div", {
            class: "tip-note",
            text: [
              `${t("pl.curve.step")} ${num(nearest[0])}`,
              lr != null ? `lr ${lr.toExponential(1)}` : null,
            ]
              .filter(Boolean)
              .join(" · "),
          }),
        );
        box.tip.hidden = false;
        const left = Math.min(cx + 12, width - box.tip.offsetWidth - 4);
        box.tip.style.left = `${Math.max(pad.l, left)}px`;
      }
    }
    const old = box.plot.querySelector("svg, .pl-curve-empty");
    if (old) old.replaceWith(root);
    else box.plot.prepend(root);
    box.drawn = true;
  }

  // ------------------------------------------------------------ scores

  /** The latest validation scores, each with its course over the run */
  function scoresBox(id) {
    const box = el("div", { class: "pl-scores", "data-scores": id });
    box.redraw = () => drawScores(box, id);
    return box;
  }

  function drawScores(box, id) {
    const data = pl.runs.get(id)?.data;
    const rows = (data?.val ?? []).filter((row) => row.split === "val");
    const has = (key) => rows.some((row) => row.values[key] != null);
    const keys = SCORES.filter(has).slice(0, 4);
    const copycat = COPYCAT.filter(([key]) => has(key));
    box.hidden = !keys.length && !copycat.length;
    if (box.hidden) return box.replaceChildren();
    const latest = rows[rows.length - 1]?.values ?? {};
    /** A score's tile: its latest value, its course and its best */
    const tile = (key, title) => {
      const points = rows
        .filter((row) => row.values[key] != null)
        .map((row) => [row.step, row.values[key]]);
      const [bestStep, best] = points.reduce((a, b) => (b[1] > a[1] ? b : a));
      const last = points[points.length - 1][1];
      return el(
        "div",
        { class: "pl-score", title: title ?? null },
        el("span", { class: "pl-score-label", text: t(`pl.score.${key}`) }),
        el("b", { class: "pl-score-value", text: num(last, 3) }),
        sparkline(points, { cls: "s2", best: [bestStep, best] }),
        el("span", {
          class: "pl-score-note",
          text: t("pl.score.best", {
            value: num(best, 3),
            step: steps(bestStep),
          }),
        }),
      );
    };
    // The copycat scores explain themselves, their companions' latest
    // values after
    const copycatTip = (key, companions) =>
      [
        t(`pl.score.tip.${key}`),
        companions
          .filter((other) => latest[other] != null)
          .map((other) => `${t(`pl.score.${other}`)} ${num(latest[other], 3)}`)
          .join(" · "),
      ]
        .filter(Boolean)
        .join("\n");
    box.replaceChildren(
      ...keys.map((key) => tile(key)),
      copycat.length
        ? el(
            "p",
            { class: "pl-scores-head", title: t("pl.copycat.note") },
            el("span", { text: t("pl.copycat") }),
            el("span", { class: "pl-dim", text: t("pl.copycat.sub") }),
          )
        : null,
      ...copycat.map(([key, companions]) =>
        tile(key, copycatTip(key, companions)),
      ),
    );
  }

  /** A small line of points [[x, y]], its last point marked; over `span`
   * ([x0, x1], else its points' own), broken where a point is missing or
   * the next is more than `gap` further */
  function sparkline(
    points,
    {
      cls = "s1",
      best = null,
      lo = null,
      hi = null,
      span = null,
      gap = Infinity,
    } = {},
  ) {
    const width = 120;
    const height = 28;
    const root = svg("svg", {
      class: "pl-spark",
      viewBox: `0 0 ${width} ${height}`,
      preserveAspectRatio: "none",
      "aria-hidden": "true",
    });
    const has = (v) => v != null && Number.isFinite(v);
    const finite = points.filter(([, v]) => has(v));
    if (finite.length < 2) return root;
    const xs = finite.map(([x]) => x);
    const vs = finite.map(([, v]) => v);
    const [x0, x1] = span ?? [Math.min(...xs), Math.max(...xs)];
    const v0 = lo ?? Math.min(...vs);
    const v1 = hi ?? Math.max(...vs);
    const sx = (v) => 2 + ((v - x0) / (x1 - x0 || 1)) * (width - 6);
    const sy = (v) => 3 + ((v1 - v) / (v1 - v0 || 1)) * (height - 6);
    const segments = [];
    let current = null;
    for (const [x, v] of points) {
      const previous = current?.[current.length - 1];
      if (!has(v) || (previous && x - previous[0] > gap)) current = null;
      if (!has(v)) continue;
      if (!current) segments.push((current = []));
      current.push([x, v]);
    }
    for (const segment of segments) {
      const d = segment
        .map(
          ([x, v], i) =>
            `${i ? "L" : "M"}${sx(x).toFixed(1)},${sy(v).toFixed(1)}`,
        )
        .join("");
      const [first] = segment[0];
      const [last] = segment[segment.length - 1];
      root.append(
        svg("path", {
          class: `pl-spark-area ${cls}`,
          d: `${d}L${sx(last).toFixed(1)},${height}L${sx(first).toFixed(1)},${height}Z`,
        }),
        svg("path", { class: `pl-spark-line ${cls}`, d }),
      );
    }
    // Dots as round-capped strokes of no length: round however the line
    // is stretched to its tile
    const dot = (px, py, name) =>
      svg("path", { class: name, d: `M${px.toFixed(1)},${py.toFixed(1)}h0` });
    if (best) root.append(dot(sx(best[0]), sy(best[1]), "pl-spark-best"));
    const [lx, lv] = finite[finite.length - 1];
    root.append(dot(sx(lx), sy(lv), `pl-spark-end ${cls}`));
    return root;
  }

  // ------------------------------------------------------------ machine

  function renderMachine(state) {
    const gpu = state.gpu;
    $("pl-gpu-name").textContent = gpu
      ? `${machineTitle(null, gpu.name)}${gpu.pstate ? ` · ${gpu.pstate}` : ""}`
      : "";
    // The last half hour's course of row index `i`, missing readings as
    // gaps
    const now = Date.now();
    const recent = pl.samples.filter(
      (row) => row[0] >= now - SPARK_MINUTES * 60000,
    );
    const course = (i, options) =>
      sparkline(
        recent.map((row) => [row[0], row[i]]),
        {
          span: [now - SPARK_MINUTES * 60000, now],
          gap: Math.max(20000, ((pl.minutes * 60000) / 720) * 3),
          ...options,
        },
      );
    const tiles = [];
    const tile = (label, value, { spark, meter, note, level, title } = {}) =>
      el(
        "div",
        { class: "pl-tile", "data-level": level ?? null, title: title ?? null },
        el("span", { class: "pl-tile-label", text: label }),
        el("span", { class: "pl-tile-value", text: value }),
        spark ?? null,
        meter ?? null,
        note ? el("span", { class: "pl-tile-note", text: note }) : null,
      );
    if (gpu) {
      tiles.push(
        tile(t("pl.tile.gpuBusy"), `${num(gpu.util)}%`, {
          spark: course(1, { cls: "s1", lo: 0, hi: 100 }),
          note: t("pl.tile.last", { minutes: SPARK_MINUTES }),
        }),
      );
      const total = gpu.mem_total_mib ?? 0;
      const used = gpu.mem_used_mib ?? 0;
      const jobs = state.queue.entries.reduce(
        (sum, e) => sum + (e.live?.gpu_mib ?? 0),
        0,
      );
      tiles.push(
        tile(t("pl.tile.gpuMem"), `${num(used / 1024, 1)} / ${gib(total)}`, {
          meter: stackedMeter(
            [
              { value: jobs, cls: "pl-m-jobs" },
              { value: Math.max(0, used - jobs), cls: "pl-m-other" },
            ],
            total,
          ),
          note: t("pl.tile.memNote", {
            jobs: gib(jobs),
            other: gib(Math.max(0, used - jobs)),
          }),
          level: total && used / total > 0.9 ? "warning" : null,
        }),
      );
      tiles.push(
        tile(t("pl.tile.temp"), `${num(gpu.temp_c)} °C`, {
          spark: course(4, { cls: "s2" }),
          note:
            gpu.fan != null ? t("pl.tile.fan", { fan: num(gpu.fan) }) : null,
          level: gpu.temp_c >= 83 ? "warning" : null,
        }),
      );
      tiles.push(
        tile(t("pl.tile.power"), `${num(gpu.power_w)} W`, {
          meter: stackedMeter(
            [{ value: gpu.power_w ?? 0, cls: "pl-m-power" }],
            gpu.power_limit_w ?? 0,
          ),
          note: t("pl.tile.powerNote", {
            limit: num(gpu.power_limit_w),
            clock: num(gpu.sm_mhz),
          }),
        }),
      );
    }
    const cpu = state.cpu;
    // The queue's share of it, in cores
    const jobCores = state.queue.entries.reduce(
      (sum, e) => sum + (e.live?.cpu_percent ?? 0) / 100,
      0,
    );
    tiles.push(
      tile(t("pl.tile.cpu"), `${num(cpu.percent)}%`, {
        spark: course(6, { cls: "s3", lo: 0, hi: 100 }),
        note: [
          t("pl.tile.cpuNote", {
            cores: cpu.cores,
            load: cpu.load ? num(cpu.load[0], 1) : "–",
          }),
          jobCores > 0
            ? t("pl.tile.cpuJobs", { cores: num(jobCores, 1) })
            : null,
        ]
          .filter(Boolean)
          .join(" · "),
      }),
    );
    const memory = state.memory;
    if (memory) {
      const usedBytes = memory.total - memory.available;
      const swapUsed = memory.swap_total - memory.swap_free;
      const swapFull =
        memory.swap_total > 0 && memory.swap_free < memory.swap_total * 0.05;
      tiles.push(
        tile(
          t("pl.tile.ram"),
          `${num(usedBytes / 2 ** 30, 1)} / ${gibBytes(memory.total)}`,
          {
            meter: stackedMeter(
              [{ value: usedBytes, cls: "pl-m-ram" }],
              memory.total,
            ),
            note: [
              t("pl.tile.ramNote", { available: gibBytes(memory.available) }),
              memory.swap_total
                ? t(swapFull ? "pl.tile.swapFull" : "pl.tile.swap", {
                    used: gibBytes(swapUsed),
                  })
                : null,
            ]
              .filter(Boolean)
              .join(" · "),
            level:
              memory.available < memory.total * 0.1
                ? "critical"
                : swapFull
                  ? "warning"
                  : null,
          },
        ),
      );
    }
    // The VM's GPU as its runner last read it: its numbers while that is
    // fresh, else when it last did and whether the runner still runs
    const remote = state.remote;
    if (remote) {
      const reading = remote.fresh ? remote.gpu : null;
      // An entry the queue says runs there: then no reading is a warning
      const expected = state.queue.entries.some(
        (e) => e.remote?.host === remote.host && e.status === "running",
      );
      const since = remote.at_ms
        ? t("pl.tile.remoteSince", { ago: ago(remote.at_ms) })
        : null;
      const down = remote.runner ? null : t("pl.tile.runnerStopped");
      tiles.push(
        tile(
          machineTitle(remote.host, remoteGpu()),
          reading?.util != null
            ? `${num(reading.util)}%`
            : remote.fresh
              ? t("pl.tile.unreachable")
              : t("pl.tile.noReading"),
          {
            spark: course(8, { cls: "s1", lo: 0, hi: 100 }),
            meter: reading
              ? stackedMeter(
                  [{ value: reading.mem_used_mib ?? 0, cls: "pl-m-other" }],
                  reading.mem_total_mib ?? 0,
                )
              : null,
            note: reading
              ? [
                  `${num((reading.mem_used_mib ?? 0) / 1024, 1)} / ${gib(reading.mem_total_mib ?? 0)}`,
                  reading.temp_c != null ? `${num(reading.temp_c)} °C` : null,
                  reading.power_w != null ? `${num(reading.power_w)} W` : null,
                  remote.hold ? t("pl.tile.hold") : null,
                ]
                  .filter(Boolean)
                  .join(" · ")
              : [remote.fresh ? remote.error : since, down]
                  .filter(Boolean)
                  .join(" · "),
            level:
              (!reading && expected) || reading?.temp_c >= 83
                ? "warning"
                : null,
            title: remote.error ?? null,
          },
        ),
      );
    }
    $("pl-tiles").replaceChildren(...tiles);
    renderProcesses(state);
  }

  /** A meter of parts stacked on a track of `total` */
  function stackedMeter(parts, total) {
    const track = el("div", { class: "pl-meter", "aria-hidden": "true" });
    for (const part of parts) {
      const share = total > 0 ? Math.min(1, part.value / total) : 0;
      if (share > 0)
        track.append(
          el("i", {
            class: part.cls,
            style: `width: ${(share * 100).toFixed(2)}%`,
          }),
        );
    }
    return track;
  }

  /** Each GPU's compute processes: memory, name, entry */
  function renderProcesses(state) {
    const box = $("pl-procs");
    const gpu = state.gpu;
    const remote = state.remote;
    // A process's row: its share of the GPU's memory (unknown on Windows)
    const row = (mib, total, name, detail, entry, title) =>
      el(
        "li",
        {
          class: "pl-proc",
          "data-entry": entry ? "1" : null,
          title: title ?? null,
        },
        el(
          "span",
          { class: "pl-proc-bar", "aria-hidden": "true" },
          mib == null
            ? null
            : el("i", {
                style: `width: ${Math.max(1.5, (mib / (total || 1)) * 100).toFixed(2)}%`,
              }),
        ),
        el("b", {
          class: "pl-proc-mem num",
          text: mib == null ? "–" : gib(mib),
        }),
        el("span", { class: "pl-proc-name", text: name }),
        entry ? el("code", { class: "pl-proc-entry", text: entry }) : null,
        detail
          ? el("span", { class: "pl-proc-detail num", text: detail })
          : null,
      );
    const parts = [];
    if (!gpu) {
      parts.push(
        el("p", {
          class: "panel-note",
          text: t("pl.noGpu", { error: state.gpu_error ?? "" }),
        }),
      );
    } else {
      const total = gpu.mem_total_mib || 1;
      const procs = state.gpu_procs ?? [];
      const counted = procs.reduce((sum, p) => sum + (p.gpu_mib ?? 0), 0);
      const rest = Math.max(0, (gpu.mem_used_mib ?? 0) - counted);
      const rows = procs.map((p) =>
        row(p.gpu_mib ?? 0, total, p.name, `pid ${p.pid}`, p.entry, p.command),
      );
      if (rest > 0)
        rows.push(
          row(
            rest,
            total,
            t("pl.procs.other"),
            null,
            null,
            t("pl.procs.otherNote"),
          ),
        );
      parts.push(
        el("h3", {
          class: "pl-sub",
          text: remote ? machineTitle(null, gpu.name) : t("pl.procs.title"),
        }),
        rows.length
          ? el("ul", { class: "pl-proc-list" }, rows)
          : el("p", { class: "panel-note", text: t("pl.procs.none") }),
      );
    }
    // The VM's: the Python processes its runner lists, the runner's job
    // on them
    if (remote) {
      const job = remote.fresh ? remote.job : null;
      const rows = remote.fresh
        ? remote.processes.map((p) =>
            row(
              p.memory_mib,
              remote.gpu?.mem_total_mib,
              p.name,
              p.pid != null ? `pid ${p.pid}` : null,
              /^python/i.test(p.name) ? job : null,
              t("pl.procs.remoteNote", { host: remote.host }),
            ),
          )
        : [];
      parts.push(
        el("h3", {
          class: "pl-sub",
          text: machineTitle(remote.host, remoteGpu()),
        }),
        rows.length
          ? el("ul", { class: "pl-proc-list" }, rows)
          : el("p", {
              class: "panel-note",
              text: remote.fresh
                ? t("pl.procs.none")
                : t("pl.procs.noReading", {
                    host: remote.host,
                    ago: ago(remote.at_ms),
                  }),
            }),
      );
    }
    box.replaceChildren(...parts);
  }

  // ------------------------------------------------------------ timeline

  const timelineBox = $("pl-timeline");
  timelineBox.hover = null;
  timelineBox.redraw = () => renderTimeline();
  timelineBox.addEventListener("pointermove", (event) => {
    timelineBox.hover =
      event.clientX - timelineBox.getBoundingClientRect().left;
    renderTimeline();
  });
  timelineBox.addEventListener("pointerleave", () => {
    timelineBox.hover = null;
    renderTimeline();
  });
  sized.observe(timelineBox);

  /** The entries' bars over [from, to] of the entries `keep` takes: the
   * spans the lab saw them running, else the queue's own times, packed
   * into lanes */
  function laneBars(from, to, keep) {
    const entries = (pl.state?.queue.entries ?? []).filter(keep);
    const bars = [];
    for (const entry of entries) {
      const seen = pl.spans[entry.id];
      const { state, place } = view(entry);
      let spans = seen ? seen.map(([a, b]) => [a, b]) : [];
      const started = entry.started_ms;
      if (spans.length && started != null && started < spans[0][0]) {
        // Begun before the lab saw it
        spans[0][0] = started;
      }
      if (!spans.length && started != null) {
        const end =
          entry.ended_ms ?? (place === "running" ? Date.now() : endedAt(entry));
        if (end != null) spans = [[started, Math.max(end, started)]];
      }
      for (const [a, b] of spans) {
        if (b < from || a > to) continue;
        bars.push({ entry, state, a: Math.max(a, from), b: Math.min(b, to) });
      }
    }
    bars.sort((p, q) => p.a - q.a);
    const lanes = [];
    const gap = (to - from) / 200;
    for (const bar of bars) {
      let lane = lanes.findIndex((end) => end + gap <= bar.a);
      if (lane < 0) {
        lane = Math.min(lanes.length, MAX_LANES - 1);
        if (lane === lanes.length) lanes.push(0);
      }
      lanes[lane] = Math.max(lanes[lane], bar.b);
      bar.lane = lane;
    }
    return { bars, lanes: lanes.length };
  }

  /** The timeline's sections, top to bottom: this host's GPU, its CPU
   * over its busy row, and the entries that ran here (GPU and CPU alike);
   * then the remote GPU and its entries, while its runner has a file or
   * the window holds its readings or its entries. Each names its row
   * indexes: busy, memory, the jobs' memory (this host's only), °C, W */
  function timelineSections(rows, from, to) {
    const state = pl.state;
    const remote = state.remote;
    const host =
      remote?.host ?? state.queue.entries.find((e) => e.remote)?.remote.host;
    const there = (e) => Boolean(host) && e.remote?.host === host;
    const sections = [
      {
        key: "local",
        title: machineTitle(null, state.gpu?.name),
        busy: 1,
        mem: 2,
        jobs: 3,
        temp: 4,
        power: 5,
        total: state.gpu?.mem_total_mib,
        cpu: true,
        ...laneBars(from, to, (e) => !there(e)),
      },
    ];
    if (host) {
      const far = {
        key: "remote",
        title: machineTitle(host, remoteGpu()),
        busy: 8,
        mem: 9,
        jobs: null,
        temp: 10,
        power: 11,
        total: remote?.gpu?.mem_total_mib,
        cpu: false,
        ...laneBars(from, to, there),
      };
      if (remote || far.bars.length || rows.some((row) => row[8] != null))
        sections.push(far);
    }
    return sections;
  }

  function renderTimeline() {
    const box = timelineBox;
    const width = Math.round(box.clientWidth);
    if (!width || !pl.state) return;
    const to = Date.now();
    const from = to - pl.minutes * 60000;
    const rows = pl.samples.filter((row) => row[0] >= from);
    const sections = timelineSections(rows, from, to);
    const narrow = width < 560;
    // Room on the right for each row's latest value
    const pad = { l: narrow ? 40 : 52, r: 46, t: 2 };
    const titleH = 18;
    const busyH = narrow ? 50 : 62;
    const memH = narrow ? 40 : 54;
    const laneH = 18;
    const gapH = 10;
    const sectionGap = 18;
    const axisH = 22;
    let top = pad.t;
    for (const s of sections) {
      s.titleY = top + 9;
      s.busyTop = top + titleH;
      s.memTop = s.busyTop + busyH + gapH;
      s.laneTop = s.memTop + memH + gapH;
      s.bottom = s.laneTop + Math.max(1, s.lanes) * (laneH + 4);
      top = s.bottom + sectionGap;
    }
    const lastSection = sections[sections.length - 1];
    const height = lastSection.bottom + axisH;
    const w = width - pad.l - pad.r;
    const x = (t) => pad.l + ((t - from) / (to - from)) * w;
    const gpu = pl.state.gpu;
    const root = svg("svg", {
      class: "pl-svg pl-timeline-svg",
      width,
      height,
      viewBox: `0 0 ${width} ${height}`,
      role: "img",
      "aria-label": t("pl.timeline.aria", {
        hours: num(pl.minutes / 60),
        util: gpu?.util != null ? num(gpu.util) : "–",
      }),
    });
    const defs = svg("defs");
    root.append(defs);
    // The time grid: every few minutes, its labels under the last lanes
    const spanMin = (to - from) / 60000;
    const stepMin =
      [5, 10, 15, 30, 60, 120, 180].find((m) => (spanMin / m) * 70 <= w) ?? 240;
    const firstTick = Math.ceil(from / (stepMin * 60000)) * stepMin * 60000;
    const gridTimes = [];
    for (let tick = firstTick; tick <= to; tick += stepMin * 60000)
      gridTimes.push(tick);
    for (const tick of gridTimes)
      root.append(
        svg(
          "text",
          { class: "tick tick-x", x: x(tick), y: lastSection.bottom + 14 },
          tickLabel(tick),
        ),
      );
    // A series as a path, broken where samples are missing or far apart
    const gapMs = Math.max(20000, ((to - from) / 720) * 3);
    const pathOf = (value, yOf, base = null) => {
      let d = "";
      let open = false;
      let startX = 0;
      let prevT = null;
      let prevX = 0;
      const close = () => {
        if (open && base != null)
          d += `L${prevX.toFixed(1)},${base}L${startX.toFixed(1)},${base}Z`;
        open = false;
      };
      for (const row of rows) {
        const v = value(row);
        if (v == null || (prevT != null && row[0] - prevT > gapMs)) close();
        if (v == null) {
          prevT = row[0];
          continue;
        }
        const px = x(row[0]);
        d += `${open ? "L" : "M"}${px.toFixed(1)},${yOf(v).toFixed(1)}`;
        if (!open) startX = px;
        open = true;
        prevT = row[0];
        prevX = px;
      }
      close();
      return d;
    };
    // The stretches without a reading of row index `i`: before the first,
    // between two far apart, after the last
    const gapsOf = (i) => {
      const gaps = [];
      let prev = null;
      for (const row of rows) {
        if (row[i] == null) continue;
        if (row[0] - (prev ?? from) > gapMs) gaps.push([prev ?? from, row[0]]);
        prev = row[0];
      }
      if (prev == null) gaps.push([from, to]);
      else if (to - prev > gapMs) gaps.push([prev, to]);
      return gaps;
    };
    // Each entry's CPU in cores, from the rows: the strip inside its bar,
    // on one scale for all (at least a core)
    const cores = new Map();
    for (const row of rows)
      for (const [id, value] of Object.entries(row[13] ?? {}))
        if (value != null) {
          if (!cores.has(id)) cores.set(id, []);
          cores.get(id).push([row[0], value]);
        }
    const coreScale = Math.max(
      1,
      ...[...cores.values()].flat().map(([, value]) => value),
    );
    let stripped = false;
    const last = rows[rows.length - 1];
    const fresh = last && to - last[0] < 60000;
    const rowLabel = (text, y) =>
      svg("text", { class: "tick pl-row-label", x: pad.l - 6, y }, text);
    const endLabel = (y, text) =>
      svg("text", { class: "pl-end", x: pad.l + w + 8, y }, text);
    sections.forEach((s, si) => {
      const busyBase = s.busyTop + busyH;
      s.yBusy = (v) => busyBase - (Math.min(v, 100) / 100) * busyH;
      s.memTotal =
        s.total ?? Math.max(1, ...rows.map((row) => row[s.mem] ?? 0));
      s.yMem = (v) =>
        s.memTop + memH - (Math.min(v, s.memTotal) / s.memTotal) * memH;
      const { yBusy, yMem } = s;
      root.append(
        svg(
          "text",
          { class: "pl-section-title", x: pad.l, y: s.titleY },
          s.title,
        ),
      );
      // No reading: shaded, never drawn as 0
      for (const [a, b] of gapsOf(s.busy))
        root.append(
          svg("rect", {
            class: "pl-nodata",
            x: x(a),
            y: s.busyTop,
            width: Math.max(0, x(b) - x(a)),
            height: s.memTop + memH - s.busyTop,
          }),
        );
      // Grids and row labels
      root.append(
        svg("line", {
          class: "grid",
          x1: pad.l,
          x2: pad.l + w,
          y1: yBusy(50),
          y2: yBusy(50),
        }),
        svg("line", {
          class: "grid baseline",
          x1: pad.l,
          x2: pad.l + w,
          y1: yBusy(0),
          y2: yBusy(0),
        }),
        rowLabel("100%", yBusy(100) + 4),
        rowLabel("50%", yBusy(50)),
        svg("line", {
          class: "grid pl-cap",
          x1: pad.l,
          x2: pad.l + w,
          y1: yMem(s.memTotal),
          y2: yMem(s.memTotal),
        }),
        svg("line", {
          class: "grid baseline",
          x1: pad.l,
          x2: pad.l + w,
          y1: yMem(0),
          y2: yMem(0),
        }),
        rowLabel(gib(s.memTotal).replace(" GiB", "G"), yMem(s.memTotal) + 4),
        rowLabel("0", yMem(0) - 4),
      );
      for (const tick of gridTimes)
        root.append(
          svg("line", {
            class: "grid pl-vgrid",
            x1: x(tick),
            x2: x(tick),
            y1: s.busyTop,
            y2: s.memTop + memH,
          }),
        );
      // Busy, and this host's CPU over it (the same 0-100% scale)
      root.append(
        svg("path", {
          class: "pl-area s1",
          d: pathOf((row) => row[s.busy], yBusy, busyBase),
        }),
        svg("path", {
          class: "series s1 pl-thin",
          d: pathOf((row) => row[s.busy], yBusy),
        }),
      );
      if (s.cpu)
        root.append(
          svg("path", {
            class: "series s3 pl-thin",
            d: pathOf((row) => row[6], yBusy),
          }),
        );
      // Memory: all of it, then the queue's jobs over it where known
      root.append(
        svg("path", {
          class: "pl-mem-other",
          d: pathOf((row) => row[s.mem], yMem, yMem(0)),
        }),
      );
      if (s.jobs != null)
        root.append(
          svg("path", {
            class: "pl-mem-jobs",
            d: pathOf(
              (row) =>
                row[s.jobs] == null
                  ? null
                  : Math.min(row[s.jobs], row[s.mem] ?? row[s.jobs]),
              yMem,
              yMem(0),
            ),
          }),
        );
      root.append(
        svg("path", {
          class: "pl-mem-line",
          d: pathOf((row) => row[s.mem], yMem),
        }),
      );
      // The newest sample, breathing, and each row's value at its end
      if (fresh && last[s.busy] != null) {
        root.append(
          svg("circle", {
            class: "pl-now s1",
            cx: x(last[0]),
            cy: yBusy(last[s.busy]),
            r: 4,
          }),
          endLabel(yBusy(last[s.busy]), `${num(last[s.busy])}%`),
        );
        // The CPU's too, where it does not sit on the busy one
        if (
          s.cpu &&
          last[6] != null &&
          Math.abs(yBusy(last[6]) - yBusy(last[s.busy])) >= 12
        )
          root.append(endLabel(yBusy(last[6]), `${num(last[6])}%`));
      }
      if (fresh && last[s.mem] != null)
        root.append(
          endLabel(yMem(last[s.mem]), gib(last[s.mem]).replace(" GiB", "G")),
        );
      // Lanes: what ran when, each bar with its job's CPU inside
      s.bars.forEach((bar, bi) => {
        const bx = x(bar.a);
        const bw = Math.max(3, x(bar.b) - bx);
        const by = s.laneTop + bar.lane * (laneH + 4);
        const group = svg("g", { class: "pl-lane", "data-state": bar.state });
        group.append(
          svg("rect", { x: bx, y: by, width: bw, height: laneH, rx: 4 }),
        );
        const points = (cores.get(bar.entry.id) ?? []).filter(
          ([t]) => t >= bar.a && t <= bar.b,
        );
        if (points.length) {
          const clip = `pl-lane-clip-${si}-${bi}`;
          defs.append(
            svg(
              "clipPath",
              { id: clip },
              svg("rect", { x: bx, y: by, width: bw, height: laneH, rx: 4 }),
            ),
          );
          const yOf = (value) =>
            by + laneH - (Math.min(value, coreScale) / coreScale) * laneH;
          const first = x(points[0][0]);
          let d = `M${first.toFixed(1)},${by + laneH}`;
          for (const [t, value] of points)
            d += `L${x(t).toFixed(1)},${yOf(value).toFixed(1)}`;
          d += `L${x(points[points.length - 1][0]).toFixed(1)},${by + laneH}Z`;
          group.append(
            svg("path", {
              class: "pl-lane-cpu",
              d,
              "clip-path": `url(#${clip})`,
            }),
          );
          stripped = true;
        }
        const label = bar.entry.id;
        if (bw > label.length * 6.4 + 12)
          group.append(
            svg(
              "text",
              { class: "pl-lane-label", x: bx + 6, y: by + laneH / 2 },
              label,
            ),
          );
        group.append(svg("title", {}, `${bar.entry.id}: ${bar.entry.title}`));
        root.append(group);
      });
      if (!s.bars.length)
        root.append(
          svg(
            "text",
            {
              class: "tick pl-lanes-empty",
              x: pad.l + 4,
              y: s.laneTop + laneH / 2,
            },
            t("pl.timeline.noLanes"),
          ),
        );
    });
    // Crosshair: the sample nearest the pointer, and what ran then, on
    // each GPU
    const tip =
      box.querySelector(".pl-tip") ?? el("div", { class: "chart-tip pl-tip" });
    tip.hidden = true;
    if (
      box.hover != null &&
      box.hover >= pad.l &&
      box.hover <= pad.l + w &&
      rows.length
    ) {
      const at = from + ((box.hover - pad.l) / w) * (to - from);
      const row = rows.reduce((best, r) =>
        Math.abs(r[0] - at) < Math.abs(best[0] - at) ? r : best,
      );
      if (Math.abs(row[0] - at) < gapMs) {
        const cx = x(row[0]);
        root.append(
          svg("line", {
            class: "crosshair",
            x1: cx,
            x2: cx,
            y1: sections[0].busyTop,
            y2: lastSection.bottom,
          }),
        );
        const line = (cls, value, label) =>
          el(
            "div",
            { class: `tip-row ${cls}` },
            el("i"),
            el("b", { text: value }),
            ` ${label}`,
          );
        const lines = [];
        for (const s of sections) {
          if (row[s.busy] != null)
            root.append(
              svg("circle", {
                class: "dot s1",
                cx,
                cy: s.yBusy(row[s.busy]),
                r: 4,
              }),
            );
          lines.push(el("div", { class: "tip-head", text: s.title }));
          if (row[s.busy] == null) {
            lines.push(
              el("div", {
                class: "tip-note",
                text: t("pl.timeline.noReading"),
              }),
            );
          } else {
            lines.push(
              line("s1", `${num(row[s.busy])}%`, t("pl.timeline.busy")),
            );
            if (s.cpu && row[6] != null)
              lines.push(
                line(
                  "s3",
                  `${num(row[6])}%`,
                  row[12] != null
                    ? t("pl.timeline.cpuLoad", { load: num(row[12], 1) })
                    : t("pl.timeline.cpu"),
                ),
              );
            if (s.jobs != null)
              lines.push(
                line(
                  "pl-k-jobs",
                  row[s.jobs] == null ? "–" : gib(row[s.jobs]),
                  t("pl.timeline.jobs"),
                ),
                line(
                  "pl-k-other",
                  row[s.mem] == null
                    ? "–"
                    : gib(Math.max(0, row[s.mem] - (row[s.jobs] ?? 0))),
                  t("pl.timeline.other"),
                ),
              );
            else
              lines.push(
                line(
                  "pl-k-other",
                  row[s.mem] == null ? "–" : gib(row[s.mem]),
                  t("pl.timeline.used"),
                ),
              );
            const heat = [
              row[s.temp] != null ? `${num(row[s.temp])} °C` : null,
              row[s.power] != null ? `${num(row[s.power])} W` : null,
            ].filter(Boolean);
            if (heat.length)
              lines.push(
                el("div", { class: "tip-note", text: heat.join(" · ") }),
              );
          }
          for (const bar of s.bars) {
            if (bar.a > row[0] || bar.b < row[0]) continue;
            const used = row[13]?.[bar.entry.id];
            lines.push(
              el("div", {
                class: "tip-note pl-tip-entry",
                text:
                  used != null
                    ? `▸ ${bar.entry.id} · ${t("pl.timeline.cores", { cores: num(used, 1) })}`
                    : `▸ ${bar.entry.id}`,
              }),
            );
          }
        }
        tip.replaceChildren(
          el("div", { class: "tip-note", text: clock(row[0], true) }),
          ...lines,
        );
        tip.hidden = false;
        box.append(tip);
        const left =
          cx + 14 + tip.offsetWidth > width
            ? cx - tip.offsetWidth - 14
            : cx + 14;
        tip.style.left = `${Math.max(0, left)}px`;
      }
    }
    const old = box.querySelector("svg");
    if (old) old.replaceWith(root);
    else box.prepend(root);
    if (!tip.isConnected) box.append(tip);
    // The legend and the note under it
    $("pl-legend").replaceChildren(
      el("span", { class: "key s1" }, el("i"), t("pl.timeline.busy")),
      el("span", { class: "key s3" }, el("i"), t("pl.timeline.cpu")),
      el("span", { class: "key pl-k-jobs" }, el("i"), t("pl.timeline.jobs")),
      el("span", { class: "key pl-k-other" }, el("i"), t("pl.timeline.other")),
      el("span", { class: "key pl-k-cpu" }, el("i"), t("pl.timeline.jobCpu")),
      el(
        "span",
        { class: "key pl-k-none" },
        el("i"),
        t("pl.timeline.noReading"),
      ),
    );
    const since = pl.samplingSince;
    $("pl-timeline-note").textContent = [
      since != null
        ? t("pl.timeline.since", {
            s: num((pl.state.sample_ms ?? 5000) / 1000),
            clock: clock(since, true),
          })
        : t("pl.timeline.empty"),
      stripped ? t("pl.timeline.strip", { cores: num(coreScale, 1) }) : null,
    ]
      .filter(Boolean)
      .join(" ");
  }

  for (const button of $("pl-range").querySelectorAll("[data-minutes]")) {
    button.addEventListener("click", async () => {
      pl.minutes = Number(button.dataset.minutes);
      remember("range", pl.minutes);
      markRange();
      try {
        await loadTimeline();
        renderTimeline();
      } catch (error) {
        pl.error = error.message;
        renderError();
      }
    });
  }

  function markRange() {
    for (const button of $("pl-range").querySelectorAll("[data-minutes]"))
      button.setAttribute(
        "aria-pressed",
        String(Number(button.dataset.minutes) === pl.minutes),
      );
  }
  markRange();

  // ------------------------------------------------------------ queue

  /** The waiting entries in the order they run (or as being saved) */
  function waiting(entries) {
    const list = entries.filter((e) => view(e).place === "waiting");
    list.sort((a, b) => (a.rank ?? 1e9) - (b.rank ?? 1e9));
    if (pl.order) {
      const at = new Map(pl.order.map((id, i) => [id, i]));
      list.sort((a, b) => (at.get(a.id) ?? 1e9) - (at.get(b.id) ?? 1e9));
    }
    return list;
  }

  function renderQueue(entries, slots) {
    const state = pl.state;
    const list = waiting(entries);
    const problems = state.queue.problems ?? [];
    const error = $("pl-queue-error");
    if (state.queue.error && !entries.length) {
      error.hidden = false;
      error.textContent = t("pl.queue.missing", { path: state.queue.path });
    } else if (state.queue.error || problems.length) {
      error.hidden = false;
      error.textContent = t("pl.queue.problem", {
        problems: [state.queue.error, ...problems].filter(Boolean).join("; "),
      });
    } else error.hidden = true;
    $("pl-queue-note").textContent =
      pl.orderNote ||
      [
        t("pl.queue.count", { n: list.length }),
        state.queue.updated_ms
          ? t("pl.queue.updated", { ago: ago(state.queue.updated_ms) })
          : null,
      ]
        .filter(Boolean)
        .join(" · ");
    $("pl-queue-note").title = state.queue.path;
    $("pl-queue-hint").textContent = list.length > 1 ? t("pl.queue.hint") : "";
    const box = $("pl-queue");
    // Drawn again only when it changed, so a focused handle stays focused
    const twoGpus = Boolean(state.remote);
    const content = list.map((e) => [
      e.id,
      e.status,
      e.title,
      e.why,
      e.group,
      e.owner,
      e.device,
      e.eta_ms,
      e.next_on,
      e.waits,
      twoGpus,
      slots.get(e.group),
    ]);
    if (!changed(box, content)) return;
    if (!list.length) {
      box.replaceChildren(
        el("li", { class: "pl-empty", text: t("pl.queue.empty") }),
      );
      return;
    }
    /** Which runners take it next: "Next", or "Next · Linux, win11" when
     * there is more than one to take it */
    const nextBadge = (entry) => {
      const on = entry.next_on ?? [];
      if (!on.length) return null;
      const names = on.map((device) =>
        device === "cpu"
          ? t("pl.device.cpu")
          : device.startsWith("gpu:")
            ? device.slice(4) === "linux"
              ? "Linux"
              : device.slice(4)
            : device,
      );
      const plain = !twoGpus && on.length === 1 && on[0] !== "cpu";
      return el("span", {
        class: "pl-next",
        text: plain
          ? t("pl.queue.next")
          : t("pl.queue.nextOn", { on: names.join(", ") }),
        title: t("pl.queue.nextNote"),
      });
    };
    let previous = null;
    const items = list.map((entry, i) => {
      const sameGroup =
        previous && entry.group && previous.group === entry.group;
      const sameWhy = sameGroup && previous.why === entry.why;
      const nextInGroup =
        list[i + 1]?.group && list[i + 1].group === entry.group;
      previous = entry;
      const grip = el(
        "button",
        {
          type: "button",
          class: "pl-grip",
          "aria-label": t("pl.queue.move", {
            id: entry.id,
            n: i + 1,
            count: list.length,
          }),
          title: t("pl.queue.drag"),
        },
        el("span", { "aria-hidden": "true" }),
      );
      grip.addEventListener("pointerdown", (event) =>
        startDrag(event, entry.id),
      );
      grip.addEventListener("keydown", (event) => keyMove(event, entry.id));
      return el(
        "li",
        {
          class: "pl-q",
          "data-id": entry.id,
          "data-status": entry.status,
          "data-joined-up": sameGroup ? "1" : null,
          "data-joined-down": nextInGroup ? "1" : null,
          style: entry.group
            ? `--g: var(--pl-c${slots.get(entry.group) ?? 1})`
            : null,
        },
        grip,
        el("span", { class: "pl-rank num", text: String(i + 1) }),
        el(
          "div",
          { class: "pl-q-body" },
          el(
            "div",
            { class: "pl-q-head" },
            groupChip(entry, slots),
            el("b", { class: "pl-q-title", text: entry.title || entry.id }),
          ),
          el(
            "div",
            { class: "pl-q-meta" },
            el("code", { class: "pl-id", text: entry.id }),
            entry.device
              ? el("span", { text: deviceWord(entry.device) })
              : null,
            entry.owner ? el("span", { text: entry.owner }) : null,
            entry.eta_ms
              ? el("span", {
                  text: t("pl.progress.eta", { clock: clock(entry.eta_ms) }),
                })
              : null,
            entry.waits?.length
              ? el("span", {
                  class: "pl-waits",
                  text: t("pl.queue.after", { ids: entry.waits.join(", ") }),
                  title: t("pl.queue.afterNote"),
                })
              : null,
          ),
          entry.why && !sameWhy
            ? el("p", { class: "pl-why", text: entry.why })
            : null,
        ),
        entry.status === "paused" ? stateBadge("paused") : nextBadge(entry),
      );
    });
    box.replaceChildren(...items);
  }

  /** Put `id` at `index` among the waiting entries and save the order */
  function moveTo(id, index) {
    const ids = waiting(pl.state.queue.entries).map((e) => e.id);
    const from = ids.indexOf(id);
    if (from < 0 || index === from) return;
    ids.splice(from, 1);
    ids.splice(Math.max(0, Math.min(ids.length, index)), 0, id);
    pl.order = ids;
    const slots = groupSlots(pl.state.queue.entries);
    pl.orderNote = t("pl.queue.saving");
    renderQueue(pl.state.queue.entries, slots);
    // Keyboard moves come in bursts: save once they stop
    clearTimeout(pl.orderTimer);
    pl.orderTimer = setTimeout(saveOrder, 350);
  }

  /** Save the order the page shows; one save at a time, the newest order
   * after the one under way */
  async function saveOrder() {
    const order = pl.order;
    if (!order || pl.saving) return;
    pl.saving = true;
    try {
      const state = await api("order", { order });
      pl.state = state;
      pl.orderNote = t("pl.queue.saved");
      setTimeout(() => {
        pl.orderNote = "";
        if (pl.state && !pl.drag && !pl.order)
          renderQueue(
            pl.state.queue.entries,
            groupSlots(pl.state.queue.entries),
          );
      }, 2500);
    } catch (error) {
      pl.orderNote = t("pl.queue.failed", { error: error.message });
    }
    pl.saving = false;
    // A move made meanwhile is saved next
    if (pl.order === order) pl.order = null;
    if (pl.order) saveOrder();
    else if (pl.state) render();
  }

  function keyMove(event, id) {
    const ids = waiting(pl.state.queue.entries).map((e) => e.id);
    const at = ids.indexOf(id);
    const to =
      event.key === "ArrowUp"
        ? at - 1
        : event.key === "ArrowDown"
          ? at + 1
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? ids.length - 1
              : null;
    if (to == null) return;
    event.preventDefault();
    if (to < 0 || to >= ids.length) return;
    moveTo(id, to);
    $("pl-queue")
      .querySelector(`.pl-q[data-id="${CSS.escape(id)}"] .pl-grip`)
      ?.focus();
  }

  /** Drag an entry by its handle: it follows the pointer, the others make
   * room, and where it is let go is its new place (mouse, pen or touch) */
  function startDrag(event, id) {
    if (event.button !== 0) return;
    const list = $("pl-queue");
    const items = [...list.querySelectorAll(".pl-q")];
    const item = items.find((node) => node.dataset.id === id);
    if (!item) return;
    event.preventDefault();
    const rects = items.map((node) => node.getBoundingClientRect());
    const index = items.indexOf(item);
    const gap =
      rects.length > 1 ? Math.max(0, rects[1].top - rects[0].bottom) : 0;
    pl.drag = {
      id,
      item,
      items,
      rects,
      index,
      target: index,
      gap,
      startY: event.clientY,
      pointer: event.pointerId,
      scroll: window.scrollY,
    };
    item.classList.add("is-dragged");
    list.classList.add("is-sorting");
    event.currentTarget.setPointerCapture(event.pointerId);
    event.currentTarget.addEventListener("pointermove", dragMove);
    event.currentTarget.addEventListener("pointerup", dragEnd);
    event.currentTarget.addEventListener("pointercancel", dragEnd);
  }

  function dragMove(event) {
    const d = pl.drag;
    if (!d || event.pointerId !== d.pointer) return;
    // The page may scroll under a long drag
    const dy = event.clientY - d.startY + (window.scrollY - d.scroll);
    d.item.style.transform = `translateY(${dy}px)`;
    const own = d.rects[d.index];
    const centre = own.top + own.height / 2 + dy;
    let target = 0;
    d.rects.forEach((rect, i) => {
      if (i !== d.index && centre > rect.top + rect.height / 2) target += 1;
    });
    const shift = own.height + d.gap;
    d.items.forEach((node, i) => {
      if (i === d.index) return;
      const moved =
        d.index < target && i > d.index && i <= target
          ? -shift
          : d.index > target && i >= target && i < d.index
            ? shift
            : 0;
      node.style.transform = moved ? `translateY(${moved}px)` : "";
    });
    d.target = target;
    // Near the window's edges, scroll along
    const edge = 48;
    if (event.clientY < edge) window.scrollBy(0, -12);
    else if (event.clientY > innerHeight - edge) window.scrollBy(0, 12);
  }

  function dragEnd(event) {
    const d = pl.drag;
    if (!d || event.pointerId !== d.pointer) return;
    event.currentTarget.removeEventListener("pointermove", dragMove);
    event.currentTarget.removeEventListener("pointerup", dragEnd);
    event.currentTarget.removeEventListener("pointercancel", dragEnd);
    pl.drag = null;
    for (const node of d.items) node.style.transform = "";
    d.item.classList.remove("is-dragged");
    $("pl-queue").classList.remove("is-sorting");
    if (event.type === "pointerup" && d.target !== d.index)
      moveTo(d.id, d.target);
    else if (pl.state)
      renderQueue(pl.state.queue.entries, groupSlots(pl.state.queue.entries));
  }

  // ------------------------------------------------------------ results

  function finished(entries) {
    const list = entries.filter((e) => view(e).place === "finished");
    list.sort((a, b) => (endedAt(b) ?? 0) - (endedAt(a) ?? 0));
    return list;
  }

  function renderResults(entries, slots) {
    const list = finished(entries).slice(0, RESULTS);
    $("pl-results-note").textContent = list.length
      ? t("pl.results.count", { n: finished(entries).length })
      : "";
    const box = $("pl-results");
    // Drawn again only when it changed; an opened result keeps its chart
    const content = list.map((e) => [
      e.id,
      view(e).state,
      e.title,
      e.result_summary,
      e.next_step,
      endedAt(e),
      e.progress?.step,
      slots.get(e.group),
      pl.openResults.has(e.id),
    ]);
    if (!changed(box, content)) return;
    if (!list.length) {
      box.replaceChildren(
        el("li", { class: "pl-empty", text: t("pl.results.empty") }),
      );
      return;
    }
    for (const [id, detail] of pl.details)
      if (!pl.openResults.has(id)) {
        const plot = detail.querySelector(".pl-curve-plot");
        if (plot) sized.unobserve(plot);
        pl.details.delete(id);
      }
    const items = list.map((entry) => {
      const { state } = view(entry);
      const ended = endedAt(entry);
      const took = tookOf(entry);
      const open = pl.openResults.has(entry.id);
      const item = el(
        "li",
        { class: "pl-r", "data-state": state, "data-open": open ? "1" : null },
        el(
          "button",
          {
            type: "button",
            class: "pl-r-toggle",
            "aria-expanded": String(open),
            onclick: () => {
              if (pl.openResults.has(entry.id)) pl.openResults.delete(entry.id);
              else pl.openResults.add(entry.id);
              renderResults(
                pl.state.queue.entries,
                groupSlots(pl.state.queue.entries),
              );
              refreshCurves();
            },
          },
          el("span", { class: "pl-r-icon", "aria-hidden": "true" }),
          el(
            "span",
            { class: "pl-r-main" },
            el(
              "span",
              { class: "pl-r-head" },
              groupChip(entry, slots),
              el("b", { class: "pl-r-title", text: entry.title || entry.id }),
            ),
            el(
              "span",
              { class: "pl-r-summary" },
              entry.result_summary ||
                (entry.progress && state === "early"
                  ? t("pl.results.early", {
                      step: num(entry.progress.step),
                      total: num(entry.progress.total),
                    })
                  : entry.progress &&
                      (state === "finished" || state === "stopped")
                    ? t(
                        entry.progress.source === "log"
                          ? "pl.results.unmarkedLog"
                          : "pl.results.unmarked",
                        {
                          step: num(entry.progress.step),
                          total: num(entry.progress.total),
                        },
                      )
                    : t("pl.results.noSummary")),
            ),
            entry.next_step
              ? el(
                  "span",
                  { class: "pl-r-next" },
                  el("b", { text: t("pl.results.next") }),
                  " ",
                  entry.next_step,
                )
              : null,
          ),
          el(
            "span",
            { class: "pl-r-when" },
            el("time", { text: clock(ended) }),
            took != null
              ? el("span", {
                  text: t("pl.results.took", { time: took }),
                })
              : null,
          ),
        ),
      );
      if (open) item.append(resultDetail(entry));
      return item;
    });
    box.replaceChildren(...items);
  }

  /** An opened result: its question, its curve and scores, notes, files */
  function resultDetail(entry) {
    const kept = pl.details.get(entry.id);
    if (kept) return kept;
    const detail = el("div", { class: "pl-r-detail" });
    pl.details.set(entry.id, detail);
    if (entry.why) detail.append(el("p", { class: "pl-why", text: entry.why }));
    if (entry.run_dir) {
      const chart = curveBox(entry.id);
      const scores = scoresBox(entry.id);
      detail.append(chart, scores);
      requestAnimationFrame(() => {
        chart.redraw();
        scores.redraw();
      });
    }
    const more = el("div", { class: "pl-run-more" });
    fillMore(more, entry);
    detail.append(more);
    return detail;
  }

  // ------------------------------------------------------------ history

  function renderHistory(entries, slots) {
    const list = finished(entries);
    const shown = pl.allHistory ? list : list.slice(0, HISTORY);
    const body = $("pl-history");
    $("pl-history-note").textContent = list.length
      ? t("pl.history.count", { n: list.length })
      : "";
    const content = shown.map((e) => [
      e.id,
      view(e).state,
      e.title,
      e.owner,
      endedAt(e),
      e.started_ms,
      slots.get(e.group),
    ]);
    if (!changed(body, [content, list.length])) return;
    if (!list.length) {
      body.replaceChildren(
        el(
          "tr",
          {},
          el("td", {
            colspan: "5",
            class: "pl-dim",
            text: t("pl.history.empty"),
          }),
        ),
      );
      return;
    }
    const rows = shown.map((entry) => {
      const { state } = view(entry);
      const ended = endedAt(entry);
      const took = tookOf(entry);
      return el(
        "tr",
        { title: entry.result_summary ?? entry.why ?? null },
        el("td", { class: "num", text: clock(ended) }),
        el("td", {}, stateBadge(state)),
        el(
          "td",
          { class: "pl-h-entry" },
          groupChip(entry, slots),
          el("code", { class: "pl-id", text: entry.id }),
          el("span", { class: "pl-h-title", text: entry.title }),
        ),
        el("td", { class: "num", text: took ?? "–" }),
        el("td", { text: entry.owner ?? "" }),
      );
    });
    if (list.length > shown.length)
      rows.push(
        el(
          "tr",
          { class: "pl-more-row" },
          el(
            "td",
            { colspan: "5" },
            el("button", {
              type: "button",
              class: "btn btn-small",
              text: t("pl.history.more", { n: list.length }),
              onclick: () => {
                pl.allHistory = true;
                renderHistory(
                  pl.state.queue.entries,
                  groupSlots(pl.state.queue.entries),
                );
              },
            }),
          ),
        ),
      );
    body.replaceChildren(...rows);
  }

  // ------------------------------------------------------------ the app

  window.addEventListener("app-route", ({ detail }) => {
    const shown = detail.app === "pipeline";
    if (shown === pl.shown) return;
    pl.shown = shown;
    if (shown) {
      pl.timelineAt = 0;
      poll();
    } else clearTimeout(pl.timer);
  });

  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && pl.shown) {
      // The samples of the hidden time come with the whole window
      pl.timelineAt = 0;
      poll();
    } else clearTimeout(pl.timer);
  });

  window.addEventListener("lang-change", () => {
    if (pl.state) render();
    for (const box of document.querySelectorAll("[data-curve], [data-scores]"))
      box.redraw?.();
  });
})();
