// Inkspector labeling mode: boxes around objects on recorded frames, drawn
// with the shared drawing layer (sketch.js) and saved frame by frame through
// POST /api/inspect/objects (format in src/inspect/objects.rs). Follow carries the
// boxes of a frame over the next frames with a tracker, as model boxes to
// accept or correct (/api/inspect/follow/..., src/inspect/follow.rs). The labeled
// frames are marked on the scrubber, in the labeling mode or not. Runs after
// inspect.js and uses its state (inspector, go, drawMarks, remembered,
// remember).
"use strict";

(() => {
  const bar = $("l-bar");
  const chipsEl = $("l-chips");
  const actionsEl = $("l-actions");
  const noteEl = $("l-note");
  const find = $("l-find");
  const toggleButton = $("i-label");
  const screen = $("i-screen");
  const scrubber = $("i-scrubber");
  const followEl = {
    run: $("l-follow-run"),
    count: $("l-follow-count"),
    direction: $("l-follow-direction"),
    cancel: $("l-follow-cancel"),
    start: $("l-follow-start"),
    progress: $("l-follow-progress"),
    status: $("l-follow-status"),
  };

  /** Wait between polls of a running Follow, in ms */
  const FOLLOW_POLL_MS = 300;
  /** Longest wait for a tracker started from the page, in ms */
  const TRACKER_START_MS = 120000;

  const labels = {
    /** Whether the mode is on */
    on: remembered("label", "false") === "true",
    /** Classes from classes.json, and the annotations folder */
    classes: [],
    dir: "",
    /** Index of the class new boxes get */
    current: 0,
    /** Text typed into the class finder */
    filter: "",
    /** Session and segment whose labels are loaded */
    key: null,
    /** Boxes by frame, as saved */
    frames: new Map(),
    /** Model boxes by frame, as loaded: what the user has seen */
    base: new Map(),
    /** Frame on the drawing layer */
    frame: -1,
    /** Saves run one after another */
    saving: Promise.resolve(),
    status: "",
    error: false,
    follow: {
      /** The current or last Follow, as the lab reports it */
      job: null,
      timer: null,
      /** Frames written when the labels were last read again */
      written: 0,
      /** Id of the Follow whose outcome is shown */
      reported: null,
      /** Message (HTML) next to the Follow button, and its tooltip */
      html: "",
      title: "",
      error: false,
      /** Whether the lab can start the tracker */
      canStart: false,
    },
  };

  const sketch = new Sketch(screen, {
    onChange: changed,
    onSelect: drawBar,
  });
  sketch.setTool("rect");

  const classOf = (name) => labels.classes.find((c) => c.name === name);
  const round = (v) => Math.round(v * 1e5) / 1e5;
  const byModel = (box) => box.by === "model";
  const plural = (n, word, many = `${word}s`) =>
    `${n} ${n === 1 ? word : many}`;

  /** A box as a shape on the drawing layer */
  function shapeOf(box) {
    const cls = classOf(box.class);
    const score = box.score != null ? ` ${box.score.toFixed(2)}` : "";
    return {
      kind: "rect",
      points: [
        [box.x, box.y],
        [box.x + box.w, box.y + box.h],
      ],
      color: cls?.color ?? "#ffffff",
      dashed: box.by === "model",
      tag: `${cls?.label ?? box.class}${box.by === "model" ? score : ""}`,
      box,
    };
  }

  /** A shape back as a box; moved or resized model boxes become the user's */
  function boxOf(shape) {
    const [x0, y0, x1, y1] = sketchBounds(shape.points);
    const box = shape.box
      ? { ...shape.box }
      : { class: labels.classes[labels.current]?.name ?? "object", by: "user" };
    if (shape.edited) mine(box);
    Object.assign(box, {
      x: round(x0),
      y: round(y0),
      w: round(x1 - x0),
      h: round(y1 - y0),
    });
    return box;
  }

  /** Make a box the user's: accepted or corrected */
  function mine(box) {
    box.by = "user";
    delete box.score;
    return box;
  }

  const active = () => labels.on && inspector.shown && inspector.info;

  /** Fetch JSON, posting `body` if given; errors carry the lab's message */
  async function fetchJson(url, body) {
    const options =
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          };
    const response = await fetch(url, options);
    const data = await response.json();
    if (!response.ok) throw new Error(data?.error ?? response.statusText);
    return data;
  }

  // ------------------------------------------------------------- loading

  /** Load a segment's labels (for the marks even when the mode is off) */
  async function load(info) {
    const key = `${info.session}/${info.segment}`;
    labels.key = key;
    labels.frames = new Map();
    labels.base = new Map();
    updateMarks();
    try {
      if (!labels.classes.length) {
        const data = await fetchJson("/api/inspect/classes");
        labels.classes = data.classes;
        labels.dir = data.dir;
        setClass(0);
      }
      await refresh();
      setStatus("");
    } catch (error) {
      labels.key = null;
      setStatus(error.message, true);
    }
    render();
    // A Follow may be under way on this segment (after a reload)
    pollFollow();
  }

  /** Read the segment's labels again, keeping a box being dragged */
  async function refresh() {
    const { info } = inspector;
    const key = labels.key;
    const query = new URLSearchParams({ s: info.session, seg: info.segment });
    const data = await fetchJson(`/api/inspect/objects?${query}`);
    if (labels.key !== key) return;
    const dragged = sketch.drag ? labels.frames.get(labels.frame) : undefined;
    labels.frames = new Map();
    labels.base = new Map();
    for (const line of data.frames) setFrame(line.frame, line.boxes);
    if (dragged) labels.frames.set(labels.frame, dragged);
    updateMarks();
    if (!sketch.drag) render();
  }

  /** Keep a frame's boxes as saved, and its model boxes as seen */
  function setFrame(frame, boxes) {
    if (boxes.length) labels.frames.set(frame, boxes);
    else labels.frames.delete(frame);
    labels.base.set(
      frame,
      boxes.filter((b) => b.by === "model"),
    );
  }

  /** Hand the labeled frames to the scrubber */
  function updateMarks() {
    const user = [];
    const model = [];
    for (const [frame, boxes] of labels.frames) {
      (boxes.some((b) => b.by !== "model") ? user : model).push(frame);
    }
    const byNumber = (a, b) => a - b;
    inspector.marks.user = user.sort(byNumber);
    inspector.marks.model = model.sort(byNumber);
    drawMarks();
  }

  // -------------------------------------------------------------- saving

  /** The layer changed: keep its boxes and save the frame */
  function changed(shapes) {
    const frame = labels.frame;
    const boxes = shapes.map(boxOf);
    if (boxes.length) labels.frames.set(frame, boxes);
    else labels.frames.delete(frame);
    updateMarks();
    render();
    save(frame);
  }

  function save(frame) {
    const { info } = inspector;
    const body = {
      s: info.session,
      seg: info.segment,
      frame,
      boxes: labels.frames.get(frame) ?? [],
      base: labels.base.get(frame) ?? [],
    };
    const key = labels.key;
    setStatus("saving…");
    labels.saving = labels.saving.then(async () => {
      try {
        const saved = await fetchJson("/api/inspect/objects", body);
        if (labels.key !== key) return;
        // Model boxes written meanwhile come back with it
        setFrame(frame, saved.boxes);
        updateMarks();
        setStatus("saved");
        if (frame === labels.frame && !sketch.drag) render();
      } catch (error) {
        setStatus(`not saved: ${error.message}`, true);
      }
    });
  }

  function setStatus(text, error = false) {
    labels.status = text;
    labels.error = error;
    drawBar();
  }

  // ------------------------------------------------------------ drawing

  /** Draw the current frame's boxes */
  function render() {
    if (!active()) return;
    const frame = inspector.frame;
    if (frame !== labels.frame) sketch.select(-1);
    labels.frame = frame;
    sketch.set((labels.frames.get(frame) ?? []).map(shapeOf));
    drawBar();
  }

  function setClass(index) {
    labels.current = index;
    sketch.color = labels.classes[index]?.color ?? "#ffffff";
    drawBar();
  }

  /** Key that picks class `index`: 1-9, 0, then Shift+1…0; later ones have none */
  function classKey(index) {
    if (index >= 20) return "";
    const digit = String((index % 10) + 1).slice(-1);
    return index < 10 ? digit : `⇧${digit}`;
  }

  /** Classes the finder's text matches, those whose name starts with it first */
  function found() {
    const text = labels.filter.trim().toLowerCase();
    const all = labels.classes.map((cls, i) => i);
    if (!text) return all;
    const words = (cls) => `${cls.label} ${cls.name}`.toLowerCase();
    const starts = all.filter((i) => words(labels.classes[i]).startsWith(text));
    const within = all.filter(
      (i) => !starts.includes(i) && words(labels.classes[i]).includes(text),
    );
    return [...starts, ...within];
  }

  /** Classes with their counts, the actions and the save status */
  function drawBar() {
    bar.hidden = !labels.on || !inspector.info;
    if (bar.hidden) return;
    const counts = new Map();
    let boxes = 0;
    for (const frameBoxes of labels.frames.values()) {
      for (const box of frameBoxes) {
        counts.set(box.class, (counts.get(box.class) ?? 0) + 1);
        boxes += 1;
      }
    }
    const selected = sketch.shapes[sketch.selected]?.box;
    const matches = found();
    chipsEl.innerHTML = labels.classes
      .map((cls, i) => {
        const pressed = selected
          ? selected.class === cls.name
          : i === labels.current;
        const key = classKey(i);
        const hidden = matches.includes(i) ? "" : " hidden";
        const first = labels.filter && matches[0] === i ? " data-first" : "";
        return `<button type="button" class="label-class" data-class="${i}" aria-pressed="${pressed}" title="${escapeHtml(cls.name)}"${hidden}${first}>${key ? `<kbd>${key}</kbd>` : ""}<i style="background:${escapeHtml(cls.color)}"></i>${escapeHtml(cls.label)}<span class="num">${counts.get(cls.name) ?? 0}</span></button>`;
      })
      .join("");
    const here = labels.frames.get(inspector.frame) ?? [];
    const models = here.filter((b) => b.by === "model").length;
    const span = acceptSpan();
    const spanTitle = span.frames.length
      ? `Accept ${plural(span.boxes, "suggestion")} on ${plural(span.frames.length, "frame")}, from ${span.after < 0 ? "the start of the segment" : `after reviewed frame ${span.after}`} up to this one (Shift+A)`
      : "No suggestions between the last reviewed frame and this one (Shift+A)";
    actionsEl.innerHTML = `
      <button type="button" class="btn" data-act="prev" title="Previous labeled frame (P)">‹ Labeled</button>
      <button type="button" class="btn" data-act="next" title="Next labeled frame (N)">Labeled ›</button>
      <button type="button" class="btn" data-act="copy" title="Copy the boxes of the previous labeled frame (C)">Copy previous</button>
      <button type="button" class="btn" data-act="accept" title="Accept the selected suggestion (dashed box), or all of this frame's (A)" ${models ? "" : "disabled"}>Accept suggestions${models ? ` (${models})` : ""}</button>
      <button type="button" class="btn" data-act="accept-span" title="${escapeHtml(spanTitle)}" ${span.frames.length ? "" : "disabled"}>Accept up to here${span.frames.length ? ` (${span.frames.length})` : ""}</button>
      <button type="button" class="btn" data-act="delete" title="Delete the selected box (Del)" ${sketch.selected < 0 ? "disabled" : ""}>Delete box</button>
      <span class="label-status num">${labels.frames.size} frames · ${boxes} boxes${labels.status ? ` · <span class="${labels.error ? "level-critical" : ""}">${escapeHtml(labels.status)}</span>` : ""}</span>`;
    noteEl.innerHTML = `Drag on the frame to box an object of the chosen class; drag a box to move it, its corners to resize it. Dashed boxes are suggestions (from Follow or a detector). <b>Follow</b> (F) carries the selected box, or all of the frame's, over the next frames as suggestions: step through with →, accept with A, fix a box that drifted and Follow again from there. It goes on over frames you labeled, adding a box wherever none of yours of the same class covers the object. Shift+A accepts every suggestion from after the last frame you fully reviewed (your boxes only) up to this one. Type / to find a class. Saved to <span class="path">${escapeHtml(labels.dir)}</span>.`;
    drawFollow();
  }

  // ------------------------------------------------------------ actions

  function toggle(on = !labels.on) {
    labels.on = on;
    remember("label", String(on));
    toggleButton.setAttribute("aria-pressed", String(on));
    screen.classList.toggle("labeling", on);
    scrubber.classList.toggle("is-faint", !on);
    sketch.setEditable(on);
    if (!on) {
      sketch.set([]);
      drawBar();
      return;
    }
    const { info } = inspector;
    if (info && labels.key !== `${info.session}/${info.segment}`) load(info);
    else render();
  }

  /** The selected box, or all boxes, change hands or class */
  function changeSelected(change) {
    const index = sketch.selected;
    if (index < 0) return false;
    const shape = sketch.shapes[index];
    const box = change(mine(boxOf(shape)));
    sketch.shapes[index] = shapeOf(box);
    changed(sketch.shapes);
    sketch.select(index);
    return true;
  }

  function pickClass(index) {
    if (index >= labels.classes.length) return;
    // The selected box takes the class, and so do the next ones drawn
    const name = labels.classes[index].name;
    changeSelected((box) => ({ ...box, class: name }));
    setClass(index);
  }

  /** Accept the selected model box, or every model box on the frame */
  function accept() {
    const shape = sketch.shapes[sketch.selected];
    if (shape?.box?.by === "model") return changeSelected((box) => box);
    const boxes = (labels.frames.get(labels.frame) ?? []).map((box) =>
      box.by === "model" ? mine({ ...box }) : box,
    );
    sketch.set(boxes.map(shapeOf));
    changed(sketch.shapes);
  }

  /** What Accept up to here takes: the frames with model boxes after the
   * last frame before this one that a person fully reviewed (boxes of
   * people only; Follow adds model boxes to labeled frames too), up to
   * this one */
  function acceptSpan() {
    const here = labels.frame;
    let after = -1;
    for (const [frame, boxes] of labels.frames) {
      const reviewed = boxes.length && !boxes.some(byModel);
      if (frame < here && frame > after && reviewed) after = frame;
    }
    const frames = [...labels.frames.keys()]
      .filter(
        (frame) =>
          frame > after &&
          frame <= here &&
          labels.frames.get(frame).some(byModel),
      )
      .sort((a, b) => a - b);
    const boxes = frames.reduce(
      (n, frame) => n + labels.frames.get(frame).filter(byModel).length,
      0,
    );
    return { after, frames, boxes };
  }

  /** Accept every model box from after the last labeled frame up to this
   * one (Shift+A), saved in one write */
  function acceptUpToHere() {
    const { frames, boxes } = acceptSpan();
    if (!frames.length) return setStatus("no suggestions to accept", true);
    const changes = frames.map((frame) => {
      const accepted = labels.frames
        .get(frame)
        .map((box) => (byModel(box) ? mine({ ...box }) : box));
      labels.frames.set(frame, accepted);
      return { frame, boxes: accepted, base: labels.base.get(frame) ?? [] };
    });
    updateMarks();
    render();
    const { info } = inspector;
    const body = { s: info.session, seg: info.segment, frames: changes };
    const key = labels.key;
    const done = `Accepted ${plural(boxes, "box", "boxes")} on ${plural(frames.length, "frame")}`;
    setStatus("saving…");
    labels.saving = labels.saving.then(async () => {
      try {
        const saved = await fetchJson("/api/inspect/objects", body);
        if (labels.key !== key) return;
        // Model boxes written meanwhile come back with them
        for (const line of saved.frames) setFrame(line.frame, line.boxes);
        updateMarks();
        setStatus(done);
        if (!sketch.drag) render();
      } catch (error) {
        setStatus(`not saved: ${error.message}`, true);
      }
    });
  }

  /** Nearest labeled frame before (-1) or after (+1) this one */
  function labeled(direction) {
    const frames = [...labels.frames.keys()].filter((k) =>
      direction < 0 ? k < inspector.frame : k > inspector.frame,
    );
    if (!frames.length) return null;
    return direction < 0 ? Math.max(...frames) : Math.min(...frames);
  }

  /** Add the boxes of the previous labeled frame, as the user's */
  function copyPrevious() {
    const from = labeled(-1);
    if (from == null)
      return setStatus("no labeled frame before this one", true);
    const copies = labels.frames.get(from).map((box) => mine({ ...box }));
    sketch.set([...sketch.shapes, ...copies.map(shapeOf)]);
    changed(sketch.shapes);
  }

  function act(name) {
    if (name === "prev" || name === "next") {
      const frame = labeled(name === "prev" ? -1 : 1);
      if (frame != null) go(frame);
    } else if (name === "copy") copyPrevious();
    else if (name === "accept") accept();
    else if (name === "accept-span") acceptUpToHere();
    else if (name === "delete") sketch.removeSelected();
  }

  // ------------------------------------------------------------- Follow

  const finished = (job) => ["done", "failed", "cancelled"].includes(job.state);
  const onThisSegment = (job) =>
    job &&
    inspector.info &&
    job.s === inspector.info.session &&
    job.seg === inspector.info.segment;
  const deviceName = (device) => (device === "cuda" ? "GPU" : "CPU");

  /** Show a message (HTML) next to the Follow button, `title` as its tooltip */
  function setFollow(html, error = false, title = "") {
    labels.follow.html = html;
    labels.follow.error = error;
    labels.follow.title = title;
    drawFollow();
  }

  /** What a running Follow is doing */
  function followProgress(job) {
    if (job.state === "starting")
      return "Starting the tracker (the first Follow loads its model)…";
    let speed = "";
    if (job.ms_per_frame != null) {
      const left = ((job.planned - job.done) * job.ms_per_frame) / 1000;
      const eta =
        left >= 90 ? `${Math.round(left / 60)} min` : `${Math.round(left)} s`;
      speed = ` · ${job.ms_per_frame} ms/frame · ${eta} left`;
    }
    // Why the CPU (the GPU is busy) matters: it is some 30 times slower
    const why =
      job.device === "cpu" && job.device_reason
        ? ` (${job.device_reason})`
        : "";
    return `Following ${job.following}/${job.objects} ${job.pass ?? ""} · ${job.done}/${job.planned} frames · ${deviceName(job.device)}${why}${speed}`;
  }

  /** Lost objects shown in a Follow's outcome; the rest are in its tooltip */
  const LOST_SHOWN = 3;

  /** What a finished Follow did, and every lost object for the tooltip */
  function followOutcome(job) {
    if (job.state === "failed")
      return [escapeHtml(`Follow failed: ${job.error}`), ""];
    const lost = job.lost.map(
      (l) =>
        `${classOf(l.class)?.label ?? l.class} #${l.id} at ${l.frame} (${l.reason})`,
    );
    const more = lost.length - LOST_SHOWN;
    const lostText = lost.length
      ? `lost ${lost.length}: ${lost.slice(0, LOST_SHOWN).join(", ")}${more > 0 ? ` and ${more} more` : ""}`
      : "";
    const parts = [
      `${job.state === "cancelled" ? "Cancelled after" : "Followed"} ${plural(job.objects, "object")} over ${plural(job.done, "frame")}`,
      job.device
        ? `${deviceName(job.device)}${job.ms_per_frame != null ? `, ${job.ms_per_frame} ms/frame` : ""}`
        : "",
      `${plural(job.written, "frame")} written`,
      job.covered ? `${plural(job.covered, "box", "boxes")} left to yours` : "",
      lostText,
      job.notes.length ? `stopped: ${job.notes.join("; ")}` : "",
    ];
    const title = [job.device_reason ?? "", ...lost].filter(Boolean).join("\n");
    return [escapeHtml(parts.filter(Boolean).join(" · ")), title];
  }

  function drawFollow() {
    const job = labels.follow.job;
    const running = job && !finished(job) && onThisSegment(job);
    followEl.run.disabled = Boolean(running) || !inspector.info;
    followEl.cancel.hidden = !running;
    followEl.start.hidden = !labels.follow.canStart || Boolean(running);
    followEl.progress.hidden = !running;
    if (running) {
      const part = job.planned ? job.done / job.planned : 0;
      followEl.progress.firstElementChild.style.width = `${Math.round(100 * part)}%`;
      followEl.status.textContent = followProgress(job);
      followEl.status.title = job.device_reason ?? "";
    } else {
      followEl.status.innerHTML = labels.follow.html;
      followEl.status.title = labels.follow.title;
    }
    followEl.status.classList.toggle(
      "level-critical",
      !running && labels.follow.error,
    );
  }

  /** The frames a Follow covers, for the band on the scrubber */
  function followRange(job) {
    const last = inspector.info.frames - 1;
    const ahead = job.direction !== "backward" ? job.count : 0;
    const behind = job.direction !== "forward" ? job.count : 0;
    return [Math.max(0, job.frame - behind), Math.min(last, job.frame + ahead)];
  }

  /** The tracker's state; shows how to start it when it does not answer */
  async function trackerReady() {
    const status = await fetchJson("/api/inspect/follow/status");
    labels.follow.canStart = status.can_start && !status.ok;
    if (status.ok) return true;
    const how = status.can_start ? ", or press Start tracker" : "";
    setFollow(
      `Tracker not running at ${escapeHtml(status.url)}: start it with <code>${escapeHtml(status.command)}</code>${how}.`,
      true,
    );
    return false;
  }

  /** Follow the selected box, or every box of the frame */
  async function follow() {
    const { info } = inspector;
    const job = labels.follow.job;
    if (!info || (job && !finished(job))) return;
    const frame = labels.frame;
    const boxes = labels.frames.get(frame) ?? [];
    const selected = boxes[sketch.selected];
    const chosen = selected ? [selected] : boxes;
    if (!chosen.length)
      return setFollow(
        "Box an object first; Follow carries it over the next frames.",
        true,
      );
    try {
      setFollow("Checking the tracker…");
      if (!(await trackerReady())) return;
      // The tracker should see the frame as saved
      await labels.saving;
      const request = {
        s: info.session,
        seg: info.segment,
        frame,
        count: Math.max(
          1,
          Math.round(parseFloat(followEl.count.value) * info.fps),
        ),
        direction: followEl.direction.value,
        boxes: chosen,
      };
      labels.follow.job = await fetchJson("/api/inspect/follow/run", request);
      labels.follow.written = 0;
      setFollow("");
      pollFollow();
    } catch (error) {
      setFollow(escapeHtml(error.message), true);
    }
  }

  /** Watch the Follow: progress, boxes as they are written, the outcome */
  async function pollFollow() {
    const state = labels.follow;
    clearTimeout(state.timer);
    let job;
    try {
      job = await fetchJson("/api/inspect/follow/job");
    } catch (error) {
      return setFollow(escapeHtml(error.message), true);
    }
    state.job = job;
    if (!onThisSegment(job)) {
      inspector.marks.range = null;
      drawMarks();
      return drawFollow();
    }
    if (job.written !== state.written) {
      state.written = job.written;
      await refresh().catch((error) => setStatus(error.message, true));
    }
    const done = finished(job);
    inspector.marks.range = done ? null : followRange(job);
    drawMarks();
    if (done && state.reported !== job.id) {
      state.reported = job.id;
      // An outcome from before this page was opened is not news
      if (job.finished_ms > performance.timeOrigin) {
        const [html, title] = followOutcome(job);
        setFollow(html, job.state === "failed", title);
        if (job.state === "failed") trackerReady().catch(() => {});
      }
    }
    drawFollow();
    if (!done && inspector.shown)
      state.timer = setTimeout(pollFollow, FOLLOW_POLL_MS);
  }

  async function cancelFollow() {
    try {
      await fetchJson("/api/inspect/follow/cancel", {});
    } catch (error) {
      setFollow(escapeHtml(error.message), true);
    }
  }

  /** Start the tracker from the configured command and wait for it */
  async function startTracker() {
    followEl.start.disabled = true;
    try {
      await fetchJson("/api/inspect/follow/start", {});
      setFollow("Starting the tracker…");
      const until = performance.now() + TRACKER_START_MS;
      while (performance.now() < until) {
        await new Promise((resolve) => setTimeout(resolve, 1000));
        const status = await fetchJson("/api/inspect/follow/status");
        if (status.ok) {
          labels.follow.canStart = false;
          const gpu = status.health?.gpu_free_gib;
          return setFollow(
            `Tracker ready${gpu != null ? ` · GPU ${gpu} GiB free` : " · CPU only"}.`,
          );
        }
        if (status.started?.startsWith("exited"))
          return setFollow(
            `The tracker ${escapeHtml(status.started)}; see the lab's log.`,
            true,
          );
      }
      setFollow("The tracker did not answer within two minutes.", true);
    } catch (error) {
      setFollow(escapeHtml(error.message), true);
    } finally {
      followEl.start.disabled = false;
    }
  }

  // ------------------------------------------------------------- events

  bar.addEventListener("click", (event) => {
    const target = event.target.closest("[data-class], [data-act]");
    if (!target) return;
    if (target.dataset.class != null) pickClass(Number(target.dataset.class));
    else act(target.dataset.act);
  });
  toggleButton.addEventListener("click", () => toggle());
  followEl.run.addEventListener("click", follow);
  followEl.cancel.addEventListener("click", cancelFollow);
  followEl.start.addEventListener("click", startTracker);
  followEl.count.value = remembered("follow-count", "2");
  followEl.direction.value = remembered("follow-direction", "forward");
  followEl.count.addEventListener("change", () =>
    remember("follow-count", followEl.count.value),
  );
  followEl.direction.addEventListener("change", () =>
    remember("follow-direction", followEl.direction.value),
  );

  // The class finder: type to filter the chips, Enter picks the first
  find.addEventListener("input", () => {
    labels.filter = find.value;
    drawBar();
  });
  find.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      const [first] = found();
      if (first != null) pickClass(first);
    } else if (event.key !== "Escape") return;
    event.preventDefault();
    find.value = "";
    labels.filter = "";
    find.blur();
    drawBar();
  });

  document.addEventListener("keydown", (event) => {
    const tag = event.target.tagName;
    if (!inspector.shown || !inspector.info) return;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "l") toggle();
    else if (!labels.on) return;
    else if (/^Digit\d$/.test(event.code)) {
      const digit = Number(event.code.slice(5));
      pickClass(((digit + 9) % 10) + (event.shiftKey ? 10 : 0));
    } else if (key === "n") act("next");
    else if (key === "p") act("prev");
    else if (key === "c") act("copy");
    else if (key === "a") act(event.shiftKey ? "accept-span" : "accept");
    else if (key === "f") follow();
    else if (key === "/") find.focus();
    else if (key === "delete" || key === "backspace") act("delete");
    else if (key === "escape") sketch.select(-1);
    else return;
    event.preventDefault();
  });

  window.addEventListener("inspect-frame", () => {
    const { info } = inspector;
    if (!info) return;
    if (labels.key !== `${info.session}/${info.segment}`) load(info);
    else render();
  });
  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    // Opened with label=1 (from the Vision app): labeling, with the labels
    // read again since they may have changed
    if (app === "inspect" && state.get("label") === "1") {
      labels.key = null;
      if (!labels.on) toggle(true);
    }
    // Back in the Inkspector: carry on watching a Follow
    if (app === "inspect" && inspector.info && labels.key) pollFollow();
    // Back in the picker: nothing to label
    if (!inspector.info) drawBar();
  });

  toggleButton.setAttribute("aria-pressed", String(labels.on));
  screen.classList.toggle("labeling", labels.on);
  scrubber.classList.toggle("is-faint", !labels.on);
  sketch.setEditable(labels.on);
})();
