// Pipeline app: the GPUs and the experiment queue, live. The queue is a
// file the agents keep, AgentZero's runs/queue.json (agentzero-queue writes
// it; AgentZero's README has the format): what runs, what waits in the order
// it runs, why each entry runs (the question it answers) and what came out.
// Two GPUs take its entries: this host's (Linux) and the win11 VM's, whose
// runner (agentzero-win11 run) runs the entries it takes there (host: win11)
// and writes the VM's GPU, CPU and memory to runs/win11/gpu.json. The lab
// samples both machines every 5 s and follows each entry's processes here
// or its runner there, its run folder and its log (src/pipeline.rs). The
// page is one system, the machines side by side:
//
// - A machine: its name and state (running, idle, busy with work the queue
//   does not list, no word from its runner) with what its GPU takes next;
//   what runs on it, a card per entry (its state, progress and ETA, or for
//   a step without a counter its name and how long it runs, with the log's
//   latest line; the loss curve of its run folder, its latest validation
//   and copycat scores, its processes and its log), or while nothing does,
//   what comes next and what ran last there; its vitals in one tile
//   grammar (GPU busy, GPU memory, temperature, power; CPU, memory, disk:
//   a label, the value, its last half hour or a meter, a note); and the
//   processes on its GPU. The two panels share their rows, so both
//   machines' parts sit side by side;
// - GPU timeline: a chart per GPU, side by side: busy % with its machine's
//   CPU over it, memory, and lanes of what ran on it when (each bar with
//   its job's CPU), over the last 1 to 12 hours; stretches without a
//   reading shaded; one crosshair over both;
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
// Timeline rows (GET timeline, and state's samples) are arrays, read
// through ROW: this host's GPU and CPU, the VM's GPU, the load, each
// entry's CPU in cores, the free space of each disk ({disk: GB}:
// pve:rpool, linux:/, win11:C:, those read), the VM's CPU and memory;
// null (or missing, in rows of before) where nothing was read.
//
// It polls while shown (the state every 5 s, the running entries' curves
// every 30 s) and stops while another app is shown or the tab is hidden.
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
  /** Most lanes of entries under a GPU's chart */
  const MAX_LANES = 3;
  /** Where a timeline row keeps each number: this host's GPU busy %, its
   * memory and the queue's jobs' part of it (MiB), °C, W; this host's CPU
   * % and RAM MiB; the VM's GPU busy %, memory, °C, W; the load; each
   * entry's CPU in cores ({id: cores}); each disk's free GB ({disk: GB});
   * the VM's CPU % and RAM MiB */
  const ROW = {
    t: 0,
    busy: 1,
    mem: 2,
    jobs: 3,
    temp: 4,
    power: 5,
    cpu: 6,
    ram: 7,
    rBusy: 8,
    rMem: 9,
    rTemp: 10,
    rPower: 11,
    load: 12,
    cores: 13,
    free: 14,
    rCpu: 15,
    rRam: 16,
  };
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
  /** A GPU this hot is worth a look, °C */
  const HOT_C = 83;
  /** The VM's runner holds new entries back under this much free on its
   * C:, bytes (AgentZero's storage guard) */
  const VM_DISK_LOW = 50e9;
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
    /** The timeline: its window, rows (see ROW) and when each entry was
     * seen running */
    minutes: remembered("range", 180),
    samples: [],
    spans: {},
    timelineAt: 0,
    samplingSince: null,
    /** The time under the pointer in either chart, and which chart */
    hoverT: null,
    hoverKey: null,
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

  /** Put `kids` in `box` in place of what it holds, leaving out the ones
   * that are not there (null, false), which the DOM would write as text */
  function fill(box, ...kids) {
    box.replaceChildren(
      ...kids.flat().filter((kid) => kid != null && kid !== false),
    );
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
   * What a run does now: `steps` (counting, with its bar and ETA),
   * `working` (live, no counter of its own: a step its job script started
   * after the counter's last line, its training over while the job goes
   * on, or no counter at all), `stalled` (live, its step unmoved for a
   * while), `ended` (not live, its run over), or null without a word on it
   */
  function phaseOf(entry, state) {
    const p = entry.progress;
    const a = entry.activity;
    const live = state === "running" || state === "detected";
    if (!live) {
      if (!p) return null;
      return p.ended || completeOf(p) ? "ended" : "steps";
    }
    // The job's own step started after the counter's last line: the
    // counter was an earlier step's
    const newer =
      a?.step &&
      (!p ||
        (p.source === "log"
          ? !a.counted
          : a.step_ms != null && p.moved_ms != null && a.step_ms > p.moved_ms));
    if (!p || p.ended || newer) return "working";
    const stall = pl.state?.stall_ms ?? STALL_MS;
    if (p.moved_ms != null && Date.now() - p.moved_ms > stall) return "stalled";
    return "steps";
  }

  /** What a live entry without a counter does, in a word: the step its
   * job script started, else its main process, else null */
  const workName = (entry) => entry.activity?.step ?? entry.live?.main ?? null;

  /** Since when it does it: its step's start, else, its training over,
   * the last word of its run folder, else its start */
  const workSince = (entry) =>
    entry.activity?.step_ms ??
    (entry.progress?.ended ? entry.progress.moved_ms : null) ??
    startedAt(entry);

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

  /** Which machine an entry runs on: `remote` (the VM its host or device
   * names), else `local` (this host's GPU or CPU) */
  const machineKey = (entry) => (entry.remote ? "remote" : "local");

  /** A device in words: "either GPU", "win11 GPU", "CPU" */
  function deviceWord(device) {
    if (!device) return null;
    const key = `pl.device.${device}`;
    const word = t(key);
    if (word !== key) return word;
    const host = device.startsWith("gpu:") ? device.slice(4) : null;
    return host ? t("pl.device.host", { host }) : device;
  }

  /** Whether an entry of `device` (none: either GPU) can run on `runner`
   * (`cpu`, `gpu:linux`, `gpu:win11`): the lab's `fits` */
  function fits(device, runner) {
    const have = (device ?? "").trim() || "gpu";
    if (have === "cpu" || runner === "cpu") return have === runner;
    return have === "gpu" || have === runner;
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

  /** A light: a dot of its level's colour and a word (a machine's state) */
  function light(level, word, title) {
    return el(
      "span",
      { class: "pl-light", "data-level": level, title: title ?? null },
      el("i", { "aria-hidden": "true" }),
      word,
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
        ? pl.samples[pl.samples.length - 1][ROW.t]
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
      // Aborted: the app was left, and polls again when shown
      if (!isAbort(error)) {
        pl.error = error.message;
        renderError();
      }
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
    const last = pl.samples.length
      ? pl.samples[pl.samples.length - 1][ROW.t]
      : 0;
    for (const row of rows) if (row[ROW.t] > last) pl.samples.push(row);
    const from = Date.now() - pl.minutes * 60000;
    while (pl.samples.length && pl.samples[0][ROW.t] < from) pl.samples.shift();
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
      // Aborted (the app was left): nothing learnt, read again when shown
      if (isAbort(error)) {
        known.pending = false;
        if (!known.data) pl.runs.delete(id);
        return;
      }
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

  /** Open logs are read again while open; the others are forgotten (a
   * card's latest line comes with the state) */
  function refreshLogs() {
    for (const [id, log] of pl.logs) {
      if (!log.open) pl.logs.delete(id);
      else if (!log.pending && Date.now() - log.at > LOG_MS - 500) loadLog(id);
    }
  }

  /** The log kept for an entry, made when missing */
  function logOf(id) {
    let log = pl.logs.get(id);
    if (!log) {
      log = { lines: null, at: 0, open: false };
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
      // Aborted (the app was left): read again when shown
      if (isAbort(error)) {
        log.pending = false;
        return;
      }
      log.error = error.message;
    }
    log.at = Date.now();
    log.pending = false;
    for (const pre of document.querySelectorAll(
      `[data-log="${CSS.escape(id)}"]`,
    ))
      fillLog(pre, log);
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
    $("pl-alert").hidden = true;
    renderMachines(state, entries, slots);
    renderHost(state.storage);
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
    const alert = $("pl-alert");
    alert.textContent = t("pl.offline", { error: pl.error });
    alert.hidden = false;
  }

  function renderChip(state) {
    const chip = $("pl-chip");
    const running = state.queue.entries.filter((e) => aliveOf(e));
    // What to look at: entries with no process or no word, stalled steps
    const trouble = state.queue.entries.filter((e) => {
      const { state: word } = view(e);
      return (
        word === "gone" ||
        word === "unknown" ||
        (aliveOf(e) && phaseOf(e, word) === "stalled")
      );
    });
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
    chip.dataset.level = trouble.length
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
      lastLine: el("p", { class: "pl-last-line" }),
      chart: curveBox(id),
      scores: scoresBox(id),
      stats: el("div", { class: "pl-run-stats" }),
      more: el("div", { class: "pl-run-more" }),
    };
    const part = card.parts;
    // Its run folder's curve and scores, side by side where the card is wide
    part.data = el("div", { class: "pl-run-data" }, part.chart, part.scores);
    card.append(
      part.head,
      part.title,
      part.why,
      part.warn,
      part.progress,
      part.lastLine,
      part.data,
      part.stats,
      part.more,
    );
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
    fill(
      part.head,
      stateBadge(
        state,
        state === "detected"
          ? t("pl.state.detectedNote", { status: entry.status })
          : null,
      ),
      groupChip(entry, slots),
      el("code", { class: "pl-id", text: entry.id }),
      // Its machine is the column's; a CPU job says so
      entry.device === "cpu"
        ? el("span", {
            class: "pl-where",
            text: t("pl.device.cpu"),
            title: t("pl.where.device", { device: deviceWord(entry.device) }),
          })
        : null,
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
    const phase = phaseOf(entry, state);
    fillProgress(part.progress, entry, state, phase);
    fillLastLine(part.lastLine, entry, phase);
    part.data.hidden = !entry.run_dir;
    if (entry.run_dir) part.scores.redraw();
    fillStats(part.stats, entry);
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

  /** The log's latest line, where the progress does not say it all: a
   * step without a counter, a stalled one, and an entry nothing of which
   * runs (often the best word on what happened) */
  function fillLastLine(box, entry, phase) {
    const activity = entry.activity;
    const wanted = Boolean(activity?.line && phase !== "steps");
    box.hidden = !wanted;
    if (!wanted) return;
    box.replaceChildren(
      el("span", {
        class: "pl-stat-label",
        text: activity.line_ms
          ? t("pl.log.lastAgo", { ago: ago(activity.line_ms) })
          : t("pl.log.last"),
      }),
      el("code", { text: activity.line, title: activity.line }),
    );
  }

  /** A bar: its fill at `fraction` (null: a band that keeps going while
   * live, else empty), in the phase's look */
  function progressBar(fraction, live, phase) {
    return el(
      "div",
      {
        class: "pl-bar",
        "data-phase": phase,
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
  }

  /**
   * The progress in the phase's words: counting (the bar, %, the step,
   * its speed and the ETA), working (a band that keeps going: the step's
   * name and how long it runs, and where the training ended if it did),
   * stalled (the bar where its step stopped, and for how long) or ended
   * (the bar where it stopped, early or at its last step)
   */
  function fillProgress(box, entry, state, phase) {
    const p = entry.progress;
    const live = state === "running" || state === "detected";
    box.hidden = !phase;
    if (!phase) return box.replaceChildren();
    const fraction =
      p?.total > 0 ? Math.min(1, Math.max(0, p.step / p.total)) : null;
    const counted = { step: num(p?.step), total: num(p?.total) };
    const facts = [];
    if (phase === "working") {
      // "decode-lead · running 4 min": its step's (or process's) name, else
      // that it counts nothing
      const name = workName(entry);
      const since = workSince(entry);
      const time = since != null ? duration(Date.now() - since) : null;
      facts.push(
        el(
          "b",
          { class: "pl-phase", title: t("pl.work.note") },
          name
            ? [
                el("code", { class: "pl-step", text: name }),
                time ? ` · ${t("pl.work.running", { time })}` : "",
              ]
            : time
              ? t("pl.work.plain", { time })
              : t("pl.work.plainNow"),
        ),
      );
      // Its training's end, when a training came before
      if (p?.ended) {
        const how =
          p.total == null
            ? "At"
            : earlyOf(p)
              ? "Early"
              : completeOf(p)
                ? ""
                : "At";
        facts.push(el("span", { text: t(`pl.phase.trained${how}`, counted) }));
        if (p.best_step != null)
          facts.push(
            el("span", {
              text: t("pl.phase.best", { step: num(p.best_step) }),
            }),
          );
      }
      box.replaceChildren(
        progressBar(null, true, "working"),
        el("div", { class: "pl-progress-row" }, facts),
      );
      return;
    }
    if (phase === "stalled") {
      if (fraction != null)
        facts.push(
          el("b", { class: "pl-pct", text: `${num(fraction * 100)}%` }),
        );
      facts.push(
        el("span", {
          text:
            p.source === "log"
              ? t("pl.progress.items", {
                  done: counted.step,
                  total: counted.total,
                })
              : t("pl.progress.steps", counted),
        }),
        el("b", {
          class: "pl-phase",
          text: t("pl.phase.stalled", {
            time: duration(Date.now() - p.moved_ms),
          }),
        }),
      );
      box.replaceChildren(
        progressBar(fraction, false, "stalled"),
        el("div", { class: "pl-progress-row" }, facts),
      );
      return;
    }
    if (phase === "ended") {
      const how =
        p.total == null
          ? "At"
          : earlyOf(p)
            ? "Early"
            : completeOf(p)
              ? ""
              : "At";
      facts.push(
        el("b", {
          class: "pl-phase",
          text: t(`pl.phase.ended${how}`, counted),
        }),
      );
      if (p.best_step != null)
        facts.push(
          el("span", { text: t("pl.phase.best", { step: num(p.best_step) }) }),
        );
      if (p.updated_ms)
        facts.push(
          el("span", {
            class: "pl-dim",
            text: t("pl.progress.written", { ago: ago(p.updated_ms) }),
          }),
        );
      box.replaceChildren(
        progressBar(fraction ?? 1, false, "ended"),
        el("div", { class: "pl-progress-row" }, facts),
      );
      return;
    }
    // Counting: the step's name when the job's script names the one that
    // counts (its log's counter came after its start, or its run folder
    // was written since)
    if (fraction != null)
      facts.push(el("b", { class: "pl-pct", text: `${num(fraction * 100)}%` }));
    const a = entry.activity;
    const step =
      a?.step &&
      (p.source === "log"
        ? a.counted
        : a.step_ms != null && p.moved_ms != null && a.step_ms <= p.moved_ms)
        ? a.step
        : null;
    facts.push(
      el(
        "span",
        {},
        step ? el("code", { class: "pl-step", text: step }) : null,
        p.source === "log"
          ? t("pl.progress.items", { done: counted.step, total: counted.total })
          : p.total != null
            ? t("pl.progress.steps", counted)
            : t("pl.progress.step", counted),
      ),
    );
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
    const eta = p.eta_ms ?? (live ? entry.eta_ms : null);
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
    if (!live && p.updated_ms)
      facts.push(
        el("span", {
          class: "pl-dim",
          text: t("pl.progress.written", { ago: ago(p.updated_ms) }),
        }),
      );
    box.replaceChildren(
      progressBar(fraction, live, "steps"),
      el("div", { class: "pl-progress-row" }, facts),
    );
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
          [
            live.main ? t("pl.proc.main", { name: live.main }) : null,
            `${t("pl.proc.found", { how: t(`pl.found.${live.found_by}`) })}: ${live.pids.join(", ")}`,
          ]
            .filter(Boolean)
            .join("\n"),
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
    const height = width < 420 ? 150 : 180;
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
    fill(
      box,
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
    // A value past a given range stays on its edge, inside the tile
    const sy = (v) =>
      3 +
      ((v1 - Math.min(v1, Math.max(v0, v))) / (v1 - v0 || 1)) * (height - 6);
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

  // ------------------------------------------------------------ machines

  /**
   * This host and the VM as the page shows them, alike: the title, the
   * GPU's, CPU's, memory's and disk's readings (null where there are
   * none: the VM's while its runner's file is stale, or those its runner
   * does not read yet), the rows of each in the samples, and the runner
   * of its GPU. The VM is there while its runner has a file, or an entry
   * names it.
   */
  function machinesOf(state) {
    const memory = state.memory;
    const local = {
      key: "local",
      host: null,
      title: machineTitle(null, state.gpu?.name),
      gpu: state.gpu,
      gpuError: state.gpu_error,
      cpu: state.cpu?.percent != null ? state.cpu : null,
      ram: memory
        ? {
            used: memory.total - memory.available,
            total: memory.total,
            swapTotal: memory.swap_total,
            swapFree: memory.swap_free,
          }
        : null,
      disk: diskOf(state.storage?.root, "/"),
      rows: {
        busy: ROW.busy,
        mem: ROW.mem,
        jobs: ROW.jobs,
        temp: ROW.temp,
        power: ROW.power,
        cpu: ROW.cpu,
      },
      runner: "gpu:linux",
      fresh: true,
    };
    const remote = state.remote;
    const host =
      remote?.host ?? state.queue.entries.find((e) => e.remote)?.remote.host;
    if (!host) return [local];
    const fresh = Boolean(remote?.fresh);
    return [
      local,
      {
        key: "remote",
        host,
        title: machineTitle(host, remoteGpu()),
        gpu: fresh ? remote.gpu : null,
        gpuError: remote?.error ?? null,
        cpu: fresh && remote.cpu?.percent != null ? remote.cpu : null,
        ram: fresh && remote.memory ? { ...remote.memory } : null,
        disk: fresh ? diskOf(remote.disk, "C:") : null,
        rows: {
          busy: ROW.rBusy,
          mem: ROW.rMem,
          jobs: null,
          temp: ROW.rTemp,
          power: ROW.rPower,
          cpu: ROW.rCpu,
        },
        runner: `gpu:${host}`,
        remote,
        fresh,
      },
    ];
  }

  /** A disk's reading ({free, total} bytes) with its path, or null */
  const diskOf = (disk, path) =>
    disk?.total > 0 ? { free: disk.free, total: disk.total, path } : null;

  function renderMachines(state, entries, slots) {
    const list = machinesOf(state);
    $("pl-machines").dataset.count = String(list.length);
    $("pl-m-remote").hidden = list.length < 2;
    for (const m of list) {
      const panel = $(`pl-m-${m.key}`);
      // What runs here: those that run first, the ones to look at after
      const running = entries.filter(
        (e) => view(e).place === "running" && machineKey(e) === m.key,
      );
      const doubtful = (e) => ["gone", "unknown"].includes(view(e).state);
      running.sort(
        (a, b) =>
          doubtful(a) - doubtful(b) ||
          (startedAt(a) ?? 0) - (startedAt(b) ?? 0),
      );
      fillHead(panel, m, running, entries);
      const tiles = vitalsOf(m, state);
      const grid = panel.querySelector(".pl-tiles");
      grid.style.setProperty("--n", String(tiles.length));
      grid.dataset.host = String(
        tiles.filter((tile) => tile.dataset.row === "host").length,
      );
      grid.replaceChildren(...tiles);
      panel
        .querySelector(".pl-m-procs")
        .replaceChildren(...processesOf(m, state));
      renderNow(panel.querySelector(".pl-m-now"), m, running, entries, slots);
    }
  }

  /**
   * A machine's state in a word and a light: what runs there, else idle,
   * busy with work the queue does not list, or, for the VM, no word from
   * its runner or unreachable; then a light for each thing to look at:
   * entries said to run with nothing of them left or no word of them,
   * stalled ones, its runner's hold and whether it runs
   */
  function machineState(m, running, entries) {
    const live = running.filter((e) => aliveOf(e));
    const doubtful = running.filter((e) =>
      ["gone", "unknown"].includes(view(e).state),
    );
    const flags = [];
    for (const state of ["gone", "unknown"]) {
      const these = doubtful.filter((e) => view(e).state === state);
      if (these.length)
        flags.push(
          light(
            "warning",
            t(`pl.m.${state}`, { n: these.length }),
            these.map((e) => `${e.id}: ${e.title}`).join("\n"),
          ),
        );
    }
    const stalled = live.filter((e) => phaseOf(e, view(e).state) === "stalled");
    if (stalled.length)
      flags.push(
        light(
          "warning",
          t("pl.m.stalled", { n: stalled.length }),
          stalled.map((e) => `${e.id}: ${e.title}`).join("\n"),
        ),
      );
    if (m.key === "remote") {
      const r = m.remote;
      // Work waits for its runner while that runner is down: then that is
      // wrong, not a choice
      const waited = entries.some((e) => e.next_on?.includes(m.runner));
      if (r && !r.runner)
        flags.push(
          light(
            waited ? "warning" : "off",
            t("pl.tile.runnerStopped"),
            t("pl.m.runnerDownNote"),
          ),
        );
      if (r?.hold)
        flags.push(light("note", t("pl.tile.hold"), t("pl.m.holdNote")));
      // AgentZero's storage guard holds it: the pool, this host or its C:
      if (r?.space) flags.push(light("warning", t("pl.m.spaceHold"), r.space));
      if (!r?.at_ms) return { main: light("warning", t("pl.m.never")), flags };
      if (!r.fresh)
        return {
          main: light(
            live.length || doubtful.length || waited ? "warning" : "off",
            t("pl.m.quiet", { ago: ago(r.at_ms) }),
            t("pl.m.quietNote", { clock: clock(r.at_ms), ago: ago(r.at_ms) }),
          ),
          flags,
        };
      if (r.error)
        return {
          main: light("warning", t("pl.tile.unreachable"), r.error),
          flags,
        };
    }
    if (live.length)
      return {
        main: light("good", t("pl.m.running", { n: live.length })),
        flags,
      };
    if (m.gpu?.util >= 50)
      return {
        main: light("note", t("pl.m.busyOther", { util: num(m.gpu.util) })),
        flags,
      };
    if (m.key === "local" && !m.gpu)
      return {
        main: light("warning", t("pl.tile.noReading"), m.gpuError),
        flags,
      };
    return { main: light("off", t("pl.m.idle")), flags };
  }

  /** What a machine's GPU takes next: the entry its runner takes next,
   * else the first waiting one it could take, with what that waits for */
  function nextFor(m, entries) {
    const list = waiting(entries);
    const named = list.find((e) => e.next_on?.includes(m.runner));
    if (named) return { entry: named, waits: [] };
    const later = list.find(
      (e) =>
        e.status === "queued" &&
        fits(e.device, m.runner) &&
        (m.key === "local" || e.command),
    );
    return later ? { entry: later, waits: later.waits ?? [] } : null;
  }

  /** The last entry that ran on a machine's GPU */
  const lastFor = (m, entries) =>
    finished(entries).find(
      (e) => machineKey(e) === m.key && e.device !== "cpu",
    ) ?? null;

  /** A machine's head: its name, its state, and what its GPU takes next */
  function fillHead(panel, m, running, entries) {
    const title = panel.querySelector(".pl-m-title");
    title.textContent = m.gpu?.pstate
      ? `${m.title} · ${m.gpu.pstate}`
      : m.title;
    const { main, flags } = machineState(m, running, entries);
    panel.querySelector(".pl-m-state").replaceChildren(main, ...flags);
    const next = nextFor(m, entries);
    const box = panel.querySelector(".pl-m-next");
    box.hidden = !next;
    if (!next) return box.replaceChildren();
    const { entry, waits } = next;
    box.title = [
      `${entry.id}: ${entry.title}`,
      waits.length ? t("pl.queue.after", { ids: waits.join(", ") }) : null,
      t(waits.length ? "pl.m.nextWaitsNote" : "pl.m.nextNote"),
    ]
      .filter(Boolean)
      .join("\n");
    fill(
      box,
      el("span", { class: "pl-m-next-label", text: t("pl.m.next") }),
      el("code", { text: entry.id }),
      waits.length
        ? el("span", {
            class: "pl-waits",
            text: t("pl.queue.after", { ids: waits.join(", ") }),
          })
        : null,
    );
  }

  /** A machine's running cards, kept by entry so charts and open logs
   * stay, then what its GPU takes next; while nothing runs there, what
   * comes next and what ran last */
  function renderNow(box, m, running, entries, slots) {
    const cards = new Map(
      [...box.querySelectorAll(".pl-run")].map((c) => [c.dataset.id, c]),
    );
    const kept = running.map((entry) => {
      const card = cards.get(entry.id) ?? runCard(entry.id);
      cards.delete(entry.id);
      fillRunCard(card, entry, slots);
      return card;
    });
    for (const gone of cards.values()) sized.unobserve(gone.parts.chart.plot);
    if (!kept.length) return box.replaceChildren(idleBlock(m, entries, slots));
    fill(box, kept, nextLine(m, entries, slots));
  }

  /** What a machine's GPU takes next, in a line, or null */
  function nextLine(m, entries, slots) {
    const next = nextFor(m, entries);
    if (!next) return null;
    return entryLine(
      next.waits.length
        ? t("pl.m.nextWaits", { ids: next.waits.join(", ") })
        : t("pl.m.nextHere"),
      next.entry,
      slots,
      next.entry.why,
    );
  }

  /** A machine where nothing of the queue runs: why, when its runner holds
   * or is quiet, what comes next there and what ran last */
  function idleBlock(m, entries, slots) {
    const notes = [];
    const note = (text, level) =>
      el("p", { class: "pl-idle-note", "data-level": level ?? null, text });
    if (m.gpu?.util >= 50)
      notes.push(note(t("pl.idleBusy", { util: num(m.gpu.util) })));
    const r = m.remote;
    if (r) {
      if (!r.runner) notes.push(note(t("pl.m.runnerDownNote"), "warning"));
      if (r.hold) notes.push(note(t("pl.m.holdNote")));
      if (r.space) notes.push(note(r.space, "warning"));
      if (r.at_ms && !r.fresh)
        notes.push(
          note(
            t("pl.m.quietNote", { clock: clock(r.at_ms), ago: ago(r.at_ms) }),
          ),
        );
      if (r.fresh && r.error) notes.push(note(r.error, "warning"));
    }
    const last = lastFor(m, entries);
    return el(
      "div",
      { class: "pl-idle" },
      el(
        "p",
        { class: "pl-idle-text" },
        svg(
          "svg",
          {
            class: "app-icon pl-idle-icon",
            viewBox: "0 0 24 24",
            "aria-hidden": "true",
          },
          svg("use", { href: "/icons/app-pipeline.svg#i" }),
        ),
        t("pl.m.idleText"),
      ),
      notes,
      nextLine(m, entries, slots),
      last
        ? entryLine(
            t("pl.m.lastHere", {
              clock: clock(endedAt(last)),
              took: tookOf(last) ?? "–",
            }),
            last,
            slots,
            last.result_summary,
            view(last).state,
          )
        : null,
    );
  }

  /** An entry in a line: what it is to the machine, its group, title and
   * id, and a line under it (its question, or its result) */
  function entryLine(label, entry, slots, sub, state) {
    return el(
      "div",
      {
        class: "pl-line",
        style: entry.group
          ? `--g: var(--pl-c${slots.get(entry.group) ?? 1})`
          : null,
      },
      el(
        "span",
        { class: "pl-line-label" },
        state ? stateBadge(state) : null,
        label,
      ),
      el(
        "span",
        { class: "pl-line-title" },
        groupChip(entry, slots),
        el("b", { text: entry.title || entry.id }),
        " ",
        el("code", { class: "pl-id", text: entry.id }),
      ),
      sub ? el("span", { class: "pl-line-sub", text: sub, title: sub }) : null,
    );
  }

  /**
   * A machine's vitals, one grammar for both: its GPU's busy, memory,
   * temperature and power, then its CPU, memory and disk. Each tile is a
   * label, the value, its last half hour or a meter, and a note; without
   * a reading the value is "–" and the note says why. The VM's rows come
   * from its runner's file; a runner of before reads no CPU or memory.
   */
  function vitalsOf(m, state) {
    const now = Date.now();
    const recent = pl.samples.filter(
      (row) => row[ROW.t] >= now - SPARK_MINUTES * 60000,
    );
    // The last half hour of row index `i`, missing readings as gaps
    const course = (i, options) =>
      i == null
        ? null
        : sparkline(
            recent.map((row) => [row[ROW.t], row[i]]),
            {
              span: [now - SPARK_MINUTES * 60000, now],
              gap: Math.max(20000, ((pl.minutes * 60000) / 720) * 3),
              ...options,
            },
          );
    const gpu = m.gpu;
    // Why a value is missing: no reading, or, of the VM's CPU and memory, a
    // runner that does not read them yet (its file fresh and whole)
    const noReading = t("pl.tile.noReading");
    const notRead =
      m.key === "remote" && m.fresh && !m.gpuError
        ? t("pl.tile.notRead")
        : noReading;
    const tiles = [];
    const tile = (row, label, value, extra = {}) =>
      pulseTile(row, label, value ?? "–", {
        ...extra,
        note:
          value == null && extra.note == null
            ? row === "host"
              ? notRead
              : noReading
            : extra.note,
      });
    tiles.push(
      tile(
        "gpu",
        t("pl.tile.gpuBusy"),
        gpu?.util != null ? `${num(gpu.util)}%` : null,
        {
          spark: course(m.rows.busy, { cls: "s1", lo: 0, hi: 100 }),
          note: gpu ? t("pl.tile.last", { minutes: SPARK_MINUTES }) : null,
        },
      ),
    );
    const used = gpu?.mem_used_mib;
    const total = gpu?.mem_total_mib;
    // This host knows the queue's share of its GPU's memory
    const jobs =
      m.key === "local"
        ? state.queue.entries.reduce(
            (sum, e) => sum + (e.live?.gpu_mib ?? 0),
            0,
          )
        : null;
    tiles.push(
      tile("gpu", t("pl.tile.gpuMem"), used != null ? gib(used) : null, {
        meter:
          used != null && total
            ? stackedMeter(
                jobs != null
                  ? [
                      { value: jobs, cls: "pl-m-jobs" },
                      { value: Math.max(0, used - jobs), cls: "pl-m-other" },
                    ]
                  : [{ value: used, cls: "pl-m-other" }],
                total,
              )
            : null,
        note: used == null ? null : t("pl.tile.of", { total: gib(total) }),
        title:
          used != null && jobs != null
            ? t("pl.tile.gpuMemNote", {
                jobs: gib(jobs),
                other: gib(Math.max(0, used - jobs)),
              })
            : null,
        level: total && used / total > 0.9 ? "warning" : null,
      }),
    );
    tiles.push(
      tile(
        "gpu",
        t("pl.tile.temp"),
        gpu?.temp_c != null ? `${num(gpu.temp_c)} °C` : null,
        {
          spark: course(m.rows.temp, { cls: "s2" }),
          note:
            gpu?.temp_c == null
              ? null
              : gpu.fan != null
                ? t("pl.tile.fan", { fan: num(gpu.fan) })
                : "",
          level: gpu?.temp_c >= HOT_C ? "warning" : null,
        },
      ),
    );
    tiles.push(
      tile(
        "gpu",
        t("pl.tile.power"),
        gpu?.power_w != null ? `${num(gpu.power_w)} W` : null,
        {
          // Against its limit where known, else its course
          meter:
            gpu?.power_w != null && gpu.power_limit_w
              ? stackedMeter(
                  [{ value: gpu.power_w, cls: "pl-m-power" }],
                  gpu.power_limit_w,
                )
              : null,
          spark:
            gpu?.power_w != null && gpu.power_limit_w
              ? null
              : course(m.rows.power, { cls: "s2" }),
          note:
            gpu?.power_w == null
              ? null
              : gpu.power_limit_w
                ? t("pl.tile.powerOf", { limit: num(gpu.power_limit_w) })
                : "",
          title:
            gpu?.sm_mhz != null
              ? t("pl.tile.clock", { clock: num(gpu.sm_mhz) })
              : null,
        },
      ),
    );
    const cpu = m.cpu;
    // The queue's share of this host's CPU, in cores
    const jobCores =
      m.key === "local"
        ? state.queue.entries.reduce(
            (sum, e) => sum + (e.live?.cpu_percent ?? 0) / 100,
            0,
          )
        : 0;
    tiles.push(
      tile("host", t("pl.tile.cpu"), cpu ? `${num(cpu.percent)}%` : null, {
        spark: course(m.rows.cpu, { cls: "s3", lo: 0, hi: 100 }),
        note: cpu
          ? [
              cpu.cores != null
                ? t("pl.tile.threads", { cores: cpu.cores })
                : null,
              cpu.load
                ? t("pl.tile.load", { load: num(cpu.load[0], 1) })
                : null,
              jobCores > 0
                ? t("pl.tile.cpuJobs", { cores: num(jobCores, 1) })
                : null,
            ]
              .filter(Boolean)
              .join(" · ")
          : null,
      }),
    );
    const ram = m.ram;
    const swapUsed = ram?.swapTotal ? ram.swapTotal - ram.swapFree : 0;
    const swapFull = ram?.swapTotal > 0 && ram.swapFree < ram.swapTotal * 0.05;
    tiles.push(
      tile("host", t("pl.tile.ram"), ram ? gibBytes(ram.used) : null, {
        meter: ram
          ? stackedMeter([{ value: ram.used, cls: "pl-m-ram" }], ram.total)
          : null,
        note: ram
          ? [
              t("pl.tile.of", { total: gibBytes(ram.total) }),
              ram.swapTotal
                ? t(swapFull ? "pl.tile.swapFull" : "pl.tile.swap", {
                    used: gibBytes(swapUsed),
                  })
                : null,
            ]
              .filter(Boolean)
              .join(" · ")
          : null,
        title: ram
          ? t("pl.tile.available", {
              available: gibBytes(ram.total - ram.used),
            })
          : null,
        level: !ram
          ? null
          : ram.total - ram.used < ram.total * 0.1
            ? "critical"
            : swapFull
              ? "warning"
              : null,
      }),
    );
    // The disk the machine runs from, where it is read (this host's `/`,
    // the VM's `C:`; both are volumes on the Proxmox host's pool, below)
    if (m.disk)
      tiles.push(
        tile("host", t("pl.tile.disk"), formatBytes(m.disk.free), {
          meter: stackedMeter(
            [{ value: m.disk.total - m.disk.free, cls: "pl-m-disk" }],
            m.disk.total,
          ),
          note: t("pl.tile.diskNote", {
            total: formatBytes(m.disk.total),
            path: m.disk.path,
          }),
          level:
            m.disk.free < m.disk.total * 0.1 ||
            (m.key === "remote" && m.disk.free < VM_DISK_LOW)
              ? "warning"
              : null,
        }),
      );
    return tiles;
  }

  /** A vital's tile: label, value, its course or a meter, a note */
  function pulseTile(row, label, value, { spark, meter, note, level, title }) {
    return el(
      "div",
      {
        class: "pl-tile",
        "data-row": row,
        "data-level": level ?? null,
        title: title ?? null,
      },
      el("span", { class: "pl-tile-label", text: label }),
      el("span", { class: "pl-tile-value", text: value }),
      spark ?? meter ?? el("span", { class: "pl-tile-gap" }),
      note ? el("span", { class: "pl-tile-note", text: note }) : null,
    );
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

  /** A machine's processes on its GPU: memory, name, entry (this host's
   * compute processes and the rest of the memory; the VM's Python
   * processes, whose memory Windows does not tell) */
  function processesOf(m, state) {
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
    const head = el("h3", { class: "pl-sub", text: t("pl.procs.title") });
    const empty = (text) => [head, el("p", { class: "panel-note", text })];
    if (m.key === "local") {
      const gpu = m.gpu;
      if (!gpu) return empty(t("pl.noGpu", { error: state.gpu_error ?? "" }));
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
      if (!rows.length) return empty(t("pl.procs.none"));
      return [head, el("ul", { class: "pl-proc-list" }, rows)];
    }
    const remote = m.remote;
    if (!m.fresh)
      return empty(
        remote?.at_ms
          ? t("pl.procs.noReading", { host: m.host, ago: ago(remote.at_ms) })
          : t("pl.m.never"),
      );
    const job = remote.job;
    const rows = remote.processes.map((p) =>
      row(
        p.memory_mib,
        remote.gpu?.mem_total_mib,
        p.name,
        p.pid != null ? `pid ${p.pid}` : null,
        /^python/i.test(p.name) ? job : null,
        t("pl.procs.remoteNote", { host: m.host }),
      ),
    );
    if (!rows.length) return empty(t("pl.procs.none"));
    return [head, el("ul", { class: "pl-proc-list" }, rows)];
  }

  /**
   * The Proxmox host both machines are VMs on, a strip under them: its
   * level as a light (the watched pool, rpool, where every VM's disk
   * lives: fine, low, almost full or no reading) and when its pools were
   * read, then its pools as tiles, the watched one first with its edge in
   * its level's colour; hidden without a storage host (`[pipeline]
   * storage_host = ""`)
   */
  function renderHost(storage) {
    const panel = $("pl-host");
    panel.hidden = !storage?.host;
    if (panel.hidden) return;
    const level = storage.level;
    const watched = storage.pools.find((p) => p.name === storage.watched);
    panel.querySelector(".pl-m-title").textContent = t("pl.host.title", {
      host: storage.host,
    });
    const word = !level
      ? t("pl.storage.noAnswer")
      : watched
        ? t(level === "ok" ? "pl.host.ok" : `pl.storage.chip.${level}`, {
            pool: watched.name,
            free: formatBytes(watched.free),
          })
        : t("pl.storage.chip.unknown", { pool: storage.watched });
    fill(
      panel.querySelector(".pl-m-state"),
      light(
        !level
          ? "off"
          : level === "ok"
            ? "good"
            : level === "critical"
              ? "critical"
              : "warning",
        word,
        storageLines(storage).join("\n"),
      ),
      el("span", {
        class: "pl-dim pl-host-read",
        text: storage.error
          ? t("pl.storage.error", { error: storage.error })
          : storage.pools_ms
            ? t("pl.storage.read", { time: clock(storage.pools_ms) })
            : null,
      }),
    );
    const edge = level === "critical" ? "critical" : "warning";
    const limits = t("pl.storage.limits", {
      low: formatBytes(storage.low_free),
      cap: storage.low_cap,
      critical: formatBytes(storage.critical_free),
    });
    const pools = [...storage.pools].sort(
      (a, b) =>
        (b.name === storage.watched) - (a.name === storage.watched) ||
        a.name.localeCompare(b.name),
    );
    panel.querySelector(".pl-host-pools").replaceChildren(
      ...pools.map((pool) => {
        const isWatched = pool.name === storage.watched;
        return pulseTile(
          "host",
          isWatched ? t("pl.host.watched", { pool: pool.name }) : pool.name,
          formatBytes(pool.free),
          {
            meter: stackedMeter(
              [{ value: pool.alloc, cls: "pl-m-disk" }],
              pool.size,
            ),
            note: t("pl.host.poolNote", {
              size: formatBytes(pool.size),
              cap: Math.round(pool.cap),
            }),
            level: isWatched && level !== "ok" ? edge : null,
            title:
              [
                pool.frag != null
                  ? t("pl.host.frag", { frag: Math.round(pool.frag) })
                  : null,
                isWatched ? limits : null,
              ]
                .filter(Boolean)
                .join("\n") || null,
          },
        );
      }),
    );
  }

  // ------------------------------------------------------------ timeline

  /** The charts' boxes by machine; each redraws both, so the crosshair
   * stays on one time */
  const chartBoxes = [...$("pl-charts").querySelectorAll(".pl-chart")];
  for (const box of chartBoxes) {
    box.redraw = () => renderTimeline();
    box.addEventListener("pointermove", (event) => {
      const at = box.timeAt?.(event.clientX - box.getBoundingClientRect().left);
      pl.hoverT = at ?? null;
      pl.hoverKey = at == null ? null : box.dataset.key;
      renderTimeline();
    });
    box.addEventListener("pointerleave", () => {
      pl.hoverT = null;
      pl.hoverKey = null;
      renderTimeline();
    });
    sized.observe(box);
  }

  /** The entries' bars over [from, to] of the entries `keep` takes: the
   * spans the lab saw them running, else the queue's own times, packed
   * into at most MAX_LANES lanes */
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

  /** Each machine's chart: its title, its rows (busy, memory, the jobs'
   * memory where known, °C, W, its CPU), its memory's size, and what ran
   * on it when (this host's lanes hold its GPU's and its CPU's entries) */
  function chartSpecs(from, to) {
    return machinesOf(pl.state).map((m) => ({
      key: m.key,
      title: m.title,
      ...m.rows,
      total:
        m.gpu?.mem_total_mib ??
        (m.key === "remote" ? pl.state.remote?.gpu?.mem_total_mib : null),
      ...laneBars(from, to, (e) => machineKey(e) === m.key),
    }));
  }

  function renderTimeline() {
    if (!pl.state) return;
    const to = Date.now();
    const from = to - pl.minutes * 60000;
    const rows = pl.samples.filter((row) => row[ROW.t] >= from);
    const specs = chartSpecs(from, to);
    // Each entry's CPU in cores, from the rows: the strip inside its bar,
    // on one scale for all (at least a core)
    const cores = new Map();
    for (const row of rows)
      for (const [id, value] of Object.entries(row[ROW.cores] ?? {}))
        if (value != null) {
          if (!cores.has(id)) cores.set(id, []);
          cores.get(id).push([row[ROW.t], value]);
        }
    const coreScale = Math.max(
      1,
      ...[...cores.values()].flat().map(([, value]) => value),
    );
    // The same lanes on both charts, so their rows line up
    const lanes = Math.max(1, ...specs.map((s) => s.lanes));
    $("pl-charts").dataset.count = String(specs.length);
    let stripped = false;
    chartBoxes.forEach((box, i) => {
      const spec = specs[i];
      box.hidden = !spec;
      if (spec)
        stripped =
          drawChart(
            box,
            spec,
            specs,
            rows,
            from,
            to,
            cores,
            coreScale,
            lanes,
          ) || stripped;
    });
    // The legend and the note under the charts
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

  /**
   * One GPU's chart in `box`: busy % with its machine's CPU over it,
   * memory (the queue's jobs against the rest where known), the lanes of
   * what ran on it; stretches without a reading shaded, each row's latest
   * value at its end; the crosshair at the time under the pointer in
   * either chart, and the tooltip of both GPUs in the one under it.
   * Whether a lane holds a job's CPU.
   */
  function drawChart(
    box,
    s,
    specs,
    rows,
    from,
    to,
    cores,
    coreScale,
    laneCount,
  ) {
    const width = Math.round(box.clientWidth);
    if (!width) return false;
    const narrow = width < 520;
    // Room on the right for each row's latest value
    const pad = { l: narrow ? 34 : 40, r: 44, t: 20 };
    const busyH = narrow ? 48 : 56;
    const memH = narrow ? 26 : 30;
    const laneH = 13;
    const laneStep = laneH + 3;
    const gapH = 7;
    const axisH = 16;
    const busyTop = pad.t;
    const memTop = busyTop + busyH + gapH;
    const laneTop = memTop + memH + gapH;
    const bottom = laneTop + laneCount * laneStep;
    const height = bottom + axisH;
    const w = width - pad.l - pad.r;
    const x = (time) => pad.l + ((time - from) / (to - from)) * w;
    box.timeAt = (px) =>
      px >= pad.l && px <= pad.l + w
        ? from + ((px - pad.l) / w) * (to - from)
        : null;
    const last = rows[rows.length - 1];
    const root = svg("svg", {
      class: "pl-svg pl-timeline-svg",
      width,
      height,
      viewBox: `0 0 ${width} ${height}`,
      role: "img",
      "aria-label": t("pl.timeline.aria", {
        gpu: s.title,
        hours: num(pl.minutes / 60),
        util: last?.[s.busy] != null ? num(last[s.busy]) : "–",
      }),
    });
    const defs = svg("defs");
    root.append(defs);
    root.append(
      svg("text", { class: "pl-section-title", x: pad.l, y: 8 }, s.title),
    );
    // The time grid: every few minutes, its labels under the lanes
    const spanMin = (to - from) / 60000;
    const stepMin =
      [5, 10, 15, 30, 60, 120, 180].find((m) => (spanMin / m) * 64 <= w) ?? 240;
    const firstTick = Math.ceil(from / (stepMin * 60000)) * stepMin * 60000;
    const gridTimes = [];
    for (let tick = firstTick; tick <= to; tick += stepMin * 60000)
      gridTimes.push(tick);
    for (const tick of gridTimes)
      root.append(
        svg(
          "text",
          { class: "tick tick-x", x: x(tick), y: bottom + 10 },
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
        if (v == null || (prevT != null && row[ROW.t] - prevT > gapMs)) close();
        if (v == null) {
          prevT = row[ROW.t];
          continue;
        }
        const px = x(row[ROW.t]);
        d += `${open ? "L" : "M"}${px.toFixed(1)},${yOf(v).toFixed(1)}`;
        if (!open) startX = px;
        open = true;
        prevT = row[ROW.t];
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
        if (row[ROW.t] - (prev ?? from) > gapMs)
          gaps.push([prev ?? from, row[ROW.t]]);
        prev = row[ROW.t];
      }
      if (prev == null) gaps.push([from, to]);
      else if (to - prev > gapMs) gaps.push([prev, to]);
      return gaps;
    };
    const fresh = last && to - last[ROW.t] < 60000;
    const rowLabel = (text, y) =>
      svg("text", { class: "tick pl-row-label", x: pad.l - 6, y }, text);
    const endLabel = (y, text) =>
      svg("text", { class: "pl-end", x: pad.l + w + 7, y }, text);
    const busyBase = busyTop + busyH;
    const yBusy = (v) => busyBase - (Math.min(v, 100) / 100) * busyH;
    const memTotal =
      s.total ?? Math.max(1, ...rows.map((row) => row[s.mem] ?? 0));
    const yMem = (v) =>
      memTop + memH - (Math.min(v, memTotal) / memTotal) * memH;
    // No reading: shaded, never drawn as 0
    for (const [a, b] of gapsOf(s.busy))
      root.append(
        svg("rect", {
          class: "pl-nodata",
          x: x(a),
          y: busyTop,
          width: Math.max(0, x(b) - x(a)),
          height: memTop + memH - busyTop,
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
        y1: yMem(memTotal),
        y2: yMem(memTotal),
      }),
      svg("line", {
        class: "grid baseline",
        x1: pad.l,
        x2: pad.l + w,
        y1: yMem(0),
        y2: yMem(0),
      }),
      rowLabel(gib(memTotal).replace(" GiB", "G"), yMem(memTotal) + 5),
    );
    for (const tick of gridTimes)
      root.append(
        svg("line", {
          class: "grid pl-vgrid",
          x1: x(tick),
          x2: x(tick),
          y1: busyTop,
          y2: bottom - 3,
        }),
      );
    // Busy, and its machine's CPU over it (the same 0-100% scale)
    root.append(
      svg("path", {
        class: "pl-area s1",
        d: pathOf((row) => row[s.busy], yBusy, busyBase),
      }),
      svg("path", {
        class: "series s1 pl-thin",
        d: pathOf((row) => row[s.busy], yBusy),
      }),
      svg("path", {
        class: "series s3 pl-thin",
        d: pathOf((row) => row[s.cpu], yBusy),
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
          cx: x(last[ROW.t]),
          cy: yBusy(last[s.busy]),
          r: 3.5,
        }),
        endLabel(yBusy(last[s.busy]), `${num(last[s.busy])}%`),
      );
      // The CPU's too, where it does not sit on the busy one
      if (
        last[s.cpu] != null &&
        Math.abs(yBusy(last[s.cpu]) - yBusy(last[s.busy])) >= 12
      )
        root.append(endLabel(yBusy(last[s.cpu]), `${num(last[s.cpu])}%`));
    }
    if (fresh && last[s.mem] != null)
      root.append(
        endLabel(yMem(last[s.mem]) + 1, gib(last[s.mem]).replace(" GiB", "G")),
      );
    // Lanes: what ran when, each bar with its job's CPU inside
    let stripped = false;
    s.bars.forEach((bar, bi) => {
      const bx = x(bar.a);
      const bw = Math.max(3, x(bar.b) - bx);
      const by = laneTop + bar.lane * laneStep;
      const group = svg("g", { class: "pl-lane", "data-state": bar.state });
      group.append(
        svg("rect", { x: bx, y: by, width: bw, height: laneH, rx: 3 }),
      );
      const points = (cores.get(bar.entry.id) ?? []).filter(
        ([time]) => time >= bar.a && time <= bar.b,
      );
      if (points.length) {
        const clip = `pl-lane-clip-${s.key}-${bi}`;
        defs.append(
          svg(
            "clipPath",
            { id: clip },
            svg("rect", { x: bx, y: by, width: bw, height: laneH, rx: 3 }),
          ),
        );
        const yOf = (value) =>
          by + laneH - (Math.min(value, coreScale) / coreScale) * laneH;
        const first = x(points[0][0]);
        let d = `M${first.toFixed(1)},${by + laneH}`;
        for (const [time, value] of points)
          d += `L${x(time).toFixed(1)},${yOf(value).toFixed(1)}`;
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
      if (bw > label.length * 6 + 10)
        group.append(
          svg(
            "text",
            { class: "pl-lane-label", x: bx + 5, y: by + laneH / 2 + 0.5 },
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
            y: laneTop + laneH / 2,
          },
          t("pl.timeline.noLanes"),
        ),
      );
    // The crosshair at the time under the pointer, in both charts; the
    // tooltip in the one under it
    const tip =
      box.querySelector(".pl-tip") ?? el("div", { class: "chart-tip pl-tip" });
    tip.hidden = true;
    const row =
      pl.hoverT != null && rows.length
        ? rows.reduce((best, r) =>
            Math.abs(r[ROW.t] - pl.hoverT) < Math.abs(best[ROW.t] - pl.hoverT)
              ? r
              : best,
          )
        : null;
    if (row && Math.abs(row[ROW.t] - pl.hoverT) < gapMs) {
      const cx = x(row[ROW.t]);
      root.append(
        svg("line", {
          class: "crosshair",
          x1: cx,
          x2: cx,
          y1: busyTop,
          y2: bottom - 3,
        }),
      );
      if (row[s.busy] != null)
        root.append(
          svg("circle", { class: "dot s1", cx, cy: yBusy(row[s.busy]), r: 4 }),
        );
      if (pl.hoverKey === s.key) {
        tip.replaceChildren(
          el("div", { class: "tip-note", text: clock(row[ROW.t], true) }),
          ...specs.flatMap((spec) => tipLines(spec, row)),
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
    return stripped;
  }

  /** A GPU's lines in the timeline's tooltip at a row: busy, its
   * machine's CPU, memory, heat, and each entry running then */
  function tipLines(s, row) {
    const line = (cls, value, label) =>
      el(
        "div",
        { class: `tip-row ${cls}` },
        el("i"),
        el("b", { text: value }),
        ` ${label}`,
      );
    const lines = [el("div", { class: "tip-head", text: s.title })];
    if (row[s.busy] == null) {
      lines.push(
        el("div", { class: "tip-note", text: t("pl.timeline.noReading") }),
      );
    } else {
      lines.push(line("s1", `${num(row[s.busy])}%`, t("pl.timeline.busy")));
      if (row[s.cpu] != null)
        lines.push(
          line(
            "s3",
            `${num(row[s.cpu])}%`,
            s.key === "local" && row[ROW.load] != null
              ? t("pl.timeline.cpuLoad", { load: num(row[ROW.load], 1) })
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
        lines.push(el("div", { class: "tip-note", text: heat.join(" · ") }));
    }
    for (const bar of s.bars) {
      if (bar.a > row[ROW.t] || bar.b < row[ROW.t]) continue;
      const used = row[ROW.cores]?.[bar.entry.id];
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
    return lines;
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
        if (isAbort(error)) return;
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
