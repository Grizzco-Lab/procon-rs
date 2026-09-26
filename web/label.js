// Inkspector labeling mode: boxes around objects on recorded frames, drawn
// with the shared drawing layer (sketch.js) and saved frame by frame through
// POST /api/inspect/objects (format in src/objects.rs). Runs after
// inspect.js and uses its state (inspector, go, remembered, remember).
"use strict";

(() => {
  const bar = $("l-bar");
  const toggleButton = $("i-label");
  const screen = $("i-screen");

  const labels = {
    /** Whether the mode is on */
    on: remembered("label", "false") === "true",
    /** Classes from classes.json, and the annotations folder */
    classes: [],
    dir: "",
    /** Index of the class new boxes get */
    current: 0,
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
  };

  const sketch = new Sketch(screen, {
    onChange: changed,
    onSelect: drawBar,
  });
  sketch.setTool("rect");

  const classOf = (name) => labels.classes.find((c) => c.name === name);
  const round = (v) => Math.round(v * 1e5) / 1e5;

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

  // ------------------------------------------------------------- loading

  async function load(info) {
    const key = `${info.session}/${info.segment}`;
    labels.key = key;
    labels.frames = new Map();
    labels.base = new Map();
    try {
      if (!labels.classes.length) {
        const response = await fetch("/api/inspect/classes");
        const data = await response.json();
        if (!response.ok) throw new Error(data.error);
        labels.classes = data.classes;
        labels.dir = data.dir;
        setClass(0);
      }
      const query = new URLSearchParams({ s: info.session, seg: info.segment });
      const response = await fetch(`/api/inspect/objects?${query}`);
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      if (labels.key !== key) return;
      for (const line of data.frames) setFrame(line.frame, line.boxes);
      setStatus("");
    } catch (error) {
      labels.key = null;
      setStatus(error.message, true);
    }
    render();
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

  // -------------------------------------------------------------- saving

  /** The layer changed: keep its boxes and save the frame */
  function changed(shapes) {
    const frame = labels.frame;
    const boxes = shapes.map(boxOf);
    if (boxes.length) labels.frames.set(frame, boxes);
    else labels.frames.delete(frame);
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
        const response = await fetch("/api/inspect/objects", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        });
        const saved = await response.json();
        if (!response.ok) throw new Error(saved.error);
        if (labels.key !== key) return;
        // Model boxes written meanwhile come back with it
        setFrame(frame, saved.boxes);
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

  /** Key that picks class `index`: 1-9, 0, then Shift+1… */
  function classKey(index) {
    const digit = String((index % 10) + 1).slice(-1);
    return index < 10 ? digit : `⇧${digit}`;
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
    const chips = labels.classes
      .map((cls, i) => {
        const pressed = selected
          ? selected.class === cls.name
          : i === labels.current;
        return `<button type="button" class="label-class" data-class="${i}" aria-pressed="${pressed}" title="${escapeHtml(cls.name)}"><kbd>${classKey(i)}</kbd><i style="background:${escapeHtml(cls.color)}"></i>${escapeHtml(cls.label)}<span class="num">${counts.get(cls.name) ?? 0}</span></button>`;
      })
      .join("");
    const here = labels.frames.get(inspector.frame) ?? [];
    const models = here.filter((b) => b.by === "model").length;
    bar.innerHTML = `
      <div class="label-classes">${chips}</div>
      <div class="label-actions">
        <button type="button" class="btn" data-act="prev" title="Previous labeled frame (P)">‹ Labeled</button>
        <button type="button" class="btn" data-act="next" title="Next labeled frame (N)">Labeled ›</button>
        <button type="button" class="btn" data-act="copy" title="Copy the boxes of the previous labeled frame (C)">Copy previous</button>
        <button type="button" class="btn" data-act="accept" title="Accept the selected model box, or all of this frame's (A)" ${models ? "" : "disabled"}>Accept model${models ? ` (${models})` : ""}</button>
        <button type="button" class="btn" data-act="delete" title="Delete the selected box (Del)" ${sketch.selected < 0 ? "disabled" : ""}>Delete box</button>
        <span class="label-status num">${labels.frames.size} frames · ${boxes} boxes${labels.status ? ` · <span class="${labels.error ? "level-critical" : ""}">${escapeHtml(labels.status)}</span>` : ""}</span>
      </div>
      <p class="panel-note">Drag on the frame to box an object of the chosen class; drag a box to move it, its corners to resize it. Dashed boxes are the model's. Saved to <span class="path">${escapeHtml(labels.dir)}</span>.</p>`;
  }

  // ------------------------------------------------------------ actions

  function toggle(on = !labels.on) {
    labels.on = on;
    remember("label", String(on));
    toggleButton.setAttribute("aria-pressed", String(on));
    screen.classList.toggle("labeling", on);
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
    else if (name === "delete") sketch.removeSelected();
  }

  bar.addEventListener("click", (event) => {
    const target = event.target.closest("[data-class], [data-act]");
    if (!target) return;
    if (target.dataset.class != null) pickClass(Number(target.dataset.class));
    else act(target.dataset.act);
  });
  toggleButton.addEventListener("click", () => toggle());

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
    else if (key === "a") act("accept");
    else if (key === "delete" || key === "backspace") act("delete");
    else if (key === "escape") sketch.select(-1);
    else return;
    event.preventDefault();
  });

  window.addEventListener("inspect-frame", () => {
    const { info } = inspector;
    if (!labels.on || !info) return;
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
    // Back in the picker: nothing to label
    if (!inspector.info) drawBar();
  });

  toggleButton.setAttribute("aria-pressed", String(labels.on));
  screen.classList.toggle("labeling", labels.on);
  sketch.setEditable(labels.on);
})();
