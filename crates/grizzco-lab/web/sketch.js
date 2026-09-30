// Drawing layer shared by the Cuttlefish app (comments drawn on a paused
// frame) and the Inkspector's labeling mode (object boxes): an SVG over a
// picture where shapes are drawn, selected, moved, resized and deleted.
//
// A shape is {kind, points, color?, ...}; `kind` is rect, ellipse, arrow or
// freehand, and `points` are [x, y] fractions (0-1) of the picture: two
// corners of a rect or ellipse, tail and head of an arrow, every point of a
// freehand line. Display-only keys: `dashed`, `tag` (a caption at the
// top-left corner), `locked` (shown but not editable) and `faded`. Other
// keys ride along untouched, so callers keep their own data on shapes.
"use strict";

/** Freehand points closer than this (a fraction of the width) are skipped */
const SKETCH_MIN_STEP = 0.003;
/** Shapes smaller than this on both axes are dropped as stray clicks */
const SKETCH_MIN_SIZE = 0.008;

class Sketch {
  /**
   * @param host a positioned element; the layer fills it
   * @param options.onChange(shapes) after a shape is added, moved, resized or deleted
   * @param options.onSelect(index) when the selection changes (-1: none)
   * @param options.onDraw() before a new shape starts, so the caller can make
   *   room for it (the Cuttlefish app creates a comment); return false to refuse
   */
  constructor(host, { onChange, onSelect, onDraw } = {}) {
    this.host = host;
    this.onChange = onChange ?? (() => {});
    this.onSelect = onSelect ?? (() => {});
    this.onDraw = onDraw ?? (() => true);
    /** Shapes on the picture */
    this.shapes = [];
    /** Index of the selected shape, or -1 */
    this.selected = -1;
    /** select, rect, ellipse, arrow or freehand */
    this.tool = "select";
    /** Color of new shapes */
    this.color = "#ff5c8a";
    /** Whether shapes can be drawn and changed */
    this.editable = false;
    this.svg = svgEl("svg", { class: "sketch", "aria-hidden": "true" });
    host.append(this.svg);
    this.svg.addEventListener("pointerdown", (event) => this.down(event));
    this.svg.addEventListener("pointermove", (event) => this.move(event));
    this.svg.addEventListener("pointerup", (event) => this.up(event));
    this.svg.addEventListener("pointercancel", (event) => this.up(event));
    new ResizeObserver(() => this.render()).observe(host);
  }

  /** Replace the shapes, keeping the selection if it still exists */
  set(shapes) {
    this.shapes = shapes;
    if (this.selected >= shapes.length) this.select(-1);
    this.render();
  }

  setEditable(editable) {
    this.editable = editable;
    this.svg.classList.toggle("is-editable", editable);
    if (!editable) this.select(-1);
    this.render();
  }

  setTool(tool) {
    this.tool = tool;
    this.svg.dataset.tool = tool;
  }

  select(index) {
    if (index === this.selected) return;
    this.selected = index;
    this.render();
    this.onSelect(index);
  }

  /** Delete the selected shape */
  removeSelected() {
    if (this.selected < 0) return false;
    this.shapes.splice(this.selected, 1);
    this.selected = -1;
    this.render();
    this.onSelect(-1);
    this.onChange(this.shapes);
    return true;
  }

  /** Picture size in CSS pixels */
  size() {
    return [this.host.clientWidth || 1, this.host.clientHeight || 1];
  }

  /** A pointer event's position as fractions of the picture */
  at(event) {
    const box = this.host.getBoundingClientRect();
    const clamp = (v) => Math.max(0, Math.min(1, v));
    return [
      clamp((event.clientX - box.left) / box.width),
      clamp((event.clientY - box.top) / box.height),
    ];
  }

  down(event) {
    if (!this.editable || event.button > 0) return;
    const p = this.at(event);
    const target = event.target.closest("[data-index], [data-handle]");
    let drag = null;
    if (target?.dataset.handle != null && this.selected >= 0) {
      const shape = this.shapes[this.selected];
      drag = { mode: "resize", handle: Number(target.dataset.handle), shape };
      if (shape.kind !== "arrow") {
        // The corner across from the handle stays put
        const [x0, y0, x1, y1] = sketchBounds(shape.points);
        const corners = [
          [x0, y0],
          [x1, y0],
          [x1, y1],
          [x0, y1],
        ];
        drag.anchor = corners[(drag.handle + 2) % 4];
      }
    } else if (target?.dataset.index != null) {
      const index = Number(target.dataset.index);
      this.select(index);
      drag = {
        mode: "move",
        shape: this.shapes[index],
        start: p,
        points: this.shapes[index].points.map((q) => [...q]),
      };
    } else if (this.tool !== "select" && this.onDraw() !== false) {
      const shape = {
        kind: this.tool,
        points: this.tool === "freehand" ? [p] : [p, p],
        color: this.color,
      };
      this.shapes.push(shape);
      this.selected = this.shapes.length - 1;
      drag = { mode: "draw", shape };
    } else {
      this.select(-1);
      return;
    }
    drag.changed = false;
    this.drag = drag;
    this.svg.setPointerCapture(event.pointerId);
    event.preventDefault();
    this.render();
  }

  move(event) {
    const { drag } = this;
    if (!drag) return;
    const p = this.at(event);
    const { shape } = drag;
    if (drag.mode === "draw") {
      if (shape.kind === "freehand") {
        const last = shape.points[shape.points.length - 1];
        if (Math.hypot(p[0] - last[0], p[1] - last[1]) < SKETCH_MIN_STEP)
          return;
        shape.points.push(p);
      } else {
        shape.points[1] = p;
      }
    } else if (drag.mode === "move") {
      // Keep the whole shape on the picture
      const [x0, y0, x1, y1] = sketchBounds(drag.points);
      const dx = Math.max(-x0, Math.min(1 - x1, p[0] - drag.start[0]));
      const dy = Math.max(-y0, Math.min(1 - y1, p[1] - drag.start[1]));
      shape.points = drag.points.map(([x, y]) => [x + dx, y + dy]);
    } else if (shape.kind === "arrow") {
      shape.points[drag.handle] = p;
    } else {
      shape.points = [drag.anchor, p];
    }
    drag.changed = true;
    this.render();
  }

  up() {
    const { drag } = this;
    if (!drag) return;
    this.drag = null;
    if (drag.mode === "draw") {
      const [x0, y0, x1, y1] = sketchBounds(drag.shape.points);
      const tiny =
        drag.shape.kind === "freehand"
          ? drag.shape.points.length < 2
          : x1 - x0 < SKETCH_MIN_SIZE && y1 - y0 < SKETCH_MIN_SIZE;
      if (tiny) {
        this.shapes.pop();
        this.selected = -1;
        this.render();
        this.onSelect(-1);
        return;
      }
      this.onSelect(this.selected);
    }
    if (drag.changed) {
      if (drag.mode !== "draw") drag.shape.edited = true;
      this.render();
      this.onChange(this.shapes);
    }
  }

  /** Draw every shape, and the selected one's handles */
  render() {
    const [width, height] = this.size();
    const svg = this.svg;
    svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
    svg.replaceChildren();
    const px = ([x, y]) => [x * width, y * height];
    this.shapes.forEach((shape, index) => {
      const g = svgEl("g", { class: "sk-shape" });
      g.style.setProperty("--sk-color", shape.color ?? this.color);
      if (shape.dashed) g.classList.add("is-dashed");
      if (shape.faded) g.classList.add("is-faded");
      if (index === this.selected) g.classList.add("is-selected");
      if (!shape.locked && this.editable) g.dataset.index = index;
      else g.classList.add("is-locked");
      const points = shape.points.map(px);
      if (shape.kind === "rect" || shape.kind === "ellipse") {
        const [x0, y0, x1, y1] = sketchBounds(points);
        g.append(
          shape.kind === "rect"
            ? svgEl("rect", {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
              })
            : svgEl("ellipse", {
                cx: (x0 + x1) / 2,
                cy: (y0 + y1) / 2,
                rx: (x1 - x0) / 2,
                ry: (y1 - y0) / 2,
              }),
        );
        if (shape.tag) g.append(...sketchTag(shape.tag, x0, y0));
      } else if (shape.kind === "arrow") {
        const [[x0, y0], [x1, y1]] = points;
        const angle = Math.atan2(y1 - y0, x1 - x0);
        const head = 14;
        const wing = (side) => [
          x1 - head * Math.cos(angle + side * 0.45),
          y1 - head * Math.sin(angle + side * 0.45),
        ];
        g.append(
          svgEl("line", { class: "sk-hit", x1: x0, y1: y0, x2: x1, y2: y1 }),
          svgEl("line", { x1: x0, y1: y0, x2: x1, y2: y1 }),
          svgEl("polyline", {
            points: [wing(1), [x1, y1], wing(-1)].join(" "),
          }),
        );
      } else {
        const line = points.map((q) => q.join(",")).join(" ");
        g.append(
          svgEl("polyline", { class: "sk-hit", points: line }),
          svgEl("polyline", { points: line }),
        );
      }
      svg.append(g);
    });
    const shape = this.shapes[this.selected];
    if (!shape || !this.editable || shape.locked || shape.kind === "freehand")
      return;
    let handles;
    if (shape.kind === "arrow") {
      handles = shape.points.map(px);
    } else {
      const [x0, y0, x1, y1] = sketchBounds(shape.points.map(px));
      handles = [
        [x0, y0],
        [x1, y0],
        [x1, y1],
        [x0, y1],
      ];
    }
    handles.forEach(([x, y], i) => {
      svg.append(
        svgEl("rect", {
          class: "sk-handle",
          "data-handle": i,
          x: x - 5,
          y: y - 5,
          width: 10,
          height: 10,
        }),
      );
    });
  }
}

/** [left, top, right, bottom] of points */
function sketchBounds(points) {
  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  return [Math.min(...xs), Math.min(...ys), Math.max(...xs), Math.max(...ys)];
}

/** A caption on a dark band at a box's top-left corner */
function sketchTag(text, x, y) {
  const label = svgEl("text", { class: "sk-tag", x: x + 4, y: y - 5 });
  label.textContent = text;
  const band = svgEl("rect", {
    class: "sk-tag-band",
    x,
    y: y - 18,
    width: 8 + 6.6 * text.length,
    height: 18,
  });
  // Near the top edge the caption goes inside the box
  if (y < 18) {
    band.setAttribute("y", y);
    label.setAttribute("y", y + 13);
  }
  return [band, label];
}
