// 3D Pro Controller for the dashboard, drawn with three.js from a CDN.
//
// The body is the SVG view's traced outline extruded into a slab, its face
// painted from the same paths, with buttons, sticks, bumpers and triggers as
// real meshes. Without WebGL or the CDN this module simply never announces
// itself and the dashboard keeps the flat SVG view.

import * as THREE from "https://cdn.jsdelivr.net/npm/three@0.186.1/+esm";

const svg = document.getElementById("procon");
const canvas = document.getElementById("procon-3d");
const stage = canvas.parentElement;

/** Body thickness in SVG units (the real controller is about 60 mm deep) */
const DEPTH = 150;
/** The SVG view box, which the face texture covers */
const VIEW = { x: -40, y: -60, width: 980, height: 740 };
/** SVG point to model coordinates: centered, y up */
const at = (x, y) => new THREE.Vector2(x - 450, 320 - y);

const renderer = new THREE.WebGLRenderer({
  canvas,
  antialias: true,
  alpha: true,
});
renderer.setPixelRatio(Math.min(2, window.devicePixelRatio));
const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(28, 16 / 9, 10, 10000);
camera.position.set(0, 0, 2300);
scene.add(new THREE.HemisphereLight(0xffffff, 0x303038, 2.2));
const sun = new THREE.DirectionalLight(0xffffff, 2.4);
sun.position.set(-600, 900, 1400);
scene.add(sun);

/** Everything that tilts with the controller */
const model = new THREE.Group();
scene.add(model);

/** Read the SVG view's theme colors, so both views match */
function palette() {
  const style = getComputedStyle(svg);
  const get = (name, fallback) =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    face: get("--pc-face-hi", "#3c3e43"),
    faceLow: get("--pc-face-lo", "#202124"),
    gripL: get("--pc-grip-l-hi", "#2b2c30"),
    gripR: get("--pc-grip-r-hi", "#2b2c30"),
    seam: get("--pc-seam", "rgba(0,0,0,.5)"),
    well: get("--pc-well-lo", "#0b0c0d"),
    button: get("--pc-btn-hi", "#3d3f45"),
    buttonLow: get("--pc-btn-lo", "#121315"),
    cap: get("--pc-cap-hi", "#4b4d53"),
    ink: get("--pc-ink", "#d7d9de"),
    press: get("--pc-press", "#4d9bff"),
    pressInk: get("--pc-press-ink", "#ffffff"),
    sideL: get("--pc-side-l", "#26272b"),
    sideR: get("--pc-side-r", "#26272b"),
    bumper: get("--pc-bumper", "#111214"),
    line: get("--pc-line", ""),
  };
}

/** Points along an SVG path element, in model coordinates */
function sample(path, count, transform = (x, y) => [x, y]) {
  const length = path.getTotalLength();
  const points = [];
  for (let i = 0; i < count; i++) {
    const p = path.getPointAtLength((length * i) / count);
    const [x, y] = transform(p.x, p.y);
    points.push(at(x, y));
  }
  return points;
}

/** The front face artwork: plate, grips and seams from the SVG paths */
function paintFace(colors) {
  const scale = 2;
  const face = document.createElement("canvas");
  face.width = VIEW.width * scale;
  face.height = VIEW.height * scale;
  const ctx = face.getContext("2d");
  ctx.scale(scale, scale);
  ctx.translate(-VIEW.x, -VIEW.y);

  const shape = new Path2D(svg.querySelector("#pc-shape").getAttribute("d"));
  const gradient = ctx.createLinearGradient(0, 0, 0, 640);
  gradient.addColorStop(0, colors.face);
  gradient.addColorStop(1, colors.faceLow);
  ctx.fillStyle = gradient;
  ctx.fill(shape);
  ctx.save();
  ctx.clip(shape);
  for (const [id, color] of [
    ["#pc-grip-l-shape", colors.gripL],
    ["#pc-grip-r-shape", colors.gripR],
  ]) {
    ctx.fillStyle = color;
    ctx.fill(new Path2D(svg.querySelector(id).getAttribute("d")));
  }
  ctx.restore();
  ctx.strokeStyle = colors.seam;
  ctx.lineWidth = 2;
  ctx.stroke(new Path2D(svg.querySelector(".pc-seam").getAttribute("d")));

  const texture = new THREE.CanvasTexture(face);
  texture.colorSpace = THREE.SRGBColorSpace;
  // Extruded caps use model coordinates as UVs; map them onto the view box
  texture.repeat.set(1 / VIEW.width, 1 / VIEW.height);
  texture.offset.set(
    (450 - VIEW.x) / VIEW.width,
    (VIEW.y + VIEW.height - 320) / VIEW.height,
  );
  return texture;
}

/** A round button top with its label, drawn on a canvas */
function capTexture(background, ink, draw) {
  const size = 128;
  const cap = document.createElement("canvas");
  cap.width = cap.height = size;
  const ctx = cap.getContext("2d");
  ctx.fillStyle = background;
  ctx.fillRect(0, 0, size, size);
  ctx.fillStyle = ctx.strokeStyle = ink;
  ctx.translate(size / 2, size / 2);
  draw(ctx);
  const texture = new THREE.CanvasTexture(cap);
  texture.colorSpace = THREE.SRGBColorSpace;
  // Cylinder caps map textures a quarter turn off; turn it upright
  texture.center.set(0.5, 0.5);
  texture.rotation = Math.PI / 2;
  return texture;
}

const letter = (text, px) => (ctx) => {
  ctx.font = `600 ${px}px Inter, system-ui, sans-serif`;
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText(text, 0, px * 0.05);
};
const bar = (vertical) => (ctx) => {
  ctx.fillRect(-26, -7, 52, 14);
  if (vertical) ctx.fillRect(-7, -26, 14, 52);
};
const ring = (ctx) => {
  ctx.lineWidth = 12;
  ctx.beginPath();
  ctx.arc(0, 0, 26, 0, Math.PI * 2);
  ctx.stroke();
};
const house = (ctx) => {
  ctx.beginPath();
  ctx.moveTo(0, -30);
  ctx.lineTo(32, 0);
  ctx.lineTo(20, 0);
  ctx.lineTo(20, 26);
  ctx.lineTo(-20, 26);
  ctx.lineTo(-20, 0);
  ctx.lineTo(-32, 0);
  ctx.closePath();
  ctx.fill();
};

/** Buttons by dashboard name: meshes to recolor and how far they sink */
const buttons = new Map();
const sticks = {};
let built = null;

function material(color, extra = {}) {
  return new THREE.MeshStandardMaterial({
    color,
    roughness: 0.55,
    metalness: 0.05,
    ...extra,
  });
}

/** A pressable button: a short cylinder or box with a painted top */
function addButton(name, x, y, { radius = 0, size = 0, height, draw }, colors) {
  const geometry = radius
    ? new THREE.CylinderGeometry(radius, radius, height, 48).rotateX(
        Math.PI / 2,
      )
    : new THREE.BoxGeometry(size, size, height);
  const side = material(colors.buttonLow);
  const top = material("#ffffff", {
    map: capTexture(colors.button, colors.ink, draw),
  });
  const pressedTop = capTexture(colors.press, colors.pressInk, draw);
  // Cylinder groups: side, top, bottom; box groups: +x, -x, +y, -y, +z, -z
  const materials = radius
    ? [side, top, side]
    : [side, side, side, side, top, side];
  const mesh = new THREE.Mesh(geometry, materials);
  const position = at(x, y);
  mesh.position.set(position.x, position.y, height / 2);
  model.add(mesh);
  buttons.set(name, {
    meshes: [mesh],
    top,
    normal: top.map,
    pressed: pressedTop,
    side,
    rest: height / 2,
  });
}

/** A band along a stroked SVG path, like the bumpers and triggers */
function addBand(name, path, radius, z, color, lift = 0) {
  const points = sample(path, 60).map(
    (p) => new THREE.Vector3(p.x, p.y + lift, z),
  );
  const curve = new THREE.CatmullRomCurve3(points);
  const mesh = new THREE.Mesh(
    new THREE.TubeGeometry(curve, 80, radius, 16, false),
    material(color),
  );
  model.add(mesh);
  buttons.set(name, { meshes: [mesh], band: mesh.material, color });
}

function build() {
  const colors = palette();
  for (const child of [...model.children]) model.remove(child);
  buttons.clear();

  // Body: the outline extruded, front face at z = 0
  const outline = new THREE.Shape(sample(svg.querySelector("#pc-shape"), 400));
  const body = new THREE.ExtrudeGeometry(outline, {
    depth: DEPTH,
    bevelEnabled: true,
    bevelThickness: 18,
    bevelSize: 16,
    bevelSegments: 6,
    curveSegments: 1,
  });
  // Put the front face, bevel included, at z = 0
  body.translate(0, 0, -DEPTH - 18);
  const faceMaterial = material("#ffffff", {
    map: paintFace(colors),
    roughness: 0.45,
  });
  model.add(
    new THREE.Mesh(body, [
      faceMaterial,
      material(colors.faceLow, { roughness: 0.7 }),
    ]),
  );

  // Shoulders follow the path just outside the outline: bumpers toward the
  // front, triggers further back and higher, as on the real controller
  const shoulder = (name) =>
    svg.querySelector(`.pc-trigger[data-btn="${name}"]`);
  addBand("l", shoulder("zl"), 16, -DEPTH * 0.3, colors.bumper);
  addBand("r", shoulder("zr"), 16, -DEPTH * 0.3, colors.bumper);
  addBand("zl", shoulder("zl"), 22, -DEPTH * 0.8, colors.sideL, 14);
  addBand("zr", shoulder("zr"), 22, -DEPTH * 0.8, colors.sideR, 14);

  // Face buttons and system buttons
  for (const [name, x, y] of [
    ["x", 718, 112],
    ["y", 642, 180],
    ["a", 794, 180],
    ["b", 718, 248],
  ]) {
    addButton(
      name,
      x,
      y,
      { radius: 33, height: 22, draw: letter(name.toUpperCase(), 64) },
      colors,
    );
  }
  addButton(
    "minus",
    334,
    108,
    { radius: 19, height: 12, draw: bar(false) },
    colors,
  );
  addButton(
    "plus",
    566,
    108,
    { radius: 19, height: 12, draw: bar(true) },
    colors,
  );
  addButton("home", 520, 180, { radius: 21, height: 12, draw: house }, colors);
  addButton("capture", 380, 180, { size: 34, height: 12, draw: ring }, colors);

  // D-pad: a center piece and four arms that light up on their own
  const dpad = at(302, 309);
  const center = new THREE.Mesh(
    new THREE.BoxGeometry(48, 48, 16),
    material(colors.button),
  );
  center.position.set(dpad.x, dpad.y, 8);
  model.add(center);
  for (const [name, dx, dy] of [
    ["up", 0, 1],
    ["down", 0, -1],
    ["left", -1, 0],
    ["right", 1, 0],
  ]) {
    const arm = new THREE.Mesh(
      new THREE.BoxGeometry(48, 48, 16),
      material(colors.button),
    );
    arm.position.set(dpad.x + dx * 48, dpad.y + dy * 48, 8);
    model.add(arm);
    buttons.set(name, {
      meshes: [arm],
      band: arm.material,
      color: colors.button,
      rest: 8,
    });
  }

  // Sticks: a dark well, then a cap on a short post that tilts with the stick
  for (const [side, x, y] of [
    ["l", 182, 180],
    ["r", 598, 309],
  ]) {
    const position = at(x, y);
    const well = new THREE.Mesh(
      new THREE.CylinderGeometry(78, 78, 2, 64).rotateX(Math.PI / 2),
      material(colors.well, { roughness: 0.9 }),
    );
    well.position.set(position.x, position.y, 1);
    model.add(well);

    const pivot = new THREE.Group();
    pivot.position.set(position.x, position.y, 0);
    const post = new THREE.Mesh(
      new THREE.CylinderGeometry(22, 26, 40, 32).rotateX(Math.PI / 2),
      material(colors.buttonLow),
    );
    post.position.z = 20;
    const cap = new THREE.Mesh(
      new THREE.CylinderGeometry(56, 58, 16, 64).rotateX(Math.PI / 2),
      material(colors.buttonLow, { roughness: 0.8 }),
    );
    cap.position.z = 44;
    const top = new THREE.Mesh(
      new THREE.CylinderGeometry(40, 40, 3, 64).rotateX(Math.PI / 2),
      material(colors.cap, { roughness: 0.6 }),
    );
    top.position.z = 53;
    pivot.add(post, cap, top);
    model.add(pivot);
    sticks[side] = pivot;
    buttons.set(`${side}_stick`, {
      meshes: [cap],
      band: cap.material,
      color: colors.buttonLow,
    });
  }

  // Telemetry draws the controller as lines, like its SVG wireframe
  if (colors.line) {
    const lineMaterial = new THREE.LineBasicMaterial({ color: colors.line });
    model.traverse((object) => {
      if (!object.isMesh) return;
      object.add(
        new THREE.LineSegments(
          new THREE.EdgesGeometry(object.geometry, 30),
          lineMaterial,
        ),
      );
      for (const m of [object.material].flat()) {
        m.transparent = true;
        m.opacity = 0.08;
      }
    });
  }

  built = colors;
}

/** Show `state`'s buttons and sticks, turned by a CSS-frame (w, x, y, z) quaternion */
function update(state, [w, x, y, z]) {
  if (!built) return;
  const colors = built;
  for (const [name, button] of buttons) {
    const on = Boolean(state.buttons[name]);
    if (button.top) {
      button.top.map = on ? button.pressed : button.normal;
      button.top.needsUpdate = true;
    }
    if (button.band) button.band.color.set(on ? colors.press : button.color);
    if (button.rest !== undefined) {
      for (const mesh of button.meshes)
        mesh.position.z = on ? button.rest - 5 : button.rest;
    }
  }
  for (const [side, stick] of [
    ["l", state.left_stick],
    ["r", state.right_stick],
  ]) {
    const dx = Math.max(-1, Math.min(1, (stick.x - 2048) / 2048));
    const dy = Math.max(-1, Math.min(1, (stick.y - 2048) / 2048));
    sticks[side].rotation.set(-dy * 0.45, dx * 0.45, 0);
  }
  // CSS has y pointing down; mirror the rotation into three.js's y-up frame
  model.quaternion.set(-x, y, -z, w);
}

function resize() {
  const { clientWidth: width, clientHeight: height } = stage;
  if (!width || !height) return;
  renderer.setSize(width, height, false);
  camera.aspect = width / height;
  // Fit the controller's width, or its height on narrow stages
  const fit = Math.max(1300 / camera.aspect, 900);
  camera.position.z =
    fit / (2 * Math.tan(THREE.MathUtils.degToRad(camera.fov / 2)));
  camera.updateProjectionMatrix();
}

build();
new ResizeObserver(resize).observe(stage);
// Theme changes recolor the model
new MutationObserver(build).observe(document.documentElement, {
  attributes: true,
  attributeFilter: ["data-theme"],
});
renderer.setAnimationLoop(() => {
  if (!canvas.hidden) renderer.render(scene, camera);
});

window.procon3d = { update };
window.dispatchEvent(new Event("procon3d-ready"));
