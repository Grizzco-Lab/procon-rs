// Technique markers: spans of a recording labelled as a technique practised
// (a squid roll, an inertia cancel, an egg throw…), for labelled examples and
// as a reminder of what to record. The Studio's Techniques panel marks them
// while recording (a span started and stopped by hand, or the last few
// seconds) through /api/command; they are saved in the session's
// session.json as `markers` (host Unix ms, the frames' clock). The
// Inkspector shows and edits them (inspect.js) and the Pedia lists them
// under a technique's entry (pedia.js), both through
// /api/inspect/markers. Runs after app.js and player.js and uses their
// helpers ($, sendCommand, recorder, studioShown, escapeHtml) and those of
// i18n.js (t, I18N).
"use strict";

/**
 * The usual techniques to practise: `id` (their Chinese name is
 * `tech.zh.<id>` in i18n-zh.js), the English name markers get as their
 * label, and the Pedia term id where the glossary has one. Techniques added
 * in the panel follow them, saved in the dashboard's state file.
 */
const TECHNIQUES = [
  { id: "squid-roll", label: "Squid roll", term: "squid-roll" },
  {
    id: "sub-strafe",
    label: "Sub strafe (inertia cancel)",
    term: "inertia-cancel",
  },
  { id: "main-strafe", label: "Main strafe", term: "main-strafe" },
  { id: "wall-climb", label: "Fast wall climb" },
  { id: "hop", label: "Small hop / big jump" },
  { id: "egg-grab", label: "Grab eggs without cancelling ink recovery" },
  { id: "egg-throw", label: "Egg throw", term: "egg-toss" },
  { id: "basket-run", label: "Egg runs at the basket", term: "egg-run" },
];

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

/** Whether a marker is an example of a technique */
function markerOf(marker, tech) {
  return (
    marker.label === tech.label ||
    Boolean(tech.term && marker.term === tech.term)
  );
}

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
    .finally(() => {
      // A failed read is not retried at once either
      allMarkers.at = performance.now();
      allMarkers.loading = null;
    });
  return allMarkers.loading;
}

// ------------------------------------------------------------ Studio panel

const techPanel = {
  /** The label of the technique picked, remembered in this browser */
  picked: null,
  /** "session": reps marked in this session; "all": the checklist */
  mode: "session",
  /** The server's side: added techniques, open span, counts */
  status: { added: [], open: null, counts: {}, total: 0 },
  /** When the open span's elapsed time arrived */
  receivedAt: 0,
  /** The list's markup as last drawn, to redraw only on change */
  drawn: "",
  /** Recorder state and marker total last seen, to reload the checklist */
  seen: "",
};

try {
  techPanel.picked = localStorage.getItem("procon-technique");
  techPanel.mode =
    localStorage.getItem("procon-technique-mode") === "all" ? "all" : "session";
} catch {
  // Storage may be refused; the first technique is picked
}

/** The usual techniques, then the added ones */
function techniques() {
  return [
    ...TECHNIQUES,
    ...techPanel.status.added.map((tech) => ({ ...tech, added: true })),
  ];
}

/** The technique picked, else the first */
function pickedTechnique() {
  const list = techniques();
  return list.find((tech) => tech.label === techPanel.picked) ?? list[0];
}

function pickTechnique(label) {
  techPanel.picked = label;
  try {
    localStorage.setItem("procon-technique", label);
  } catch {
    // The choice holds until reload
  }
  drawTechniques();
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

function drawTechniques() {
  const { status, mode } = techPanel;
  const list = techniques();
  const picked = pickedTechnique();
  const all = mode === "all" ? allMarkers.list : null;
  const stale = !allMarkers.at || performance.now() - allMarkers.at >= 30000;
  if (mode === "all" && stale && !allMarkers.loading)
    loadAllMarkers().then(drawTechniques, (error) =>
      showError(error.message, "tech-error"),
    );
  const rows = list.map((tech, i) => {
    const [name, alt] = techniqueNames(tech);
    let count;
    if (mode === "all") {
      const n = all ? all.filter((m) => markerOf(m, tech)).length : null;
      count =
        n == null
          ? `<span class="tech-count">…</span>`
          : n
            ? `<span class="tech-count is-done" title="${escapeHtml(t("tech.allCount", { n }))}">✓ ${n}</span>`
            : `<span class="tech-count is-missing" title="${escapeHtml(t("tech.none"))}">○</span>`;
    } else {
      const n = status.counts[tech.label] ?? 0;
      count = `<span class="tech-count${n ? " is-done" : ""}" title="${escapeHtml(t("tech.sessionCount", { n }))}">${n ? `×${n}` : "–"}</span>`;
    }
    const open = status.open?.label === tech.label;
    // An empty slot keeps the counts in one column
    const pedia = tech.term
      ? `<a class="tech-link" href="/cuttlefish/pedia/${encodeURIComponent(tech.term)}" title="${escapeHtml(t("tech.pedia"))}" aria-label="${escapeHtml(t("tech.pedia"))}">?</a>`
      : tech.added
        ? ""
        : `<span class="tech-link is-empty" aria-hidden="true"></span>`;
    const remove = tech.added
      ? `<button type="button" class="tech-link" data-remove="${i}" title="${escapeHtml(t("tech.remove"))}" aria-label="${escapeHtml(t("tech.remove"))}">×</button>`
      : "";
    return `<li class="tech-item${open ? " is-open" : ""}">
      <button type="button" class="tech-pick" data-pick="${i}" aria-pressed="${tech === picked}">
        <span class="tech-key">${i < 9 ? i + 1 : ""}</span>
        <span class="tech-names"><span class="tech-name">${escapeHtml(name)}</span>${alt ? `<span class="tech-alt">${escapeHtml(alt)}</span>` : ""}</span>
        ${count}
      </button>${pedia}${remove}</li>`;
  });
  const html = rows.join("");
  if (html !== techPanel.drawn) {
    techPanel.drawn = html;
    $("tech-list").innerHTML = html;
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
  drawSpanClock();
}

/** The span being marked and how long it has run */
function drawSpanClock() {
  const open = techPanel.status.open;
  $("tech-live").hidden = !open;
  if (!open) return;
  const tech = techniques().find((x) => x.label === open.label) ?? open;
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

function toggleSpan() {
  if (techPanel.status.open) return techCommand({ action: "mark_stop" });
  if (recorder.state !== "recording") return;
  const tech = pickedTechnique();
  techCommand({ action: "mark_start", label: tech.label, term: tech.term });
}

function markLast() {
  if (recorder.state === "idle") return;
  const tech = pickedTechnique();
  const seconds = parseFloat($("tech-seconds").value) || 5;
  techCommand({
    action: "mark_last",
    label: tech.label,
    term: tech.term,
    seconds,
  });
}

function undoMarker() {
  techCommand({ action: "mark_undo" });
}

/** Save the added techniques */
function saveAdded(added) {
  return techCommand({
    action: "set_techniques",
    techniques: added.map(({ label, zh, term }) => ({ label, zh, term })),
  });
}

$("tech-list").addEventListener("click", (event) => {
  const pick = event.target.closest("[data-pick]");
  if (pick) return pickTechnique(techniques()[Number(pick.dataset.pick)].label);
  const remove = event.target.closest("[data-remove]");
  if (!remove) return;
  const tech = techniques()[Number(remove.dataset.remove)];
  if (!confirm(t("tech.removeAsk", { name: tech.label }))) return;
  saveAdded(techPanel.status.added.filter((x) => x.label !== tech.label));
});
$("tech-span").addEventListener("click", toggleSpan);
$("tech-last").addEventListener("click", markLast);
$("tech-undo").addEventListener("click", undoMarker);
for (const [id, mode] of [
  ["tech-mode-session", "session"],
  ["tech-mode-all", "all"],
]) {
  $(id).addEventListener("click", () => {
    techPanel.mode = mode;
    try {
      localStorage.setItem("procon-technique-mode", mode);
    } catch {
      // The choice holds until reload
    }
    // The checklist always reads the sessions afresh when opened
    if (mode === "all") allMarkers.at = 0;
    drawTechniques();
  });
}
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
  const added = [
    ...techPanel.status.added,
    { label, zh: value("zh") || null, term: value("term") || null },
  ];
  if (await saveAdded(added)) {
    form.reset();
    pickTechnique(label);
  }
});

// Keys while the Studio is shown, outside text fields: 1–9 pick, M starts or
// stops a span, B marks the last seconds, U undoes the last marker
document.addEventListener("keydown", (event) => {
  if (!studioShown() || event.repeat) return;
  const tag = event.target.tagName;
  if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
  if (event.ctrlKey || event.metaKey || event.altKey) return;
  const key = event.key.toLowerCase();
  if (/^[1-9]$/.test(key)) {
    const tech = techniques()[Number(key) - 1];
    if (!tech) return;
    pickTechnique(tech.label);
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
  // Back to the Studio: the checklist may have changed in the Inkspector
  if (detail.app === "studio") allMarkers.at = 0;
  if (detail.app === "studio" && techPanel.mode === "all") drawTechniques();
});
drawTechniques();
