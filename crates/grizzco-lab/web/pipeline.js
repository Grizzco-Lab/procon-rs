// Pipeline app: the GPU and the experiment queue, live. The queue is a file
// the agents keep, AgentZero's runs/queue.json (agentzero-queue writes it;
// AgentZero's README has the format): what runs, what waits in the order it
// runs, why each entry runs (the question it answers) and what came out.
// The studio samples the GPU and the machine every 5 s and follows each
// entry's processes and run folder (src/pipeline.rs). The page shows:
//
// - Running: a card per running entry, with its progress and ETA, the loss
//   curve of its run folder, its latest validation scores, its processes
//   and its log; also entries whose processes run although the queue still
//   says queued, and entries the queue says run but whose processes are
//   gone;
// - Machine: the GPU, CPU and memory as tiles with the last half hour, and
//   the GPU's processes with the entries they belong to;
// - GPU timeline: busy %, memory (the queue's jobs against the rest) and
//   what ran when, over the last 1 to 12 hours;
// - Queue: the waiting entries in the order they run, reordered by dragging
//   the handle (or ↑ ↓ on it), written back as priorities (POST order),
//   which agents follow (`agentzero-queue next`);
// - Results and History: what came out, newest first, and entries whose run
//   folder reached its last step before the queue said so.
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

  /**
   * What the page makes of an entry: its place (running, waiting, finished)
   * and its state word, from the queue's status and what the studio saw:
   * processes alive (`live`), and a run folder at its last step
   */
  function view(entry) {
    const alive = Boolean(entry.live?.pids?.length);
    const progress = entry.progress;
    const complete = progress?.total != null && progress.step >= progress.total;
    const status = entry.status;
    if (alive)
      return {
        place: "running",
        state: status === "running" ? "running" : "detected",
      };
    // Said to run, nothing of it runs: over if its steps are all done,
    // else something to look at
    if (status === "running")
      return complete
        ? { place: "finished", state: "finished" }
        : { place: "running", state: "gone" };
    // Waiting, yet its run folder has rows and nothing of it runs: it ran
    // (to the end, or stopped early) before the queue was told
    if ((status === "queued" || status === "paused") && progress)
      return { place: "finished", state: complete ? "finished" : "stopped" };
    if (status === "queued" || status === "paused")
      return { place: "waiting", state: status };
    // Done, failed, or a word of the owner's own: over
    return { place: "finished", state: status ?? "done" };
  }

  /** When an entry ended, as well as the page knows */
  const endedAt = (entry) =>
    entry.ended_ms ??
    entry.seen?.last_ms ??
    entry.progress?.updated_ms ??
    entry.started_ms ??
    null;

  /** How long an entry took, from its start (the file's, else when the
   * studio first saw it) to its end: "≥ 5 min" when the studio only saw it
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

  /** The whole timeline window, averaged down by the studio */
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
  }

  function renderError() {
    const note = $("pl-now-note");
    note.textContent = t("pl.offline", { error: pl.error });
    note.classList.add("level-critical");
  }

  function renderChip(state) {
    const chip = $("pl-chip");
    const running = state.queue.entries.filter((e) => e.live?.pids?.length);
    const gone = state.queue.entries.filter((e) => view(e).state === "gone");
    const parts = [];
    if (state.gpu?.util != null)
      parts.push(t("pl.chip.gpu", { util: num(state.gpu.util) }));
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
    chip.title = [state.gpu?.name, ...running.map((e) => `${e.id}: ${e.title}`)]
      .filter(Boolean)
      .join("\n");
  }

  // ------------------------------------------------------------ running

  /** The running cards, kept by entry so charts and open logs stay */
  function renderRunning(entries, slots) {
    const box = $("pl-running");
    const running = entries.filter((e) => view(e).place === "running");
    running.sort(
      (a, b) =>
        (view(a).state === "gone") - (view(b).state === "gone") ||
        (startedAt(a) ?? 0) - (startedAt(b) ?? 0),
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
        .filter((e) => view(e).place === "waiting" && e.status === "queued")
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
    const meta = [
      entry.owner,
      entry.device ? t(`pl.device.${entry.device}`) : null,
    ]
      .filter(Boolean)
      .join(" · ");
    part.head.replaceChildren(
      stateBadge(
        state,
        state === "detected"
          ? t("pl.state.detectedNote", { status: entry.status })
          : null,
      ),
      groupChip(entry, slots),
      el("code", { class: "pl-id", text: entry.id }),
      meta ? el("span", { class: "pl-meta", text: meta }) : null,
      clockSpan,
    );
    part.title.textContent = entry.title || entry.id;
    part.why.textContent = entry.why;
    part.why.hidden = !entry.why;
    const warning =
      state === "gone"
        ? t("pl.state.goneNote")
        : state === "detected"
          ? t("pl.state.detectedNote", { status: entry.status })
          : state === "finished"
            ? t("pl.state.finishedNote", { status: entry.status })
            : "";
    part.warn.textContent = warning;
    part.warn.hidden = !warning;
    part.warn.dataset.level = state === "gone" ? "warning" : "note";
    fillProgress(part.progress, entry, state);
    part.chart.hidden = !entry.run_dir;
    if (entry.run_dir) part.scores.redraw();
    else part.scores.hidden = true;
    fillStats(part.stats, entry);
    fillLastLine(part.lastLine, entry, state);
    fillMore(part.more, entry);
  }

  /** The log's last line under a live entry without a run folder: often
   * the best word on how far it got */
  function fillLastLine(box, entry, state) {
    const live = state === "running" || state === "detected";
    const wanted = Boolean(live && entry.log && !entry.progress);
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
      el("span", { class: "pl-stat-label", text: t("pl.log.last") }),
      el("code", { text: last ?? "…" }),
    );
  }

  /** The bar, the step and the ETA */
  function fillProgress(box, entry, state) {
    const p = entry.progress;
    const live = state === "running" || state === "detected";
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

  /** Its processes: GPU memory, CPU, RAM */
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
    // Steps still to run
    if (lastStep < total)
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
    // The newest point of the train loss, breathing while it runs
    const train = series[0];
    const [lastX, lastV] = train.points[train.points.length - 1];
    const running =
      view(pl.state?.queue.entries.find((e) => e.id === id) ?? {}).place ===
      "running";
    if (running && lastV >= lo && lastV <= hi)
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
    const keys = SCORES.filter((key) =>
      rows.some((row) => row.values[key] != null),
    ).slice(0, 4);
    box.hidden = !keys.length;
    if (!keys.length) return box.replaceChildren();
    box.replaceChildren(
      ...keys.map((key) => {
        const points = rows
          .filter((row) => row.values[key] != null)
          .map((row) => [row.step, row.values[key]]);
        const [bestStep, best] = points.reduce((a, b) => (b[1] > a[1] ? b : a));
        const last = points[points.length - 1][1];
        return el(
          "div",
          { class: "pl-score" },
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
      }),
    );
  }

  /** A small line of points [[x, y]], its last point marked */
  function sparkline(
    points,
    { cls = "s1", best = null, lo = null, hi = null } = {},
  ) {
    const width = 120;
    const height = 28;
    const root = svg("svg", {
      class: "pl-spark",
      viewBox: `0 0 ${width} ${height}`,
      preserveAspectRatio: "none",
      "aria-hidden": "true",
    });
    const finite = points.filter(([, v]) => v != null && Number.isFinite(v));
    if (finite.length < 2) return root;
    const xs = finite.map(([x]) => x);
    const vs = finite.map(([, v]) => v);
    const x0 = Math.min(...xs);
    const x1 = Math.max(...xs);
    const v0 = lo ?? Math.min(...vs);
    const v1 = hi ?? Math.max(...vs);
    const sx = (v) => 2 + ((v - x0) / (x1 - x0 || 1)) * (width - 6);
    const sy = (v) => 3 + ((v1 - v) / (v1 - v0 || 1)) * (height - 6);
    const d = finite
      .map(
        ([x, v], i) =>
          `${i ? "L" : "M"}${sx(x).toFixed(1)},${sy(v).toFixed(1)}`,
      )
      .join("");
    root.append(
      svg("path", {
        class: `pl-spark-area ${cls}`,
        d: `${d}L${sx(x1).toFixed(1)},${height}L${sx(x0).toFixed(1)},${height}Z`,
      }),
      svg("path", { class: `pl-spark-line ${cls}`, d }),
    );
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
      ? `${gpu.name.replace(/^NVIDIA /, "")}${gpu.pstate ? ` · ${gpu.pstate}` : ""}`
      : "";
    const recent = pl.samples.filter(
      (row) => row[0] >= Date.now() - SPARK_MINUTES * 60000,
    );
    const course = (i) => recent.map((row) => [row[0], row[i]]);
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
          spark: sparkline(course(1), { cls: "s1", lo: 0, hi: 100 }),
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
          spark: sparkline(course(4), { cls: "s2" }),
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
    tiles.push(
      tile(t("pl.tile.cpu"), `${num(cpu.percent)}%`, {
        spark: sparkline(course(6), { cls: "s3", lo: 0, hi: 100 }),
        note: t("pl.tile.cpuNote", {
          cores: cpu.cores,
          load: cpu.load ? num(cpu.load[0], 1) : "–",
        }),
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

  /** The GPU's compute processes: memory, name, entry */
  function renderProcesses(state) {
    const box = $("pl-procs");
    const gpu = state.gpu;
    if (!gpu) {
      box.replaceChildren(
        el("p", {
          class: "panel-note",
          text: t("pl.noGpu", { error: state.gpu_error ?? "" }),
        }),
      );
      return;
    }
    const total = gpu.mem_total_mib || 1;
    const procs = state.gpu_procs ?? [];
    const counted = procs.reduce((sum, p) => sum + (p.gpu_mib ?? 0), 0);
    const rest = Math.max(0, (gpu.mem_used_mib ?? 0) - counted);
    const row = (mib, name, detail, entry, title) =>
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
          el("i", {
            style: `width: ${Math.max(1.5, (mib / total) * 100).toFixed(2)}%`,
          }),
        ),
        el("b", { class: "pl-proc-mem num", text: gib(mib) }),
        el("span", { class: "pl-proc-name", text: name }),
        entry ? el("code", { class: "pl-proc-entry", text: entry }) : null,
        detail
          ? el("span", { class: "pl-proc-detail num", text: detail })
          : null,
      );
    const rows = procs.map((p) =>
      row(p.gpu_mib ?? 0, p.name, `pid ${p.pid}`, p.entry, p.command),
    );
    if (rest > 0)
      rows.push(
        row(rest, t("pl.procs.other"), null, null, t("pl.procs.otherNote")),
      );
    box.replaceChildren(
      el("h3", { class: "pl-sub", text: t("pl.procs.title") }),
      rows.length
        ? el("ul", { class: "pl-proc-list" }, rows)
        : el("p", { class: "panel-note", text: t("pl.procs.none") }),
    );
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

  /** The entries' bars over [from, to]: the spans the studio saw them
   * running, else the queue's own times, packed into lanes */
  function laneBars(from, to) {
    const entries = pl.state?.queue.entries ?? [];
    const bars = [];
    for (const entry of entries) {
      const seen = pl.spans[entry.id];
      const { state, place } = view(entry);
      let spans = seen ? seen.map(([a, b]) => [a, b]) : [];
      const started = entry.started_ms;
      if (spans.length && started != null && started < spans[0][0]) {
        // Begun before the studio saw it
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

  function renderTimeline() {
    const box = timelineBox;
    const width = Math.round(box.clientWidth);
    if (!width || !pl.state) return;
    const gpu = pl.state.gpu;
    const to = Date.now();
    const from = to - pl.minutes * 60000;
    const rows = pl.samples.filter((row) => row[0] >= from);
    const { bars, lanes } = laneBars(from, to);
    const narrow = width < 560;
    // Room on the right for each row's latest value
    const pad = { l: narrow ? 40 : 52, r: 46, t: 8 };
    const busyH = narrow ? 56 : 72;
    const memH = narrow ? 64 : 86;
    const laneH = 18;
    const gapH = 14;
    const lanesH = Math.max(1, lanes) * (laneH + 4);
    const axisH = 22;
    const height = pad.t + busyH + gapH + memH + gapH + lanesH + axisH;
    const w = width - pad.l - pad.r;
    const x = (t) => pad.l + ((t - from) / (to - from)) * w;
    const busyTop = pad.t;
    const memTop = busyTop + busyH + gapH;
    const laneTop = memTop + memH + gapH;
    const memTotal =
      gpu?.mem_total_mib ?? Math.max(1, ...rows.map((row) => row[2] ?? 0));
    const yBusy = (v) => busyTop + busyH - (v / 100) * busyH;
    const yMem = (v) => memTop + memH - (v / memTotal) * memH;
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
    // Before the studio sampled: nothing to show but the queue's lanes
    const since = pl.samplingSince;
    if (since != null && since > from)
      root.append(
        svg("rect", {
          class: "pl-nodata",
          x: pad.l,
          y: busyTop,
          width: Math.max(0, x(since) - pad.l),
          height: memTop + memH - busyTop,
        }),
      );
    // Row labels and grids
    const rowLabel = (text, y) =>
      svg("text", { class: "tick pl-row-label", x: pad.l - 6, y }, text);
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
      rowLabel(gib(memTotal).replace(" GiB", "G"), yMem(memTotal) + 4),
      rowLabel("0", yMem(0) - 4),
    );
    // Time axis: its grid under the rows, its labels under the lanes
    const spanMin = (to - from) / 60000;
    const stepMin =
      [5, 10, 15, 30, 60, 120, 180].find((m) => (spanMin / m) * 70 <= w) ?? 240;
    const axisY = laneTop + lanesH + 14;
    const firstTick = Math.ceil(from / (stepMin * 60000)) * stepMin * 60000;
    for (let tick = firstTick; tick <= to; tick += stepMin * 60000) {
      const tx = x(tick);
      root.append(
        svg("line", {
          class: "grid pl-vgrid",
          x1: tx,
          x2: tx,
          y1: busyTop,
          y2: memTop + memH,
        }),
        svg("text", { class: "tick tick-x", x: tx, y: axisY }, clock(tick)),
      );
    }
    // A series as a path, broken where samples are missing or far apart
    const gapMs = Math.max(20000, ((to - from) / 720) * 3);
    const pathOf = (index, yOf, base = null) => {
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
        const v = typeof index === "function" ? index(row) : row[index];
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
    root.append(
      svg("path", { class: "pl-area s1", d: pathOf(1, yBusy, yBusy(0)) }),
      svg("path", { class: "series s1 pl-thin", d: pathOf(1, yBusy) }),
      // Memory: all of it, then the queue's jobs over it
      svg("path", { class: "pl-mem-other", d: pathOf(2, yMem, yMem(0)) }),
      svg("path", {
        class: "pl-mem-jobs",
        d: pathOf(
          (row) => (row[3] == null ? null : Math.min(row[3], row[2] ?? row[3])),
          yMem,
          yMem(0),
        ),
      }),
      svg("path", { class: "pl-mem-line", d: pathOf(2, yMem) }),
    );
    // The newest sample, breathing, and each row's value at its end
    const last = rows[rows.length - 1];
    if (last && last[1] != null && to - last[0] < 30000)
      root.append(
        svg("circle", {
          class: "pl-now s1",
          cx: x(last[0]),
          cy: yBusy(last[1]),
          r: 4,
        }),
      );
    if (last && to - last[0] < 60000) {
      const endLabel = (y, text) =>
        svg("text", { class: "pl-end", x: pad.l + w + 8, y }, text);
      if (last[1] != null)
        root.append(endLabel(yBusy(last[1]), `${num(last[1])}%`));
      if (last[2] != null)
        root.append(endLabel(yMem(last[2]), gib(last[2]).replace(" GiB", "G")));
    }
    // Lanes: what ran when
    for (const bar of bars) {
      const bx = x(bar.a);
      const bw = Math.max(3, x(bar.b) - bx);
      const by = laneTop + bar.lane * (laneH + 4);
      const group = svg("g", { class: "pl-lane", "data-state": bar.state });
      group.append(
        svg("rect", { x: bx, y: by, width: bw, height: laneH, rx: 4 }),
      );
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
    }
    if (!bars.length)
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
    // Crosshair: the sample nearest the pointer, and what ran then
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
            y1: busyTop,
            y2: laneTop + lanesH,
          }),
        );
        if (row[1] != null)
          root.append(
            svg("circle", { class: "dot s1", cx, cy: yBusy(row[1]), r: 4 }),
          );
        const ran = bars.filter((bar) => bar.a <= row[0] && bar.b >= row[0]);
        const line = (cls, value, label) =>
          el(
            "div",
            { class: `tip-row ${cls}` },
            el("i"),
            el("b", { text: value }),
            ` ${label}`,
          );
        tip.replaceChildren(
          line(
            "s1",
            row[1] == null ? "–" : `${num(row[1])}%`,
            t("pl.timeline.busy"),
          ),
          line(
            "pl-k-jobs",
            row[3] == null ? "–" : gib(row[3]),
            t("pl.timeline.jobs"),
          ),
          line(
            "pl-k-other",
            row[2] == null ? "–" : gib(Math.max(0, row[2] - (row[3] ?? 0))),
            t("pl.timeline.other"),
          ),
          el("div", {
            class: "tip-note",
            text: [
              row[4] != null ? `${num(row[4])} °C` : null,
              row[5] != null ? `${num(row[5])} W` : null,
              row[6] != null ? `CPU ${num(row[6])}%` : null,
            ]
              .filter(Boolean)
              .join(" · "),
          }),
          ...ran.map((bar) =>
            el("div", {
              class: "tip-note pl-tip-entry",
              text: `▸ ${bar.entry.id}`,
            }),
          ),
          el("div", { class: "tip-note", text: clock(row[0], true) }),
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
      el("span", { class: "key pl-k-jobs" }, el("i"), t("pl.timeline.jobs")),
      el("span", { class: "key pl-k-other" }, el("i"), t("pl.timeline.other")),
    );
    const note = $("pl-timeline-note");
    note.textContent =
      since != null
        ? t("pl.timeline.since", {
            s: num((pl.state.sample_ms ?? 5000) / 1000),
            clock: clock(since, true),
          })
        : t("pl.timeline.empty");
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
    const content = list.map((e) => [
      e.id,
      e.status,
      e.title,
      e.why,
      e.group,
      e.owner,
      e.device,
      e.eta_ms,
      slots.get(e.group),
    ]);
    if (!changed(box, content)) return;
    if (!list.length) {
      box.replaceChildren(
        el("li", { class: "pl-empty", text: t("pl.queue.empty") }),
      );
      return;
    }
    const nextId = list.find((e) => e.status === "queued")?.id;
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
              ? el("span", { text: t(`pl.device.${entry.device}`) })
              : null,
            entry.owner ? el("span", { text: entry.owner }) : null,
            entry.eta_ms
              ? el("span", {
                  text: t("pl.progress.eta", { clock: clock(entry.eta_ms) }),
                })
              : null,
          ),
          entry.why && !sameWhy
            ? el("p", { class: "pl-why", text: entry.why })
            : null,
        ),
        entry.status === "paused"
          ? stateBadge("paused")
          : entry.id === nextId
            ? el("span", { class: "pl-next", text: t("pl.queue.next") })
            : null,
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
                (entry.progress && (state === "finished" || state === "stopped")
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
