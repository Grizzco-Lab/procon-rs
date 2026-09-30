// The video player shared by the apps, made from the Inkspector's: a picture
// (exact frames decoded by the lab, or a <video>), the transport with its
// keys, a scrubber with marks, the neighbours strip, the input overlays
// (Full, Minimal, None) with the truth and a prediction, and the labels
// table. Runs after i18n.js and app.js and uses their helpers (t, $, svgEl,
// stickPercent, drawInputHud); inspect.js, cuttlefish.js, vision.js and
// predictor.js each make a Player in their own panel.
//
//   const player = new Player({
//     screen,      // the .screen element: the picture goes in, apps put layers over it
//     controls,    // takes the transport: play, frame steps, speed, sound, overlay, go to, position
//     scrubber,    // the .scrubber element
//     strip,       // the neighbours strip (optional), stripNote its note
//     table,       // tbody of the labels table (optional), predNote its note
//     compact,     // a narrow labels table: no Valid column, values apart by a space
//     remember,    // localStorage prefix of the choices (overlay, sound, spacing)
//     neighbours,  // { radius, seconds }: thumbnails on each side, every frame or every `seconds`
//     onFrame(n), onSeek(n), onPlay(playing), onMark(tick), onError(message),
//   });
//   player.open(source, at)
//
// A source is { frames, fps, frame(n) } for exact frames (the Inkspector's
// endpoint) or { video, fps } for a <video>, plus thumb(n) for the strip
// (frame(n) by default), audio (a URL, exact frames only), sound, labels(n)
// (a promise of [truth, prediction]) and title (of the full overlay). Frames
// and thumbnails load through the page's request queue (`imageUrl` in
// app.js), after the app's data, and stop when they are no longer wanted.
"use strict";

/** Stick difference (raw 12-bit units) that counts as a mismatch */
const STICK_TOLERANCE = 256;
/** Gyro difference (degrees over the frame) that counts as a mismatch */
const GYRO_TOLERANCE = 0.5;
/** Frames requested ahead of the current one while playing exact frames */
const PREFETCH = 45;
/** Wait after the last scrubber move before seeking, in ms */
const SCRUB_DEBOUNCE_MS = 120;
/** Wait after the last seek before a spaced strip is drawn again, in ms */
const STRIP_DEBOUNCE_MS = 150;
/** A click this close (in CSS pixels) to a mark goes to its frame */
const MARK_SNAP_PX = 6;
/** Marks shown under a strip thumbnail at most */
const STRIP_MARKS = 3;
/** Angular rate that fills a gyro bar of the full overlay, in °/s */
const FULL_GYRO_DPS = 300;
/** Playback speeds offered */
const SPEEDS = [0.25, 0.5, 1, 1.5, 2];
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

function escapeHtml(text) {
  return String(text).replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c],
  );
}

/** Seconds as "1:02.500" */
function clock(seconds) {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds - 60 * minutes).toFixed(3).padStart(6, "0")}`;
}

/** A label with reports (a prediction has none to lack) */
const validLabel = (label) => Boolean(label) && label.valid !== false;

function stickText(v) {
  return v ? `${v[0].toFixed(0)}, ${v[1].toFixed(0)}` : "";
}

function gyroText(v) {
  return v ? v.map((x) => x.toFixed(2)).join(", ") : "";
}

/** A label in short, for the strip captions */
function summaryText(label) {
  if (!validLabel(label)) return "no reports";
  const buttons = (label.buttons ?? []).join(" ") || "–";
  return `${buttons}<br>L ${stickText(label.left_stick)}<br>R ${stickText(label.right_stick)}<br>g ${gyroText(label.gyro_deg)}`;
}

/** Whether truth and prediction differ in one column */
function differs(key, truth, pred) {
  if (!pred || !truth || !truth.valid) return false;
  const a = truth[key];
  const b = pred[key];
  if (b == null) return false;
  if (key === "buttons") return [...a].sort().join() !== [...b].sort().join();
  if (key === "valid") return a !== b;
  const tolerance = key === "gyro_deg" ? GYRO_TOLERANCE : STICK_TOLERANCE;
  return a.some((x, i) => Math.abs(x - b[i]) > tolerance);
}

/**
 * Build the full overlay (the style of AgentZero's agentzero-overlay): a
 * title band on top, and below the frame both sticks, a grid of every
 * button and bars of the yaw and pitch turned over the frame. A prediction
 * next to the truth marks each key's bottom strip, a ring on each stick and
 * a thin bar under the truth's; keys where the two differ get a red edge.
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
    text({
      class: "full-legend",
      x: 632,
      y: 16,
      "text-anchor": "end",
      "data-full": "legend",
    }),
    svgEl("rect", { class: "full-band", y: 250, width: 640, height: 110 }),
  );
  for (const [side, cx] of [
    ["l", 50],
    ["r", 590],
  ]) {
    svg.append(
      svgEl("circle", { class: "full-ring", cx, cy: 305, r: 40 }),
      svgEl("circle", {
        class: "full-dot-pred",
        cx,
        cy: 305,
        r: 6,
        "data-full": `stick-${side}-pred`,
      }),
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
      svgEl("rect", {
        class: "full-key-pred",
        x: x + 1,
        y: y + 18,
        width: box - 2,
        height: 4,
      }),
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
      svgEl("rect", {
        class: "full-bar-pred",
        x: 320,
        y: y + 8,
        width: 0,
        height: 4,
        "data-full": `${name}-pred`,
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

class Player {
  constructor(options) {
    this.options = options;
    this.screen = options.screen;
    this.controls = options.controls;
    this.scrubber = options.scrubber;
    this.strip = options.strip ?? null;
    this.stripNote = options.stripNote ?? null;
    this.table = options.table ?? null;
    this.predNote = options.predNote ?? null;
    /** A narrow labels table: no Valid column, a frame without reports marked on its row */
    this.compact = Boolean(options.compact);
    this.prefix = `procon-${options.remember ?? "player"}-`;
    const neighbours = options.neighbours ?? {};
    /** Thumbnails on each side of the current frame */
    this.radius = neighbours.radius ?? 3;
    /** Seconds between thumbnails, or null for every frame */
    this.spacing = neighbours.seconds
      ? parseFloat(this.remembered("spacing", neighbours.seconds)) ||
        neighbours.seconds
      : null;
    this.source = null;
    /** Sources opened so far, telling one from the next in the strip's key */
    this.opened = 0;
    this.frame = 0;
    this.frames = 0;
    this.fps = 30;
    this.playing = false;
    /** Whether the keys act: the app sets it while it is shown */
    this.enabled = false;
    /** Overlay style: full, minimal (the Studio's) or none */
    this.overlay = this.remembered("overlay", "full");
    /** Play the sound: an exact-frame source's audio sets the frame then */
    this.sound = this.remembered("sound", "true") === "true";
    /** Loaded frame images by frame number (exact frames), and what stops
     * their loads when the source changes */
    this.images = new Map();
    this.loads = new AbortController();
    /** Marks on the scrubber: ticks {n, kind, short?, id?, title?} and ranges {a, b, kind} */
    this.marks = { ticks: [], ranges: [] };
    this.stripKey = "";
    this.stripTimer = null;
    this.build();
  }

  remembered(key, fallback) {
    try {
      return localStorage.getItem(this.prefix + key) ?? fallback;
    } catch {
      return fallback;
    }
  }

  remember(key, value) {
    try {
      localStorage.setItem(this.prefix + key, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  // ------------------------------------------------------------ building

  /** The picture, overlays, transport and scrubber, once */
  build() {
    const el = (tag, attrs = {}) => {
      const node = document.createElement(tag);
      for (const [key, value] of Object.entries(attrs)) {
        if (value === true) node.setAttribute(key, "");
        else if (value !== false) node.setAttribute(key, value);
      }
      return node;
    };
    this.canvas = el("canvas", {
      class: "player-frame",
      width: 640,
      height: 360,
      hidden: true,
    });
    this.context = this.canvas.getContext("2d");
    this.video = el("video", {
      class: "player-video",
      preload: "auto",
      playsinline: true,
      hidden: true,
    });
    this.audio = el("audio", { preload: "auto" });
    // The Studio's input overlay, copied over the picture
    this.hud = $("input-hud").cloneNode(true);
    this.hud.removeAttribute("id");
    this.hud.dataset.keys = "";
    this.full = buildFullOverlay();
    this.full.toggleAttribute("hidden", true);
    this.noReports = el("span", { class: "no-reports", hidden: true });
    // Layers the app adds later come after, so over these
    this.screen.prepend(
      this.canvas,
      this.video,
      this.audio,
      this.hud,
      this.full,
      this.noReports,
    );

    const controls = this.controls;
    controls.innerHTML = `
      <button type="button" class="btn btn-play" data-player="play" title="Space"></button>
      <button type="button" class="btn" data-player="prev" title="← (Shift: 10)">‹</button>
      <button type="button" class="btn" data-player="next" title="→ (Shift: 10)">›</button>
      <select class="select" data-player="speed"></select>
      <button type="button" class="mode-toggle" data-player="sound" hidden></button>
      <select class="select" data-player="overlay">
        <option value="full"></option>
        <option value="minimal"></option>
        <option value="none"></option>
      </select>
      <button type="button" class="btn" data-player="goto" title="G"></button>
      <span class="chip num" data-player="position">–</span>`;
    this.part = (name) => controls.querySelector(`[data-player="${name}"]`);
    const speed = this.part("speed");
    speed.replaceChildren(
      ...SPEEDS.map((s) => new Option(`${s}×`, String(s), s === 1, s === 1)),
    );
    speed.addEventListener("change", () => {
      this.video.playbackRate = this.speed();
      this.audio.playbackRate = this.speed();
    });
    this.part("play").onclick = () => this.toggle();
    this.part("prev").onclick = (event) =>
      this.go(this.frame - (event.shiftKey ? 10 : 1));
    this.part("next").onclick = (event) =>
      this.go(this.frame + (event.shiftKey ? 10 : 1));
    this.part("goto").onclick = () => this.goTo();
    this.part("overlay").value = this.overlay;
    this.part("overlay").addEventListener("change", (event) => {
      this.overlay = event.target.value;
      this.remember("overlay", this.overlay);
      this.refresh();
    });
    this.part("sound").addEventListener("click", () => {
      this.sound = !this.sound;
      this.remember("sound", String(this.sound));
      this.markSound();
      this.video.muted = !this.sound;
      // Exact frames: restart playback on the other clock
      if (this.playing && this.source?.frame) {
        this.pause();
        this.play();
      }
    });
    this.texts();
    window.addEventListener("lang-change", () => this.texts());

    this.scrubber.innerHTML = `
      <div class="scrubber-track">
        <div class="scrubber-fill"></div>
        <canvas class="scrubber-marks"></canvas>
      </div>
      <div class="scrubber-thumb"></div>
      <div class="scrubber-bubble" hidden></div>`;
    this.fill = this.scrubber.querySelector(".scrubber-fill");
    this.thumb = this.scrubber.querySelector(".scrubber-thumb");
    this.bubble = this.scrubber.querySelector(".scrubber-bubble");
    this.marksCanvas = this.scrubber.querySelector(".scrubber-marks");
    this.buildScrubber();
    new ResizeObserver(() => this.drawMarks()).observe(this.scrubber);

    if (this.strip) {
      this.strip.addEventListener("click", (event) => {
        const mark = event.target.closest("[data-mark]");
        if (mark) {
          const tick = this.marks.ticks[Number(mark.dataset.mark)];
          if (tick) this.goMark(tick);
          return;
        }
        const figure = event.target.closest("figure");
        if (!figure || figure.classList.contains("is-empty")) return;
        this.go(Number(figure.dataset.n));
      });
    }

    const video = this.video;
    video.addEventListener("loadedmetadata", () => {
      if (!this.source || this.source.frame) return;
      this.frames = Math.max(
        1,
        Math.floor((video.duration || 0) * this.fps + 1e-6),
      );
      this.drawMarks();
      this.show(this.wanted ?? 0);
      this.wanted = null;
    });
    video.addEventListener("play", () => this.followVideo());
    video.addEventListener("pause", () => this.settle());
    video.addEventListener("ended", () => this.settle());
    video.addEventListener("seeked", () => {
      if (this.playing || !this.source || this.source.frame) return;
      // Landed elsewhere (past the end, say): show where it is
      if (
        Math.abs(video.currentTime - this.timeOf(this.frame) - 0.5 / this.fps) >
        1 / this.fps
      )
        this.show(this.frameOf(video.currentTime));
    });
    video.addEventListener("timeupdate", () => {
      if (this.playing && !video.requestVideoFrameCallback)
        this.show(this.frameOf(video.currentTime));
    });
    video.addEventListener("error", () => {
      if (this.source && !this.source.frame)
        this.options.onError?.(t("player.cannotPlay"));
    });

    // Keys act while the app shows the player and something is open
    document.addEventListener("keydown", (event) => {
      if (!this.enabled || !this.source) return;
      const tag = event.target.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      if (event.target.isContentEditable) return;
      if (event.ctrlKey || event.metaKey || event.altKey) return;
      const step = event.shiftKey ? 10 : 1;
      if (event.key === " ") this.toggle();
      else if (event.key === "ArrowLeft") this.go(this.frame - step);
      else if (event.key === "ArrowRight") this.go(this.frame + step);
      else if (event.key === "Home") this.go(0);
      else if (event.key === "End") this.go(this.frames - 1);
      else if (event.key.toLowerCase() === "g") this.goTo();
      else return;
      event.preventDefault();
    });
  }

  /** Every text the player draws itself, in the page's language */
  texts() {
    this.markPlay();
    this.part("prev").textContent = t("player.frameBack");
    this.part("next").textContent = t("player.frameNext");
    this.part("speed").setAttribute("aria-label", t("player.speed"));
    this.part("sound").textContent = t("player.sound");
    this.part("sound").title = t("player.soundTitle");
    this.part("goto").textContent = t("player.goto");
    const overlay = this.part("overlay");
    overlay.setAttribute("aria-label", t("player.overlay"));
    for (const option of overlay.options)
      option.textContent = t(`player.overlay.${option.value}`);
    this.noReports.textContent = t("player.noReports");
    this.drawStripNote();
  }

  speed() {
    return parseFloat(this.part("speed").value) || 1;
  }

  // ------------------------------------------------------------- sources

  /** Show a source at frame `at`; the previous one is forgotten */
  open(source, at = 0) {
    this.pause();
    this.source = source;
    this.opened += 1;
    this.forgetImages();
    this.marks = { ticks: [], ranges: [] };
    this.stripKey = "";
    this.fps = source.fps || 30;
    this.frame = 0;
    this.wanted = null;
    this.audio.removeAttribute("src");
    delete this.audio.dataset.source;
    const exact = Boolean(source.frame);
    this.canvas.hidden = !exact;
    this.video.hidden = exact;
    this.part("overlay").hidden = !source.labels;
    this.markSound();
    if (exact) {
      this.frames = source.frames;
      if (this.video.getAttribute("src")) {
        this.video.removeAttribute("src");
        this.video.load();
      }
      this.drawMarks();
      this.show(at);
      return;
    }
    // A <video>: its length comes with the metadata, the frame then
    this.frames = 0;
    this.wanted = at;
    this.video.muted = !this.sound;
    this.video.playbackRate = this.speed();
    this.video.src = source.video;
    this.drawMarks();
    this.drawPosition();
  }

  /** Stop and forget the source */
  close() {
    this.pause();
    this.source = null;
    this.frames = 0;
    this.frame = 0;
    this.forgetImages();
    this.marks = { ticks: [], ranges: [] };
    this.canvas.hidden = true;
    this.video.hidden = true;
    if (this.video.getAttribute("src")) {
      this.video.removeAttribute("src");
      this.video.load();
    }
    this.audio.removeAttribute("src");
    delete this.audio.dataset.source;
    this.hud.toggleAttribute("hidden", true);
    this.full.toggleAttribute("hidden", true);
    this.noReports.hidden = true;
    this.part("sound").hidden = true;
    this.drawPosition();
    this.drawScrubber(0);
    this.drawMarks();
    this.stripKey = "";
    this.strip?.replaceChildren();
    this.table?.replaceChildren();
  }

  /** The frame rate changed (a video's came with its metadata): same time */
  setFps(fps) {
    if (!fps || fps === this.fps) return;
    const time = this.time();
    this.fps = fps;
    if (this.source && !this.source.frame) {
      this.frames = Math.max(
        1,
        Math.floor((this.video.duration || 0) * fps + 1e-6),
      );
    }
    this.frame = this.frameOf(time);
    this.stripKey = "";
    this.drawMarks();
    this.drawStripNote();
    this.refresh();
  }

  /** Seconds between the strip's thumbnails */
  setSpacing(seconds) {
    this.spacing = seconds;
    this.remember("spacing", String(seconds));
    this.stripKey = "";
    this.drawStripNote();
    this.scheduleStrip();
  }

  /** A message in the position chip: loading, or what went wrong */
  status(text) {
    this.part("position").textContent = text;
  }

  clamp(n) {
    return Math.max(0, Math.min(Math.max(0, this.frames - 1), n));
  }

  /** The frame on screen at `seconds` into the video */
  frameOf(seconds) {
    return this.clamp(Math.floor(seconds * this.fps + 1e-3));
  }

  timeOf(n) {
    return n / this.fps;
  }

  /** Where the player is, in seconds */
  time() {
    return this.timeOf(this.frame);
  }

  // -------------------------------------------------------------- frames

  /**
   * The frame's image, loading it once (exact frames) through the page's
   * request queue (`imageUrl` in app.js): its turn after the app's data,
   * stopped when the app is left, the source changes or the frame is
   * forgotten; one that did not load is asked for again next time
   */
  image(n) {
    const { images, source, loads } = this;
    if (!images.has(n)) {
      const img = new Image();
      img.stop = new AbortController();
      const signal = AbortSignal.any
        ? AbortSignal.any([loads.signal, img.stop.signal])
        : loads.signal;
      img.loaded = imageUrl(source.frame(n), signal).then(
        (url) => {
          img.src = url;
          return img.decode().then(
            () => true,
            () => false,
          );
        },
        () => {
          if (images.get(n) === img) images.delete(n);
          return false;
        },
      );
      images.set(n, img);
    }
    return images.get(n);
  }

  /** A frame's image no longer kept: its load stopped, its bytes let go */
  forget(img) {
    img.stop?.abort();
    if (img.src.startsWith("blob:")) URL.revokeObjectURL(img.src);
  }

  /** Every frame and strip picture of the source let go, their loads
   * stopped: another source comes, or none */
  forgetImages() {
    this.loads?.abort();
    this.loads = new AbortController();
    for (const img of this.images?.values() ?? []) this.forget(img);
    this.images = new Map();
    this.forgetStrip();
  }

  /** Request the next frames and forget those far away */
  prefetch(n) {
    const { images, frames, radius } = this;
    for (let k = n; k < Math.min(frames, n + PREFETCH); k++) this.image(k);
    for (const [k, img] of images) {
      if (k < n - 2 * radius || k > n + 2 * PREFETCH) {
        this.forget(img);
        images.delete(k);
      }
    }
  }

  /** Jump to frame n, pausing playback: the app hears a seek */
  go(n) {
    if (!this.source) return;
    this.pause();
    n = this.clamp(n);
    this.options.onSeek?.(n);
    this.show(n);
  }

  /** Jump to a time in seconds */
  seek(seconds) {
    this.go(Math.round(seconds * this.fps));
  }

  /** Show the frame again: the labels or the overlay changed */
  refresh() {
    if (this.source) this.show(this.frame);
  }

  /** Show frame n: picture, labels, position; the strip only if paused */
  async show(n) {
    const source = this.source;
    if (!source) return;
    n = this.clamp(n);
    this.frame = n;
    this.drawPosition();
    this.drawScrubber(n);
    if (source.frame) this.prefetch(n);
    else if (!this.playing && this.video.readyState >= 1) {
      // The middle of the frame lands on it whatever the rounding
      const at = (n + 0.5) / this.fps;
      if (Math.abs(this.video.currentTime - at) > 0.5 / this.fps)
        this.video.currentTime = at;
    }
    if (!this.playing) this.scheduleStrip();
    this.options.onFrame?.(n);

    const still = () => n === this.frame && source === this.source;
    if (source.frame) {
      const img = this.image(n);
      img.loaded.then((ok) => {
        if (ok && still())
          this.context.drawImage(
            img,
            0,
            0,
            this.canvas.width,
            this.canvas.height,
          );
      });
    }
    if (!source.labels) {
      this.drawLabel(undefined, undefined);
      return;
    }
    let rows;
    try {
      const around = [];
      for (let k = n - this.radius; k <= n + this.radius; k++) {
        if (k >= 0 && k < this.frames) around.push(k);
      }
      rows = await Promise.all(
        around.map(async (k) => {
          const [truth, pred] = (await source.labels(k)) ?? [];
          return { n: k, truth, pred };
        }),
      );
    } catch (error) {
      if (this.table && still()) {
        this.table.innerHTML = `<tr><td colspan="6" class="level-critical">${escapeHtml(error.message)}</td></tr>`;
      }
      return;
    }
    if (!still()) return;
    this.drawTable(rows);
    this.drawCaptions(rows);
    const current = rows.find((row) => row.n === n);
    this.drawLabel(current?.truth, current?.pred);
  }

  drawPosition() {
    const chip = this.part("position");
    if (!this.source || !this.frames) return (chip.textContent = "–");
    chip.textContent = `${t("player.frame")} ${this.frame} / ${this.frames - 1} · ${clock(this.time())}`;
  }

  // ------------------------------------------------------------ overlays

  /** Draw a frame's labels over it in the chosen overlay style */
  drawLabel(truth, pred) {
    const { hud, full, overlay } = this;
    // The minimal overlay shows the truth; a video without a recording, its prediction
    const shown = truth !== undefined ? truth : pred;
    hud.toggleAttribute("hidden", overlay !== "minimal" || !validLabel(shown));
    full.toggleAttribute("hidden", overlay !== "full" || !this.source?.labels);
    this.noReports.hidden =
      overlay === "full" || truth === undefined || validLabel(truth);
    if (overlay === "full") return this.drawFull(truth, pred);
    if (overlay !== "minimal" || !validLabel(shown)) return;
    drawInputHud(hud, {
      left: (shown.left_stick ?? [2048, 2048]).map(stickPercent),
      right: (shown.right_stick ?? [2048, 2048]).map(stickPercent),
      pressed: new Set(shown.buttons ?? []),
      yaw: (shown.gyro_deg?.[2] ?? 0) * this.fps,
      pitch: (shown.gyro_deg?.[1] ?? 0) * this.fps,
    });
  }

  /**
   * Draw a frame's labels on the full overlay: the truth, with the
   * prediction against it when there is one; a video without a recording
   * shows its prediction alone
   */
  drawFull(truth, pred) {
    const { full, source, frame, fps } = this;
    if (!source?.labels) return;
    const part = (name) => full.querySelector(`[data-full="${name}"]`);
    const main = truth !== undefined ? truth : pred;
    const compare = truth !== undefined && pred ? pred : null;
    const predOnly = truth === undefined && Boolean(pred);
    full.classList.toggle("is-pred", predOnly);
    full.classList.toggle("has-pred", Boolean(compare));
    part("title").textContent =
      `${source.title ?? ""}  ${t("player.frame")} ${frame}  ${(frame / fps).toFixed(3)} s`;
    part("alert").textContent = main
      ? validLabel(main)
        ? ""
        : "NO REPORTS"
      : "NO LABEL";
    part("legend").textContent =
      compare || predOnly ? t("player.prediction") : "";
    const pressed = new Set(validLabel(main) ? (main.buttons ?? []) : []);
    const predicted = compare ? new Set(compare.buttons ?? []) : null;
    for (const key of full.querySelectorAll("[data-full-key]")) {
      const name = key.dataset.fullKey;
      key.classList.toggle("on", pressed.has(name));
      key.classList.toggle("pred", Boolean(predicted?.has(name)));
      key.classList.toggle(
        "miss",
        Boolean(predicted) && predicted.has(name) !== pressed.has(name),
      );
    }
    for (const [side, key] of [
      ["l", "left_stick"],
      ["r", "right_stick"],
    ]) {
      const cx = side === "l" ? 50 : 590;
      for (const [label, suffix] of [
        [validLabel(main) ? main : null, ""],
        [compare, "-pred"],
      ]) {
        const [x, y] = label?.[key] ? label[key].map(stickPercent) : [0, 0];
        const dot = part(`stick-${side}${suffix}`);
        dot.setAttribute("cx", (cx + (x / 100) * 40).toFixed(1));
        dot.setAttribute("cy", (305 - (y / 100) * 40).toFixed(1));
      }
    }
    // Rotation over the frame; a full bar is FULL_GYRO_DPS
    const fullDeg = FULL_GYRO_DPS / fps;
    const signed = (degrees) =>
      `${degrees < 0 ? "−" : "+"}${Math.abs(degrees).toFixed(2)}°`;
    for (const [name, axis] of [
      ["yaw", 2],
      ["pitch", 1],
    ]) {
      const degrees = validLabel(main) ? (main.gyro_deg?.[axis] ?? 0) : 0;
      const predDeg = compare ? (compare.gyro_deg?.[axis] ?? 0) : 0;
      for (const [value, suffix] of [
        [degrees, ""],
        [predDeg, "-pred"],
      ]) {
        const end = 320 + Math.max(-1, Math.min(1, value / fullDeg)) * 220;
        const bar = part(name + suffix);
        bar.setAttribute("x", Math.min(320, end).toFixed(1));
        bar.setAttribute("width", Math.abs(end - 320).toFixed(1));
      }
      const value = part(`${name}-text`);
      value.textContent = `${value.dataset.label} ${signed(degrees)}${compare ? ` / ${signed(predDeg)}` : ""}`;
    }
  }

  // --------------------------------------------------------------- table

  cell(key, truth, pred, format) {
    const td = document.createElement("td");
    if (differs(key, truth, pred)) td.className = "mismatch";
    td.innerHTML = truth ? format(truth[key]) : "–";
    if (pred !== undefined) {
      const value = pred && pred[key] != null ? format(pred[key]) : "–";
      td.innerHTML += `<div class="pred">${value}</div>`;
    }
    return td;
  }

  /** The frames around the current one, truth over prediction */
  drawTable(rows) {
    if (!this.table) return;
    const body = this.table;
    // Compact: values apart by a space alone
    const tight = (text) => (v) => text(v).replaceAll(", ", " ");
    const stick = this.compact ? tight(stickText) : stickText;
    const gyro = this.compact ? tight(gyroText) : gyroText;
    body.replaceChildren();
    for (const { n, truth, pred } of rows) {
      const tr = document.createElement("tr");
      if (n === this.frame) tr.className = "current";
      tr.onclick = () => this.go(n);
      const number = document.createElement("td");
      number.textContent = n;
      tr.append(number);
      if (!this.compact)
        tr.append(
          this.cell("valid", truth, pred, (v) =>
            v == null ? "" : v ? "yes" : "no",
          ),
        );
      else if (truth && !validLabel(truth)) {
        tr.classList.add("no-reports");
        tr.title = t("player.noReports");
      }
      tr.append(
        this.cell("buttons", truth, pred, (v) => (v ? v.join(" ") || "–" : "")),
        this.cell("left_stick", truth, pred, stick),
        this.cell("right_stick", truth, pred, stick),
        this.cell("gyro_deg", truth, pred, gyro),
      );
      body.append(tr);
    }
    if (this.predNote)
      this.predNote.hidden = !rows.some((row) => row.pred !== undefined);
  }

  // --------------------------------------------------------------- strip

  /** Draw the strip now (every frame) or once seeking settles (spaced) */
  scheduleStrip() {
    clearTimeout(this.stripTimer);
    if (!this.strip) return;
    if (this.spacing)
      this.stripTimer = setTimeout(() => this.drawStrip(), STRIP_DEBOUNCE_MS);
    else this.drawStrip();
  }

  /** Frames between thumbnails */
  stripStep() {
    return this.spacing ? Math.max(1, Math.round(this.spacing * this.fps)) : 1;
  }

  /**
   * Thumbnails around the current frame; spaced ones on a grid, so a
   * nearby pause asks for the same (cached) pictures
   */
  drawStrip() {
    const { strip, source, frames, radius } = this;
    if (!strip || !source || !frames || this.playing) return;
    const step = this.stripStep();
    const center = step > 1 ? Math.round(this.frame / step) * step : this.frame;
    const { ticks } = this.marks;
    const key = [
      this.opened,
      center,
      step,
      frames,
      ...ticks.map((tick) => `${tick.n}:${tick.kind}`),
    ].join("|");
    if (key === this.stripKey) return this.markStrip();
    this.stripKey = key;
    this.forgetStrip();
    const thumb = source.thumb ?? source.frame;
    const { signal } = this.stripLoads;
    const figures = [];
    for (let k = -radius; k <= radius; k++) {
      const n = center + k * step;
      const figure = document.createElement("figure");
      figure.dataset.n = n;
      if (n < 0 || n >= frames) {
        figure.className = "is-empty";
        figures.push(figure);
        continue;
      }
      const near = ticks
        .map((tick, i) => [tick, i])
        .filter(([tick]) =>
          step > 1
            ? tick.n >= n - step / 2 && tick.n < n + step / 2
            : tick.n === n,
        );
      const marks = near
        .slice(0, STRIP_MARKS)
        .map(
          ([tick, i]) =>
            `<button type="button" class="strip-mark is-${escapeHtml(tick.kind)}" data-mark="${i}" title="${escapeHtml(tick.title ?? "")}"></button>`,
        )
        .join("");
      const more =
        near.length > STRIP_MARKS
          ? `<span class="strip-more">+${near.length - STRIP_MARKS}</span>`
          : "";
      const caption = step > 1 ? clock(this.timeOf(n)).slice(0, -2) : n;
      figure.innerHTML = `<img alt=""><figcaption><span data-cap="${n}">${caption}</span><span class="strip-marks">${marks}${more}</span></figcaption>`;
      const img = figure.querySelector("img");
      imageUrl(thumb(n), signal).then(
        (url) => {
          this.stripUrls.push(url);
          img.src = url;
        },
        (error) => {
          if (!isAbort(error)) figure.classList.add("is-missing");
        },
      );
      if (near.length) figure.classList.add("has-marks");
      figures.push(figure);
    }
    strip.replaceChildren(...figures);
    this.markStrip();
  }

  /** The strip's pictures let go, their loads stopped: it is drawn again,
   * or the source changes */
  forgetStrip() {
    this.stripLoads?.abort();
    this.stripLoads = new AbortController();
    for (const url of this.stripUrls ?? []) URL.revokeObjectURL(url);
    this.stripUrls = [];
  }

  /** Mark the thumbnail nearest the current frame and keep it in view */
  markStrip() {
    const strip = this.strip;
    let current = null;
    let best = Infinity;
    for (const figure of strip.children) {
      const gap = Math.abs(Number(figure.dataset.n) - this.frame);
      figure.classList.remove("current");
      if (!figure.classList.contains("is-empty") && gap < best) {
        best = gap;
        current = figure;
      }
    }
    if (!current) return;
    current.classList.add("current");
    if (strip.scrollWidth > strip.clientWidth) {
      strip.scrollLeft =
        current.offsetLeft - (strip.clientWidth - current.offsetWidth) / 2;
    }
  }

  /** The labels in short under the thumbnails that have them */
  drawCaptions(rows) {
    if (!this.strip || this.stripStep() > 1) return;
    for (const { n, truth, pred } of rows) {
      const caption = this.strip.querySelector(`[data-cap="${n}"]`);
      if (caption)
        caption.innerHTML = `${n}: ${summaryText(truth !== undefined ? truth : pred)}`;
    }
  }

  drawStripNote() {
    if (!this.stripNote) return;
    const span = this.radius * (this.spacing ?? 0);
    this.stripNote.textContent = this.spacing
      ? t("player.strip.seconds", {
          span: Number.isInteger(span) ? span : span.toFixed(1),
        })
      : t("player.strip.frames", { n: this.radius });
  }

  // ------------------------------------------------------------ playback

  play() {
    const source = this.source;
    if (this.playing || !source || this.frame >= this.frames - 1) return;
    this.playing = true;
    this.markPlay();
    this.options.onPlay?.(true);
    if (!source.frame) {
      this.video.play().catch((error) => {
        console.warn("Player video:", error);
        this.settle();
      });
      return;
    }
    if (source.audio && this.sound) return this.playWithSound();
    let due = performance.now();
    const step = async () => {
      if (!this.playing || source !== this.source) return;
      if (this.frame >= this.frames - 1) return this.toggle();
      const next = this.frame + 1;
      // Wait for the frame rather than skip it: timing stays checkable
      await this.image(next).loaded;
      if (!this.playing || source !== this.source) return;
      this.show(next);
      const interval = 1000 / (this.fps * this.speed());
      due = Math.max(due + interval, performance.now() - interval);
      setTimeout(step, Math.max(0, due - performance.now()));
    };
    step();
  }

  /**
   * Play the source's sound from the current frame and let its clock set
   * the frame: frame n is at n / fps in the file, sound included. Frames not
   * decoded in time are skipped, so picture and sound stay together.
   */
  async playWithSound() {
    const { audio, source } = this;
    if (audio.dataset.source !== source.audio) {
      audio.dataset.source = source.audio;
      audio.src = source.audio;
    }
    audio.playbackRate = this.speed();
    audio.preservesPitch = true;
    audio.currentTime = this.time();
    try {
      await audio.play();
    } catch (error) {
      // No sound after all (autoplay refused, decoding failed): play silently
      console.warn("Player sound:", error);
      this.sound = false;
      this.markSound();
      this.playing = false;
      return this.play();
    }
    const follow = () => {
      if (!this.playing || source !== this.source) return;
      if (audio.ended) return this.toggle();
      const n = this.frameOf(audio.currentTime);
      if (n !== this.frame) this.show(n);
      requestAnimationFrame(follow);
    };
    requestAnimationFrame(follow);
  }

  /** Follow every frame the video presents while it plays */
  followVideo() {
    const { video, source } = this;
    if (!this.playing || !source || source.frame) return;
    const tick = (_, meta) => {
      if (!this.playing || source !== this.source) return;
      const n = this.frameOf(meta ? meta.mediaTime : video.currentTime);
      if (n !== this.frame) this.show(n);
      video.requestVideoFrameCallback(tick);
    };
    if (video.requestVideoFrameCallback) video.requestVideoFrameCallback(tick);
  }

  /** The video stopped on its own (paused, ended): settle on its frame */
  settle() {
    if (this.playing) {
      this.playing = false;
      this.markPlay();
      this.options.onPlay?.(false);
    }
    if (this.source && !this.source.frame && this.video.readyState >= 1)
      this.show(this.frameOf(this.video.currentTime));
  }

  pause() {
    if (!this.playing) return;
    this.playing = false;
    this.audio.pause();
    if (this.source && !this.source.frame) this.video.pause();
    this.markPlay();
    this.options.onPlay?.(false);
  }

  toggle() {
    if (!this.playing) return this.play();
    this.pause();
    this.show(this.frame);
  }

  markPlay() {
    this.part("play").textContent = this.playing
      ? t("player.pause")
      : t("player.play");
  }

  /** Show whether sound is on, for sources that have it */
  markSound() {
    const button = this.part("sound");
    button.hidden = !this.source?.sound;
    button.setAttribute("aria-pressed", String(this.sound));
  }

  /** Ask for a frame number or a time (12.5s, 1:02.5) and go there */
  goTo() {
    if (!this.source) return;
    const text = prompt(t("player.gotoAsk"));
    if (!text) return;
    const value = text.trim();
    if (/^\d+$/.test(value)) return this.go(parseInt(value));
    const parts = value.replace(/s$/, "").split(":").map(parseFloat);
    if (parts.some(isNaN)) return;
    const seconds = parts.reduce((total, part) => total * 60 + part, 0);
    this.go(Math.round(seconds * this.fps));
  }

  // ------------------------------------------------------------ scrubber

  drawScrubber(n) {
    const { frames } = this;
    const percent = frames > 1 ? (100 * n) / (frames - 1) : 0;
    this.fill.style.width = `${percent}%`;
    this.thumb.style.left = `${percent}%`;
  }

  /** Marks on the scrubber; drawn once per change */
  setMarks({ ticks = [], ranges = [] }) {
    this.marks = { ticks, ranges };
    this.stripKey = "";
    this.drawMarks();
    if (!this.playing) this.scheduleStrip();
  }

  /**
   * Draw the marks: ranges as bands, short ticks (frames holding only model
   * boxes) from the middle down, full-height ticks on top; colors from the
   * `--mark-<kind>` variables
   */
  drawMarks() {
    const marks = this.marksCanvas;
    const dpr = window.devicePixelRatio || 1;
    const width = Math.round(marks.clientWidth * dpr);
    const height = Math.round(marks.clientHeight * dpr);
    if (marks.width !== width || marks.height !== height) {
      marks.width = width;
      marks.height = height;
    }
    const g = marks.getContext("2d");
    g.clearRect(0, 0, width, height);
    const frames = this.frames;
    if (!frames || !width) return;
    const style = getComputedStyle(marks);
    const color = (kind) =>
      style.getPropertyValue(`--mark-${kind}`).trim() || style.color;
    const x = (n) => (frames > 1 ? (n / (frames - 1)) * (width - 1) : 0);
    const tick = Math.max(2 * dpr, width / frames);
    const { ticks, ranges } = this.marks;
    g.globalAlpha = 0.35;
    for (const range of ranges) {
      g.fillStyle = color(range.kind);
      const [a, b] = [Math.min(range.a, range.b), Math.max(range.a, range.b)];
      g.fillRect(x(a), 0, Math.max(tick, x(b) - x(a)), height);
    }
    g.globalAlpha = 1;
    for (const short of [true, false]) {
      for (const mark of ticks) {
        if (Boolean(mark.short) !== short) continue;
        g.fillStyle = color(mark.kind);
        g.fillRect(
          x(mark.n) - tick / 2,
          short ? height * 0.45 : 0,
          tick,
          height,
        );
      }
    }
  }

  /** The tick nearest to frame n, if within `px` CSS pixels of it */
  nearestMark(n, px) {
    const perFrame = this.scrubber.clientWidth / Math.max(1, this.frames - 1);
    let best = null;
    for (const tick of this.marks.ticks) {
      if (best == null || Math.abs(tick.n - n) < Math.abs(best.n - n))
        best = tick;
    }
    return best && Math.abs(best.n - n) * perFrame <= px ? best : null;
  }

  /** Go to a mark's frame and tell the app */
  goMark(tick) {
    this.go(tick.n);
    this.options.onMark?.(tick);
  }

  buildScrubber() {
    const { scrubber, bubble } = this;
    let timer = null;
    let dragging = false;
    /** Where the pointer went down, to tell a click from a drag */
    let downX = 0;
    const frameAt = (event) => {
      const box = scrubber.getBoundingClientRect();
      const x = (event.clientX - box.left) / box.width;
      return this.clamp(
        Math.round(Math.max(0, Math.min(1, x)) * (this.frames - 1)),
      );
    };
    const showBubble = (n, text) => {
      bubble.hidden = false;
      bubble.style.left = `${(100 * n) / Math.max(1, this.frames - 1)}%`;
      bubble.textContent = text;
    };
    const preview = (n) => {
      this.drawScrubber(n);
      showBubble(n, `${n} · ${clock(this.timeOf(n))}`);
    };
    scrubber.addEventListener("pointerdown", (event) => {
      if (!this.source || !this.frames) return;
      dragging = true;
      downX = event.clientX;
      scrubber.setPointerCapture(event.pointerId);
      this.pause();
      preview(frameAt(event));
    });
    scrubber.addEventListener("pointermove", (event) => {
      if (!this.source || !this.frames) return;
      const n = frameAt(event);
      if (!dragging) {
        // Resting near a titled mark tells what it is
        const mark = this.nearestMark(n, MARK_SNAP_PX);
        if (mark?.title) showBubble(mark.n, mark.title);
        else bubble.hidden = true;
        return;
      }
      preview(n);
      clearTimeout(timer);
      timer = setTimeout(() => this.show(n), SCRUB_DEBOUNCE_MS);
    });
    scrubber.addEventListener("pointerleave", () => {
      if (!dragging) bubble.hidden = true;
    });
    scrubber.addEventListener("pointerup", (event) => {
      if (!dragging) return;
      dragging = false;
      clearTimeout(timer);
      bubble.hidden = true;
      // A click (not a drag) next to a mark goes to that frame
      const n = frameAt(event);
      const click = Math.abs(event.clientX - downX) < 4;
      const mark = click ? this.nearestMark(n, MARK_SNAP_PX) : null;
      if (mark) this.goMark(mark);
      else this.go(n);
    });
  }
}
