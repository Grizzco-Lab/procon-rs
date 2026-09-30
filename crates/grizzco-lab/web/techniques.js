// Technique markers: spans of a recording labelled as what was practised,
// for labelled examples and as a reminder of what to record. The Studio's
// Techniques panel lists the items in groups (movement, egg handling,
// Salmon Run's weapon pool with the Grizzco weapons last under a head of
// their own, the sub weapon, the specials Salmon Run hands out), each
// folded or open with how many of its items are recorded; while recording
// it marks spans (started and stopped by hand, or the last few seconds)
// through /api/command. They are saved in the session's session.json as
// `markers` (host Unix ms, the frames' clock) with the item's id and kind
// (`technique`, `weapon` or `special`), so examples can be counted per
// weapon and special. The weapons and specials, with their names and
// pictures, come from Lean's datamine in the Cuttlefish store
// (GET /api/cuttlefish/game-items; each picture fetched once by the lab into
// its cache, game-icon), credited to Lean where they show. The Inkspector
// shows and edits the markers (inspect.js) and the Pedia lists them under
// the item's entry (pedia.js), both through /api/inspect/markers. Runs after
// app.js and player.js and uses their helpers ($, sendCommand, recorder,
// studioShown, escapeHtml, showError, formatClock) and those of i18n.js (t,
// I18N, i18nLang).
"use strict";

/** Lean's site, credited under the items from his data */
const LEANNY = "https://leanny.github.io/";

/** The panel's groups in order: `kind` is what their markers are, `data`
 * when their items come from Lean's data, `adds` when techniques added by
 * hand may join them */
const TECH_GROUPS = [
  { id: "movement", kind: "technique", adds: true },
  { id: "eggs", kind: "technique", adds: true },
  { id: "weapon", kind: "weapon", data: true },
  { id: "sub", kind: "technique", adds: true },
  { id: "special", kind: "special", data: true },
];

/**
 * The usual techniques and the sub weapon: `id` (markers keep it as their
 * item; the Chinese name is `tech.zh.<id>` in i18n-zh.js), the group, the
 * English name markers get as their label, the Pedia term id where the
 * glossary has one, and a picture on Lean's site. Techniques added in the
 * panel join their group, saved in the dashboard's state file; the weapons
 * and specials come from Lean's data.
 */
const TECHNIQUES = [
  {
    id: "squid-roll",
    group: "movement",
    label: "Squid roll",
    term: "squid-roll",
  },
  {
    id: "sub-strafe",
    group: "movement",
    label: "Sub strafe (inertia cancel)",
    term: "inertia-cancel",
  },
  {
    id: "main-strafe",
    group: "movement",
    label: "Main strafe",
    term: "main-strafe",
  },
  { id: "wall-climb", group: "movement", label: "Fast wall climb" },
  { id: "hop", group: "movement", label: "Small hop / big jump" },
  {
    id: "egg-grab",
    group: "eggs",
    label: "Grab eggs without cancelling ink recovery",
  },
  { id: "egg-throw", group: "eggs", label: "Egg throw", term: "egg-toss" },
  {
    id: "basket-run",
    group: "eggs",
    label: "Egg runs at the basket",
    term: "egg-run",
  },
  {
    id: "splat-bomb",
    group: "sub",
    label: "Splat Bomb throw",
    term: "splat-bomb",
    icon: "subspe/Wsb_Bomb_Splash00.png",
  },
];

/** Search results shown at most */
const TECH_FOUND = 12;

/** Lean's weapons and specials (GET /api/cuttlefish/game-items) as items
 * of the list: null until read, `failed` when the read failed */
const gameItems = { list: null, loading: null, failed: "" };

/** Read Lean's weapons and specials once (again after a failure, or while
 * the store has none, or `again`); the panel and the Inkspector's markers
 * redraw when they come. The lab answers at once with the items it kept,
 * `refreshing` while it looks at their files again: then they are read
 * once more a moment later, as they may have changed, and the Inkspector's
 * markers (whose inputs a redraw would reset) are drawn again only if they
 * did. */
function loadGameItems(again = false) {
  if ((gameItems.list?.length && !again) || gameItems.loading) return;
  let changed = true;
  gameItems.loading = fetch("/api/cuttlefish/game-items")
    .then(async (response) => {
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      // In the lab's order: the weapons, the Grizzco ones last, then the
      // specials
      const list = data.items.map((item) => ({
        id: item.key,
        // The groups of the weapons and specials are named as their kinds
        group: item.kind,
        label: item.names.en ?? item.key,
        zh: item.names.zh ?? "",
        ja: item.names.ja ?? "",
        term: item.term ?? null,
        icon: item.icon,
        grizzco: item.grizzco,
        search: item.search,
      }));
      changed = JSON.stringify(list) !== JSON.stringify(gameItems.list);
      gameItems.list = list;
      gameItems.failed = "";
      if (data.refreshing) setTimeout(() => loadGameItems(true), 2000);
    })
    .catch((error) => {
      // Aborted (the Studio was left): read again when it is shown
      if (!isAbort(error)) gameItems.failed = error.message;
    })
    .finally(() => {
      gameItems.loading = null;
      drawTechniques();
      if (changed) window.dispatchEvent(new Event("tech-items"));
    });
}

/** An item's key: its id, or an added technique's name */
const itemKey = (tech) => tech.id ?? tech.label;

/** The kind of an item's markers: its group's */
const itemKind = (tech) =>
  TECH_GROUPS.find((group) => group.id === tech.group)?.kind ?? "technique";

/** A technique's Chinese name, if it has one */
function techniqueZh(tech) {
  return tech.zh || (tech.id && I18N.zh?.[`tech.zh.${tech.id}`]) || "";
}

/** A technique's names as [shown first, the other language], for the page's language */
function techniqueNames(tech) {
  const zh = techniqueZh(tech);
  if (!zh) return [tech.label, ""];
  return i18nLang() === "zh" ? [zh, tech.label] : [tech.label, zh];
}

/** Whether a marker is an example of an item: by the item's id when the
 * marker has one, else (older markers, added techniques) by name or term */
function markerOf(marker, tech) {
  if (marker.item) return marker.item === tech.id;
  return (
    marker.label === tech.label ||
    Boolean(tech.term && marker.term === tech.term)
  );
}

/** A picture of Lean's site, fetched by the lab into its cache */
const iconUrl = (path) =>
  `/api/cuttlefish/game-icon?path=${encodeURIComponent(path)}`;

/** Every session's markers (GET /api/inspect/markers), kept a little while */
const allMarkers = { list: null, at: 0, loading: null };

/** Every session's markers, read again after 30 s or once `at` is reset */
function loadAllMarkers() {
  if (allMarkers.list && performance.now() - allMarkers.at < 30000)
    return Promise.resolve(allMarkers.list);
  if (allMarkers.loading) return allMarkers.loading;
  allMarkers.loading = fetch("/api/inspect/markers")
    .then(async (response) => {
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      allMarkers.list = data.markers;
      return data.markers;
    })
    .catch((error) => {
      // A failed read is not retried at once either; an aborted one (its
      // app was left) is, when asked for again
      if (isAbort(error)) allMarkers.at = 0;
      throw error;
    })
    .finally(() => {
      allMarkers.loading = null;
    });
  allMarkers.at = performance.now();
  return allMarkers.loading;
}

// ------------------------------------------------------------ Studio panel

const techPanel = {
  /** The key of the item picked, remembered in this browser */
  picked: null,
  /** "session": reps marked in this session; "all": the checklist */
  mode: "session",
  /** The open group ("" when all are folded), remembered too */
  group: "movement",
  /** The search typed, and which of its results Enter picks */
  query: "",
  hit: 0,
  /** The results as last drawn */
  found: [],
  /** The server's side: added techniques, open span, counts */
  status: { added: [], open: null, counts: {}, total: 0 },
  /** When the open span's elapsed time arrived */
  receivedAt: 0,
  /** The list's markup as last drawn, to redraw only on change, and the
   * group open then ("" for search results) */
  drawn: "",
  drawnGroup: "",
  /** Recorder state and marker total last seen, to reload the checklist */
  seen: "",
};

try {
  techPanel.picked = localStorage.getItem("procon-technique");
  techPanel.mode =
    localStorage.getItem("procon-technique-mode") === "all" ? "all" : "session";
  techPanel.group =
    localStorage.getItem("procon-technique-group") ?? techPanel.group;
} catch {
  // Storage may be refused; the first technique is picked
}

/** Remember a choice in this browser */
function techRemember(key, value) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // The choice holds until reload
  }
}

/** Every item: the usual techniques, the added ones in their groups, then
 * Lean's weapons and specials */
function techniques() {
  const added = techPanel.status.added.map((tech) => ({
    ...tech,
    group: TECH_GROUPS.some((g) => g.adds && g.id === tech.group)
      ? tech.group
      : "movement",
    added: true,
  }));
  return [...TECHNIQUES, ...added, ...(gameItems.list ?? [])];
}

/** The item picked, else the first */
function pickedTechnique() {
  const list = techniques();
  const key = techPanel.picked;
  return (
    list.find((tech) => itemKey(tech) === key) ??
    // Picked by name before items had ids
    list.find((tech) => tech.label === key) ??
    list[0]
  );
}

/** Pick an item; its group opens, so 1–9 carry on in it */
function pickTechnique(tech) {
  techPanel.picked = itemKey(tech);
  techRemember("procon-technique", techPanel.picked);
  openGroup(tech.group);
  drawTechniques();
  showPicked();
}

/** Open a group (folding the others), or fold it with `toggle` */
function openGroup(id, toggle = false) {
  techPanel.group = toggle && techPanel.group === id ? "" : id;
  techRemember("procon-technique-group", techPanel.group);
}

/** The panel from the status's `techniques` (or a command's reply) */
function renderTechniques(status) {
  if (!status) return;
  techPanel.status = status;
  techPanel.receivedAt = performance.now();
  // A new marker, or a session stopped: the checklist reads the sessions again
  const seen = `${recorder.state}/${status.total}`;
  if (techPanel.seen && seen !== techPanel.seen) allMarkers.at = 0;
  techPanel.seen = seen;
  drawTechniques();
}

/** Whether an item is the one being marked */
function isMarking(tech) {
  const open = techPanel.status.open;
  if (!open) return false;
  return open.item ? open.item === tech.id : open.label === tech.label;
}

/** An item as the list shows it: its key (1–9 in the open group), picture,
 * names (and group, among search results), count, a link to its Pedia
 * entry and, added by hand, Remove */
function itemRow(tech, { picked, count, key = "", hit = false, tag = false }) {
  const [name, alt] = techniqueNames(tech);
  const n = count(tech);
  let counted;
  if (techPanel.mode === "all") {
    counted =
      n == null
        ? `<span class="tech-count">…</span>`
        : n
          ? `<span class="tech-count is-done" title="${escapeHtml(t("tech.allCount", { n }))}">✓ ${n}</span>`
          : `<span class="tech-count is-missing" title="${escapeHtml(t("tech.none"))}">○</span>`;
  } else {
    counted = `<span class="tech-count${n ? " is-done" : ""}" title="${escapeHtml(t("tech.sessionCount", { n }))}">${n ? `×${n}` : "–"}</span>`;
  }
  // An empty slot keeps the counts in one column
  const pedia = tech.term
    ? `<a class="tech-link" href="/cuttlefish/pedia/${encodeURIComponent(tech.term)}" title="${escapeHtml(t("tech.pedia"))}" aria-label="${escapeHtml(t("tech.pedia"))}">?</a>`
    : tech.added
      ? ""
      : `<span class="tech-link is-empty" aria-hidden="true"></span>`;
  const remove = tech.added
    ? `<button type="button" class="tech-link" data-remove="${escapeHtml(tech.label)}" title="${escapeHtml(t("tech.remove"))}" aria-label="${escapeHtml(t("tech.remove"))}">×</button>`
    : "";
  const icon = tech.icon
    ? `<img class="tech-icon" src="${iconUrl(tech.icon)}" alt="" loading="lazy" decoding="async" />`
    : "";
  const group = tag
    ? `<span class="tech-tag">${escapeHtml(t(tech.grizzco ? "tech.grizzco" : `tech.group.${tech.group}`))}</span>`
    : "";
  const classes = `tech-item${isMarking(tech) ? " is-open" : ""}${hit ? " is-hit" : ""}`;
  return `<li class="${classes}">
      <button type="button" class="tech-pick" data-pick="${escapeHtml(itemKey(tech))}" aria-pressed="${itemKey(tech) === itemKey(picked)}">
        <span class="tech-key">${key}</span>${icon}
        <span class="tech-names"><span class="tech-name">${escapeHtml(name)}</span>${alt ? `<span class="tech-alt">${escapeHtml(alt)}</span>` : ""}${group}</span>
        ${counted}
      </button>${pedia}${remove}</li>`;
}

/** Why a group of Lean's items is empty: still read, not read, or not
 * imported into the store yet */
function dataNote() {
  if (gameItems.loading || (!gameItems.list && !gameItems.failed))
    return escapeHtml(t("tech.data.loading"));
  if (gameItems.failed)
    return escapeHtml(t("tech.data.failed", { error: gameItems.failed }));
  const link = `<a href="/cuttlefish/knowledge">${escapeHtml(t("tech.data.knowledge"))}</a>`;
  return t("tech.data.none", { link });
}

/** How many of `items` are recorded (the checklist) or marked (this
 * session), as a head shows it: its count, the color level and what it
 * counts; "…" while not `known` */
function tallyOf(items, count, known = true) {
  const counts = items.map(count);
  const ready = known && counts.every((n) => n != null);
  const done = counts.filter((n) => n > 0).length;
  const all = techPanel.mode === "all";
  const text = !ready
    ? "…"
    : items.length
      ? t(all ? "tech.group.recorded" : "tech.group.marked", {
          done,
          n: items.length,
        })
      : "–";
  // Green once every item is recorded
  const level =
    !ready || !done ? "" : done < items.length ? " is-some" : " is-done";
  const note = t(all ? "tech.group.recordedNote" : "tech.group.markedNote");
  return `<span class="tech-group-count${level}" title="${escapeHtml(note)}">${escapeHtml(text)}</span>`;
}

/** A group: its head (folded or open, how many of its items are recorded,
 * marks for the item picked or being marked) and, open, its items, the
 * first nine with their keys; the Grizzco weapons come last in the
 * Weapons, under a head of their own */
function groupBlock(group, items, { picked, count }) {
  const open = techPanel.group === group.id;
  const marking = items.some(isMarking);
  const hasPicked = items.some((tech) => itemKey(tech) === itemKey(picked));
  const classes = `tech-group${open ? " is-open" : ""}${marking ? " is-marking" : ""}${hasPicked ? " has-picked" : ""}`;
  const head = `<button type="button" class="tech-group-head" data-group="${group.id}" aria-expanded="${open}">
      <span class="tech-chevron" aria-hidden="true"></span>
      <span class="tech-group-name">${escapeHtml(t(`tech.group.${group.id}`))}</span>
      ${tallyOf(items, count, !(group.data && !gameItems.list))}
    </button>`;
  if (!open) return `<li class="${classes}">${head}</li>`;
  const rare = items.filter((tech) => tech.grizzco);
  const rows = items.map((tech, i) => {
    const row = itemRow(tech, {
      picked,
      count,
      key: i < 9 ? String(i + 1) : "",
    });
    if (!tech.grizzco || items[i - 1]?.grizzco) return row;
    return `<li class="tech-subhead"><span>${escapeHtml(t("tech.grizzco"))}</span>${tallyOf(rare, count)}</li>${row}`;
  });
  const body = items.length
    ? `<ul class="tech-items">${rows.join("")}</ul>`
    : `<p class="panel-note tech-empty">${group.data ? dataNote() : ""}</p>`;
  const credit =
    group.data && items.length
      ? `<p class="panel-note tech-credit">${t("tech.credit", {
          link: `<a href="${LEANNY}" target="_blank" rel="noopener">leanny.github.io ↗</a>`,
        })}</p>`
      : "";
  return `<li class="${classes}">${head}${body}${credit}</li>`;
}

/** A name or query as searched: full-width forms made plain, lowercase,
 * letters and digits only, as the Pedia's search (pedia.js) */
const techLoose = (s) =>
  (s ?? "")
    .normalize("NFKC")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]/gu, "");

/** How well an item matches the search `q` (loose): 0 a whole name, 1 a
 * name's start, 2 inside a name; null when it does not */
function techRank(tech, q) {
  const names = [
    tech.label,
    techniqueZh(tech),
    tech.ja,
    tech.term,
    tech.id,
    ...(tech.search ?? []),
  ];
  let best = null;
  for (const name of names) {
    const n = techLoose(name);
    if (!n) continue;
    const r = n === q ? 0 : n.startsWith(q) ? 1 : n.includes(q) ? 2 : null;
    if (r !== null && (best === null || r < best)) best = r;
  }
  return best;
}

/** The items matching the search, best first, at most TECH_FOUND */
function foundItems(list, q) {
  return list
    .map((tech, i) => ({ tech, i, rank: techRank(tech, q) }))
    .filter((x) => x.rank !== null)
    .sort((a, b) => a.rank - b.rank || a.i - b.i)
    .slice(0, TECH_FOUND)
    .map((x) => x.tech);
}

function drawTechniques() {
  const { status, mode } = techPanel;
  const list = techniques();
  const picked = pickedTechnique();
  const all = mode === "all" ? allMarkers.list : null;
  const stale = !allMarkers.at || performance.now() - allMarkers.at >= 30000;
  if (mode === "all" && stale && !allMarkers.loading)
    loadAllMarkers().then(
      drawTechniques,
      (error) => isAbort(error) || showError(error.message, "tech-error"),
    );
  // This session's reps by name (as the lab counts them), or every
  // session's examples; null while those are read
  const count = (tech) =>
    mode === "all"
      ? all
        ? all.filter((m) => markerOf(m, tech)).length
        : null
      : (status.counts[tech.label] ?? 0);
  const q = techLoose(techPanel.query);
  let html;
  if (q) {
    const found = foundItems(list, q);
    techPanel.found = found;
    techPanel.hit = Math.max(0, Math.min(techPanel.hit, found.length - 1));
    html = found.length
      ? found
          .map((tech, i) =>
            itemRow(tech, {
              picked,
              count,
              hit: i === techPanel.hit,
              tag: true,
            }),
          )
          .join("")
      : `<li class="panel-note tech-empty">${escapeHtml(t("tech.find.none"))}</li>`;
  } else {
    techPanel.found = [];
    html = TECH_GROUPS.map((group) =>
      groupBlock(
        group,
        list.filter((tech) => tech.group === group.id),
        { picked, count },
      ),
    ).join("");
  }
  if (html !== techPanel.drawn) {
    const box = $("tech-list");
    // The open group's list keeps its scroll through a redraw
    const shown = q ? "" : techPanel.group;
    const scrolled = box.querySelector(".tech-items")?.scrollTop ?? 0;
    const same = techPanel.drawnGroup === shown;
    techPanel.drawn = html;
    techPanel.drawnGroup = shown;
    box.innerHTML = html;
    const items = box.querySelector(".tech-items");
    if (items && same) items.scrollTop = scrolled;
  }

  if (mode === "all") {
    const done = all
      ? list.filter((tech) => all.some((m) => markerOf(m, tech))).length
      : 0;
    const sessions = all ? new Set(all.map((m) => m.session)).size : 0;
    $("tech-note").textContent = all
      ? t("tech.allNote", {
          done,
          n: list.length,
          markers: all.length,
          sessions,
        })
      : t("tech.loading");
  } else {
    $("tech-note").textContent =
      recorder.state === "idle" && !status.total
        ? t("tech.idleNote")
        : t("tech.sessionNote", { n: status.total });
  }
  $("tech-mode-session").setAttribute("aria-pressed", String(mode !== "all"));
  $("tech-mode-all").setAttribute("aria-pressed", String(mode === "all"));

  const recording = recorder.state === "recording";
  $("tech-span").disabled = !status.open && !recording;
  $("tech-span-text").textContent = t(status.open ? "tech.stop" : "tech.start");
  $("tech-span").classList.toggle("is-open", Boolean(status.open));
  $("tech-last").disabled = recorder.state === "idle";
  $("tech-undo").disabled = !status.open && !status.total;
  drawPicked(picked);
  drawSpanClock();
}

/** What M and B mark: the item picked, with its picture */
function drawPicked(tech) {
  const [name, alt] = techniqueNames(tech);
  const html = `<span class="tech-picked-label">${escapeHtml(t("tech.picked"))}</span>
    ${tech.icon ? `<img class="tech-icon" src="${iconUrl(tech.icon)}" alt="" />` : ""}
    <span class="tech-name">${escapeHtml(name)}</span>${alt ? `<span class="tech-alt">${escapeHtml(alt)}</span>` : ""}`;
  const el = $("tech-picked");
  if (el.dataset.drawn !== html) {
    el.dataset.drawn = html;
    el.innerHTML = html;
  }
}

/** Bring the item picked into view in its group's list, scrolling only the
 * list */
function showPicked() {
  const row = $("tech-list").querySelector(
    '.tech-items .tech-pick[aria-pressed="true"]',
  );
  const box = row?.closest(".tech-items");
  if (!box) return;
  const top = row.offsetTop - box.offsetTop;
  if (top < box.scrollTop) box.scrollTop = top;
  else if (top + row.offsetHeight > box.scrollTop + box.clientHeight)
    box.scrollTop = top + row.offsetHeight - box.clientHeight;
}

/** The span being marked and how long it has run */
function drawSpanClock() {
  const open = techPanel.status.open;
  $("tech-live").hidden = !open;
  if (!open) return;
  const tech = techniques().find(isMarking) ?? open;
  const elapsed =
    open.elapsed_ms +
    (recorder.state === "recording"
      ? performance.now() - techPanel.receivedAt
      : 0);
  const text = t("tech.marking", {
    name: techniqueNames(tech)[0],
    time: formatClock(elapsed).replace(/^00:/, ""),
  });
  const live = $("tech-live-text");
  if (live.textContent !== text) live.textContent = text;
}

/** Send a marker command; the reply redraws the panel */
function techCommand(body) {
  return sendCommand(body, "tech-error");
}

/** The item picked as a marker command names it */
function markedItem() {
  const tech = pickedTechnique();
  return {
    label: tech.label,
    term: tech.term ?? null,
    item: tech.id ?? null,
    kind: itemKind(tech),
  };
}

function toggleSpan() {
  if (techPanel.status.open) return techCommand({ action: "mark_stop" });
  if (recorder.state !== "recording") return;
  techCommand({ action: "mark_start", ...markedItem() });
}

function markLast() {
  if (recorder.state === "idle") return;
  const seconds = parseFloat($("tech-seconds").value) || 5;
  techCommand({ action: "mark_last", ...markedItem(), seconds });
}

function undoMarker() {
  techCommand({ action: "mark_undo" });
}

/** Save the added techniques */
function saveAdded(added) {
  return techCommand({
    action: "set_techniques",
    techniques: added.map(({ label, zh, term, group }) => ({
      label,
      zh,
      term,
      group,
    })),
  });
}

/** Leave the search: its text cleared, the groups back */
function closeFind() {
  const find = $("tech-find");
  find.value = "";
  techPanel.query = "";
  techPanel.hit = 0;
  find.blur();
  drawTechniques();
}

$("tech-list").addEventListener("click", (event) => {
  const head = event.target.closest("[data-group]");
  if (head) {
    openGroup(head.dataset.group, true);
    return drawTechniques();
  }
  const pick = event.target.closest("[data-pick]");
  if (pick) {
    const tech = techniques().find((x) => itemKey(x) === pick.dataset.pick);
    if (!tech) return;
    if (techPanel.query) closeFind();
    return pickTechnique(tech);
  }
  const remove = event.target.closest("[data-remove]");
  if (!remove) return;
  const label = remove.dataset.remove;
  if (!confirm(t("tech.removeAsk", { name: label }))) return;
  saveAdded(techPanel.status.added.filter((x) => x.label !== label));
});
// A picture Lean's site does not have leaves its slot empty
for (const id of ["tech-list", "tech-picked"]) {
  $(id).addEventListener(
    "error",
    (event) => {
      if (event.target.classList?.contains("tech-icon"))
        event.target.classList.add("is-broken");
    },
    true,
  );
}
$("tech-span").addEventListener("click", toggleSpan);
$("tech-last").addEventListener("click", markLast);
$("tech-undo").addEventListener("click", undoMarker);
for (const [id, mode] of [
  ["tech-mode-session", "session"],
  ["tech-mode-all", "all"],
]) {
  $(id).addEventListener("click", () => {
    techPanel.mode = mode;
    techRemember("procon-technique-mode", mode);
    // The checklist always reads the sessions afresh when opened
    if (mode === "all") allMarkers.at = 0;
    drawTechniques();
  });
}

// The search: type to list the matching items, ↑ ↓ to choose, Enter picks
// (the first by default), Esc leaves
$("tech-find").addEventListener("input", (event) => {
  techPanel.query = event.target.value;
  techPanel.hit = 0;
  drawTechniques();
});
$("tech-find").addEventListener("keydown", (event) => {
  const { found } = techPanel;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    if (!found.length) return;
    const step = event.key === "ArrowDown" ? 1 : found.length - 1;
    techPanel.hit = (techPanel.hit + step) % found.length;
    drawTechniques();
  } else if (event.key === "Enter") {
    const tech = found[techPanel.hit];
    if (!tech) return;
    closeFind();
    pickTechnique(tech);
  } else if (event.key === "Escape") closeFind();
  else return;
  event.preventDefault();
});

$("tech-add").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = event.target;
  const value = (name) => form.elements[name].value.trim();
  const label = value("label");
  if (!label) return;
  if (techniques().some((tech) => tech.label === label)) {
    showError(t("tech.exists", { name: label }), "tech-error");
    return;
  }
  const group = value("group");
  const added = [
    ...techPanel.status.added,
    { label, zh: value("zh") || null, term: value("term") || null, group },
  ];
  if (await saveAdded(added)) {
    form.reset();
    pickTechnique({ label, group });
  }
});

// Keys while the Studio is shown, outside text fields: / finds an item,
// 1–9 pick in the open group, M starts or stops a span, B marks the last
// seconds, U undoes the last marker
document.addEventListener("keydown", (event) => {
  if (!studioShown() || event.repeat) return;
  const tag = event.target.tagName;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  const key = event.key.toLowerCase();
  if (key === "/") $("tech-find").focus();
  else if (/^[1-9]$/.test(key)) {
    const tech = techniques().filter((x) => x.group === techPanel.group)[
      Number(key) - 1
    ];
    if (!tech) return;
    pickTechnique(tech);
  } else if (key === "m") toggleSpan();
  else if (key === "b") markLast();
  else if (key === "u") undoMarker();
  else return;
  event.preventDefault();
});

// The span's clock runs between status messages
setInterval(() => {
  if (techPanel.status.open && studioShown()) drawSpanClock();
}, 250);

window.addEventListener("lang-change", () => {
  techPanel.drawn = "";
  drawTechniques();
});
window.addEventListener("app-route", ({ detail }) => {
  if (detail.app !== "studio") return;
  // Back to the Studio: the checklist may have changed in the Inkspector,
  // and Lean's data may have been imported since
  allMarkers.at = 0;
  loadGameItems();
  if (techPanel.mode === "all") drawTechniques();
});
loadGameItems();
drawTechniques();
