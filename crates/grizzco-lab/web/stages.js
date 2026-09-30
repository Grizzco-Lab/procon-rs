// Salmon Run stages (Splatoon 3) and links to Gungee's community map
// viewers at salmon-learn-nw.gungee.jp (Salmon Learn NW): a 2D viewer for
// every stage and a 3D one for some. The maps are Gungee's work, which the
// community uses as its shared reference: we link to them (in a new tab)
// and may show a stage's top-down map for reference, always credited to him
// with a link and thanks; the studio fetches such a map once into its local
// cache, and none is ever kept in this repository.
//
// The stage keys come from Gungee's lists (/maplist/ and /maplist3d/, read
// once in 2026-09): `/map/?stage=<key>&tide=Mid` and `/map3d/?stage=…`. Our
// ids are the glossary's (crates/cuttlefish/glossary.toml), as a review keeps
// them in its `stage`.
//
//   const map = new StageMap(el, (id) => …); // a picker and the links
//   new StageMap(el, onPick, { picture: true }) // and the map, credited
//   map.set(id);                              // show a stage, or none ("")
//   stageFromText("Spawning Grounds 180")     // "spawning-grounds"
//   await stageOfVideo({ kind, ref, title })  // from its review, or the title
//
// Runs after i18n.js and uses t().
"use strict";

/** Gungee's community tools */
const GUNGEE = "https://salmon-learn-nw.gungee.jp";

/** Stages by glossary id: the English name and
 * Gungee's key; `map3d` when his 3D viewer has the stage */
const STAGES = [
  {
    id: "spawning-grounds",
    en: "Spawning Grounds",
    gungee: "Shakeup",
    map3d: false,
  },
  {
    id: "sockeye-station",
    en: "Sockeye Station",
    gungee: "Shakespiral",
    map3d: false,
  },
  {
    id: "gone-fission-hydroplant",
    en: "Gone Fission Hydroplant",
    gungee: "Shakedent",
    map3d: true,
  },
  {
    id: "marooners-bay",
    en: "Marooner's Bay",
    gungee: "Shakeship",
    map3d: true,
  },
  {
    id: "jammin-salmon-junction",
    en: "Jammin' Salmon Junction",
    gungee: "Shakehighway",
    map3d: true,
  },
  {
    id: "salmonid-smokeyard",
    en: "Salmonid Smokeyard",
    gungee: "Shakelift",
    map3d: false,
  },
  {
    id: "bonerattle-arena",
    en: "Bonerattle Arena",
    gungee: "Shakerail",
    map3d: false,
  },
];

const stageById = (id) => STAGES.find((s) => s.id === id) ?? null;

/** The stage's name in the page's language (English when it has none) */
const stageName = (stage) =>
  I18N[i18nLang()]?.[`stage.${stage.id}`] ?? stage.en;

/** The stage a text names (a video title, say), by its English or
 * Chinese name; null when none or several */
function stageFromText(text) {
  if (!text) return null;
  const lower = text.toLowerCase().replace(/[’`]/g, "'");
  const found = STAGES.filter((s) =>
    [s.en.toLowerCase(), I18N.zh?.[`stage.${s.id}`]].some(
      (name) => name && lower.includes(name),
    ),
  );
  return found.length === 1 ? found[0].id : null;
}

/** Gungee's 2D viewer of a stage at a tide */
const gungeeMapUrl = (stage, tide = "Mid") =>
  `${GUNGEE}/map/?stage=${stage.gungee}&tide=${tide}`;

/** Links to Gungee's 2D and (when there is one) 3D map of a stage */
function stageMapLinks(id, tide = "Mid") {
  const stage = stageById(id);
  if (!stage) return "";
  const link = (url, text, title) =>
    `<a class="stage-link" href="${url}" target="_blank" rel="noopener" title="${escapeHtml(title)}">${text} ↗</a>`;
  return [
    `<span class="stage-links-label">${escapeHtml(t("stage.map"))}</span>`,
    link(gungeeMapUrl(stage, tide), "2D", t("stage.map2d")),
    stage.map3d
      ? link(
          `${GUNGEE}/map3d/?stage=${stage.gungee}&tide=${tide}`,
          "3D",
          t("stage.map3d"),
        )
      : "",
  ].join(" ");
}

/** A stage picker and its map links; `onPick(id)` hears the user's choice
 * (no picker without it: the links alone). With `picture`, also Gungee's
 * top-down map of the stage at a tide, credited to him under it: fetched
 * once by the studio into its cache (`/api/cuttlefish/stage-map`), for
 * reference only */
class StageMap {
  constructor(el, onPick, { picture = false } = {}) {
    this.el = el;
    this.id = "";
    this.tide = "Mid";
    el.classList.add("stage-map");
    this.select = null;
    if (onPick) {
      this.select = document.createElement("select");
      this.select.className = "select stage-pick";
      this.select.onchange = () => {
        this.set(this.select.value);
        onPick(this.id);
      };
      el.append(this.select);
    }
    this.links = document.createElement("span");
    this.links.className = "stage-links";
    el.append(this.links);
    this.figure = null;
    if (picture) {
      this.figure = document.createElement("figure");
      this.figure.className = "stage-figure";
      this.figure.innerHTML = `
        <div class="stage-tides" role="group"></div>
        <canvas class="stage-img is-missing" role="img"></canvas>
        <figcaption class="stage-credit"></figcaption>`;
      this.figure.querySelector(".stage-tides").onclick = (event) => {
        const button = event.target.closest("[data-tide]");
        if (!button) return;
        this.tide = button.dataset.tide;
        this.draw();
      };
      el.append(this.figure);
    }
    this.draw();
    window.addEventListener("lang-change", () => this.draw());
  }

  /** Show stage `id` ("" or null for none) */
  set(id) {
    this.id = stageById(id) ? id : "";
    this.draw();
  }

  draw() {
    const { select, figure } = this;
    const stage = stageById(this.id);
    if (select) {
      select.setAttribute("aria-label", t("stage.pick"));
      select.title = t("stage.pickTitle");
      select.replaceChildren(
        new Option(t("stage.unknown"), ""),
        ...STAGES.map((s) => new Option(stageName(s), s.id)),
      );
      select.value = this.id;
    }
    this.links.innerHTML = stageMapLinks(this.id, this.tide);
    this.el.hidden = !select && !stage;
    if (!figure) return;
    figure.hidden = !stage;
    if (!stage) return;
    const tides = figure.querySelector(".stage-tides");
    tides.setAttribute("aria-label", t("stage.tide"));
    tides.innerHTML = ["Low", "Mid", "High"]
      .map(
        (tide) =>
          `<button type="button" class="mode-toggle" data-tide="${tide}" aria-pressed="${tide === this.tide}">${escapeHtml(t(`stage.tide.${tide}`))}</button>`,
      )
      .join("");
    const canvas = figure.querySelector("canvas");
    canvas.setAttribute(
      "aria-label",
      t("stage.mapAlt", { stage: stageName(stage) }),
    );
    this.drawPicture(
      `/api/cuttlefish/stage-map?stage=${stage.gungee}&tide=${this.tide}`,
    );
    figure.querySelector("figcaption").innerHTML = t("stage.credit", {
      link: `<a href="${gungeeMapUrl(stage, this.tide)}" target="_blank" rel="noopener">salmon-learn-nw.gungee.jp ↗</a>`,
    });
  }

  /** Draw the map from `src`, cut to the stage: the picture is a large
   * square, mostly empty around it */
  drawPicture(src) {
    if (this.src === src) return;
    this.src = src;
    const canvas = this.figure.querySelector("canvas");
    canvas.classList.add("is-missing");
    const img = new Image();
    img.onload = () => {
      if (this.src !== src) return;
      const [x, y, w, h] = opaqueBox(img);
      const scale = Math.min(1, STAGE_MAP_PX / Math.max(w, h));
      canvas.width = Math.round(w * scale);
      canvas.height = Math.round(h * scale);
      canvas
        .getContext("2d")
        .drawImage(img, x, y, w, h, 0, 0, canvas.width, canvas.height);
      canvas.classList.remove("is-missing");
    };
    img.src = src;
  }
}

/** Largest side of a drawn stage map, in pixels */
const STAGE_MAP_PX = 1024;

/** The part of a picture that is not transparent, with a small margin, as
 * [x, y, w, h] in its pixels; found on a small copy */
function opaqueBox(img) {
  const size = 256;
  const probe = document.createElement("canvas");
  probe.width = size;
  probe.height = size;
  const ctx = probe.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(img, 0, 0, size, size);
  const { data } = ctx.getImageData(0, 0, size, size);
  let [x0, y0, x1, y1] = [size, size, -1, -1];
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (data[(y * size + x) * 4 + 3] < 16) continue;
      x0 = Math.min(x0, x);
      x1 = Math.max(x1, x);
      y0 = Math.min(y0, y);
      y1 = Math.max(y1, y);
    }
  }
  const { naturalWidth: W, naturalHeight: H } = img;
  if (x1 < 0) return [0, 0, W, H];
  const margin = 4;
  const left = Math.max(0, x0 - margin) / size;
  const top = Math.max(0, y0 - margin) / size;
  const right = Math.min(size, x1 + 1 + margin) / size;
  const bottom = Math.min(size, y1 + 1 + margin) / size;
  return [left * W, top * H, (right - left) * W, (bottom - top) * H];
}

/** The stage of a video: the one picked on its review (`r`, or a review of
 * the same `kind` and `ref`, such as a session's `<session>/<file>`), else
 * the one its `title` names; "" when unknown */
async function stageOfVideo({ r, kind, ref, title }) {
  try {
    const response = await fetch("/api/cuttlefish/reviews");
    if (response.ok) {
      const { reviews } = await response.json();
      const review = reviews.find(
        (x) =>
          x.stage &&
          (r ? x.id === r : x.video?.kind === kind && x.video?.ref === ref),
      );
      if (review) return review.stage;
    }
  } catch {
    // The title may still tell
  }
  return stageFromText(title) ?? "";
}
