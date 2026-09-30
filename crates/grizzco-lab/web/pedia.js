// Cuttlefish's Overfishing Pedia (/cuttlefish/pedia, one entry at
// /cuttlefish/pedia/<term id>): the glossary as an encyclopedia of Salmon
// Run for new players, from GET /api/cuttlefish/pedia and pedia/<id> (see
// src/pedia.rs). The index groups the terms in sections, with search in any
// language (slang included), filters by era and source, and A–Z or "most
// discussed" (comments of #vod-review mentioning the term). An entry has
// the official names, the slang, the definition, related terms both ways,
// an icon, fact cards (filled in when the server is still reading them),
// quotes "in the wild" (each linking its Discord message and, when the VOD
// is a review here, the review at its moment),
// expert notes and deep questions, and "Ask Cuttlefish". Everything can be
// corrected in place: the definition, kind and relation through the
// term-edit API (editTerm below, the one place that knows it), aliases
// through the slang endpoints; the glossary reloads after each, so the chat
// and the translator use the correction at once. Runs after cuttlefish.js
// (which hides its library and player for this view and marks the tab),
// knowledge.js (window.cuttlefishNotes, the note editor) and translate.js,
// and uses the helpers of i18n.js, app.js and player.js (t, $, escapeHtml,
// navigate, replaceRoute).
"use strict";

(() => {
  /** Quotes an entry shows first, and how many more "Show more" adds */
  const QUOTES = 6;
  const MORE_QUOTES = 10;
  /** Typing pauses this long (ms) before the index filters */
  const SEARCH_MS = 120;
  /** How often (ms) an entry asks again for fact cards still being read */
  const CARDS_MS = 2000;
  /** Fact cards shown open; with more, each is folded to its title */
  const FACTS_OPEN = 3;
  /** Fact cards listed before "Show all" */
  const FACTS_SHOWN = 8;
  /** Relations a term can have to a broader one */
  const RELATIONS = ["part-of", "kind-of", "related-to"];
  /** Languages offered for a new alias, each named in itself */
  const LANGUAGES = [
    ["en", "English"],
    ["zh", "中文"],
    ["ja", "日本語"],
    ["es", "Español"],
    ["fr", "Français"],
    ["ru", "Русский"],
    ["ko", "한국어"],
  ];
  /** Languages of official names shown first on an entry */
  const FIRST_LANGS = ["en", "ja", "zh", "zh-Hant", "ko"];
  /** Terms drawn with a class icon of the set (web/icons/class-*.svg) */
  const CLASS_ICONS = {
    steelhead: "steelhead",
    flyfish: "flyfish",
    scrapper: "scrapper",
    "steel-eel": "steel_eel",
    stinger: "stinger",
    maws: "maws",
    drizzler: "drizzler",
    "fish-stick": "fish_stick",
    "flipper-flopper": "flipper_flopper",
    "big-shot": "big_shot",
    "slammin-lid": "slammin_lid",
    smallfry: "smallfry",
    chum: "chum",
    cohock: "cohock",
    "golden-egg": "golden_egg",
    "egg-basket": "basket",
    cohozuna: "cohozuna",
    horrorboros: "horrorboros",
    megalodontia: "megalodontia",
  };
  /** Kinds offered when editing (any kind may be typed) */
  const KINDS = [
    "boss",
    "part",
    "attack",
    "technique",
    "mechanic",
    "event",
    "tide",
    "stage",
    "weapon",
    "special",
    "sub",
    "role",
    "category",
    "callout",
    "mode",
  ];

  const pd = {
    /** Whether the view is shown */
    shown: false,
    /** GET pedia, or null until loaded (and after an edit) */
    list: null,
    /** The open entry (GET pedia/<id>), or null in the index */
    entry: null,
    /** The term the address names */
    id: "",
    /** Quotes asked for */
    quotes: QUOTES,
    /** The index's search and filters */
    q: "",
    section: "",
    era: remembered("era", ""),
    source: remembered("source", ""),
    order: remembered("order", "az"),
    /** A new term rejected from its page, for Undo in the index */
    rejected: null,
    /** Whether the open entry lists all its fact cards */
    allFacts: false,
  };

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-pedia-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-pedia-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  /** GET or POST (with a JSON body) under /api/cuttlefish/; throws with the
   * server's message */
  async function api(path, method = "GET", body) {
    const response = await fetch(`/api/cuttlefish/${path}`, {
      method,
      ...(body !== undefined && {
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      }),
    });
    const data = await response.json();
    if (!response.ok) throw new Error(data.error ?? response.statusText);
    return data;
  }

  /** Changes a term's kind, definition or relation (`relation`: {kind,
   * term} or null to remove it): a new term of the user file in place, a
   * glossary term as the user's override (see
   * cuttlefish::slang::UserGlossary::edit_term) */
  const editTerm = (id, fields) =>
    api("knowledge/slang/term-edit", "POST", { id, ...fields });

  /** A glossary term back as the glossary has it (drops the override) */
  const resetTerm = (id) => api("knowledge/slang/term-reset", "POST", { id });

  const number = (n) => Number(n ?? 0).toLocaleString(i18nLocale());
  const langName = (code) =>
    LANGUAGES.find(([c]) => c === code)?.[1] ??
    {
      "zh-Hant": "繁體中文",
      de: "Deutsch",
      it: "Italiano",
      nl: "Nederlands",
      pt: "Português",
    }[code] ??
    code;

  /** A term's name in the page's language, else English */
  function nameOf(term) {
    const names = term.names ?? {};
    return (
      (i18nLang() === "zh" && names.zh) || term.name || names.en || term.id
    );
  }

  /** Its other main names worth showing under it (English, Japanese,
   * Chinese), without the one shown */
  function otherNames(term) {
    const shown = nameOf(term);
    const names = term.names ?? {};
    return [...new Set(["en", "ja", "zh"].map((l) => names[l]).filter(Boolean))]
      .filter((n) => n !== shown)
      .slice(0, 2);
  }

  /** The icon of a term: a class icon of the set, its broader term's when
   * it is a part of one, else none */
  function classIcon(term) {
    const own = CLASS_ICONS[term.id];
    if (own) return own;
    const r = term.related;
    return r && r.kind === "part-of" ? CLASS_ICONS[r.term] : undefined;
  }

  const iconSvg = (name, cls) =>
    `<svg class="${cls}" viewBox="0 0 24 24" aria-hidden="true"><use href="/icons/class-${name}.svg#i" /></svg>`;

  /** The address of an entry, or of the index (with a section) */
  const entryUrl = (id) => `/cuttlefish/pedia/${encodeURIComponent(id)}`;
  const sectionUrl = (section) =>
    `/cuttlefish/pedia${section ? `?section=${encodeURIComponent(section)}` : ""}`;

  /** `text` with every name of `names` marked (a Latin name as a whole
   * word), escaped */
  function highlight(text, names) {
    const latin = [];
    const other = [];
    for (const n of names) {
      if (!n || n.length < 2) continue;
      const pattern = n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      (/^[\p{Script=Latin}\p{N}\p{P}\s]+$/u.test(n) ? latin : other).push(
        pattern,
      );
    }
    const parts = [];
    if (latin.length)
      parts.push(`(?<![\\p{L}\\p{N}])(?:${latin.join("|")})(?![\\p{L}\\p{N}])`);
    if (other.length) parts.push(`(?:${other.join("|")})`);
    if (!parts.length) return escapeHtml(text);
    const re = new RegExp(parts.join("|"), "giu");
    let out = "";
    let at = 0;
    for (const m of text.matchAll(re)) {
      out += escapeHtml(text.slice(at, m.index));
      out += `<mark>${escapeHtml(m[0])}</mark>`;
      at = m.index + m[0].length;
    }
    return out + escapeHtml(text.slice(at));
  }

  /** "12 mentions" as a chip */
  const talkHtml = (n) =>
    n
      ? `<span class="pd-talk" title="${escapeHtml(t("pedia.mentionsTitle", { n }))}">${escapeHtml(t("pedia.mentions", { n }))}</span>`
      : "";

  /** Era chips: every game on an entry; on a card only an older game's
   * term that Splatoon 3 lacks is worth marking */
  const gamesHtml = (games, all = false) =>
    (games ?? [])
      .filter((g) => all || (g !== "S3" && !games.includes("S3")))
      .map(
        (g) =>
          `<span class="pd-game" data-game="${escapeHtml(g)}">${escapeHtml(g)}</span>`,
      )
      .join("");

  /** Where the entry comes from, as badges */
  const facetsHtml = (facets) =>
    ["official", "community", "user"]
      .filter((f) => facets?.[f])
      .map(
        (f) =>
          `<span class="pd-facet" data-facet="${f}" title="${escapeHtml(t(`pedia.facet.${f}Title`))}">${escapeHtml(t(`pedia.facet.${f}`))}</span>`,
      )
      .join("");

  /** A kind as shown: translated when the dictionary has it, else as the
   * glossary writes it */
  function kindLabel(kind) {
    if (!kind) return "";
    const key = `pedia.kind.${kind}`;
    const label = t(key);
    return label === key ? kind : label;
  }

  // ------------------------------------------------------------ the index

  async function loadList() {
    try {
      pd.list = await api("pedia");
    } catch (error) {
      $("pd-empty").hidden = false;
      $("pd-empty").textContent = t("pedia.failed", { error: error.message });
      return;
    }
    drawIndex();
  }

  /** A name or query as searched: full-width forms made plain (NFKC),
   * lowercase, letters and digits only, so "splash down", "Splash-Down"
   * and "ＳＰＬＡＳＨＤＯＷＮ" all read "splashdown" (as the glossary's
   * search, cuttlefish::glossary::loose) */
  const loose = (s) =>
    (s ?? "")
      .normalize("NFKC")
      .toLowerCase()
      .replace(/[^\p{L}\p{N}]/gu, "");

  /** Each term's names and definition as searched, made once per list:
   * each name loose, and loose from each of its later words on */
  const searchable = new WeakMap();
  function searchOf(term) {
    let found = searchable.get(term);
    if (!found) {
      found = {
        names: [term.id, ...(term.search ?? [])].map((name) => {
          const words = name
            .normalize("NFKC")
            .toLowerCase()
            .split(/[^\p{L}\p{N}]+/u)
            .filter(Boolean);
          return {
            name,
            whole: words.join(""),
            later: words.slice(1).map((_, i) => words.slice(i + 1).join("")),
          };
        }),
        definition: loose(term.definition),
      };
      searchable.set(term, found);
    }
    return found;
  }

  /** How well a term matches the search `q` (loose): 0 a whole name, 1 a
   * name's start, 2 a later word's start, 3 inside a name, 4 the
   * definition, with the name that matched; null when it does not. A
   * short Latin query only counts at word starts, so "sd" is not found
   * across "ataques de". */
  function rank(term, q) {
    const { names, definition } = searchOf(term);
    const inside = q.length >= 3 || /[^\p{Script=Latin}\p{N}]/u.test(q);
    let best = null;
    for (const { name, whole, later } of names) {
      const r =
        whole === q
          ? 0
          : whole.startsWith(q)
            ? 1
            : later.some((w) => w.startsWith(q))
              ? 2
              : inside && whole.includes(q)
                ? 3
                : null;
      if (r !== null && (best === null || r < best.rank))
        best = { rank: r, name };
    }
    if (best === null && q.length >= 3 && definition.includes(q))
      best = { rank: 4, name: "" };
    return best;
  }

  /** The terms the search keeps, each with its rank and the name that
   * matched, and whether the era, source and section filters keep them
   * too (the era: a term of no game in particular, the seed's and the
   * user's, is in both) */
  function matching() {
    const q = loose(pd.q);
    return pd.list.terms
      .map((term) => ({ term, ...(q ? rank(term, q) : { rank: 0 }) }))
      .filter(({ rank }) => rank !== undefined)
      .map((found) => {
        const { term } = found;
        found.kept =
          !(pd.era && term.games.length && !term.games.includes(pd.era)) &&
          !(pd.source && !term.facets[pd.source]);
        return found;
      });
  }

  /** The terms the search and the filters keep */
  const filtered = () => matching().filter((found) => found.kept);

  function sortTerms(found) {
    const collator = new Intl.Collator(i18nLocale(), { sensitivity: "base" });
    return found.sort(
      (a, b) =>
        a.rank - b.rank ||
        (pd.order === "talk" ? b.term.mentions - a.term.mentions : 0) ||
        collator.compare(nameOf(a.term), nameOf(b.term)),
    );
  }

  /** A term's card in the index; `matched`, the name the search found it
   * by, shows when the card does not already */
  function cardHtml(term, matched = "") {
    const icon = classIcon(term);
    const initial = [...nameOf(term)][0] ?? "?";
    const shown = [nameOf(term), ...otherNames(term)].map(loose);
    const via =
      matched && matched !== term.id && !shown.includes(loose(matched))
        ? matched
        : "";
    return `<a class="pd-card" href="${entryUrl(term.id)}" data-section="${term.section}">
      <span class="pd-card-art" aria-hidden="true">${icon ? iconSvg(icon, "pd-card-icon") : `<span class="pd-initial">${escapeHtml(initial.toUpperCase())}</span>`}</span>
      <span class="pd-card-text">
        <span class="pd-card-name">${escapeHtml(nameOf(term))}</span>
        ${otherNames(term).length ? `<span class="pd-card-alt">${escapeHtml(otherNames(term).join(" · "))}</span>` : ""}
        ${via ? `<span class="pd-card-via">${escapeHtml(t("pedia.matched", { name: via }))}</span>` : ""}
        ${term.definition ? `<span class="pd-card-def">${escapeHtml(term.definition)}</span>` : ""}
        <span class="pd-card-meta">
          ${term.kind ? `<span class="pd-kind">${escapeHtml(kindLabel(term.kind))}</span>` : ""}
          ${gamesHtml(term.games)}
          ${talkHtml(term.mentions)}
        </span>
      </span>
    </a>`;
  }

  function drawIndex() {
    if (!pd.list) return;
    const all = matching();
    const found = sortTerms(all.filter((f) => f.kept));
    const { corpus } = pd.list;
    $("pd-lede").textContent = t("pedia.lede", {
      n: number(pd.list.terms.length),
      comments: number(corpus.comments),
      vods: number(corpus.vods),
    });
    markTools();
    // Section chips count what the other filters keep
    const counts = {};
    for (const { term } of found)
      counts[term.section] = (counts[term.section] ?? 0) + 1;
    const sections = pd.list.sections.map((s) => s.id);
    $("pd-sections").innerHTML = [
      `<a class="pd-section-chip" href="${sectionUrl("")}" ${pd.section ? "" : 'aria-current="page"'}>${escapeHtml(t("pedia.all"))} <span class="num">${number(found.length)}</span></a>`,
      ...sections.map(
        (s) =>
          `<a class="pd-section-chip" data-section="${s}" href="${sectionUrl(s)}" ${pd.section === s ? 'aria-current="page"' : ""}>${escapeHtml(t(`pedia.section.${s}`))} <span class="num">${number(counts[s] ?? 0)}</span></a>`,
      ),
    ].join("");
    // While searching, the sections in the order of their best match
    const order = loose(pd.q)
      ? [...new Set(found.map(({ term }) => term.section))]
      : sections;
    const shown = pd.section ? [pd.section] : order;
    const groups = shown
      .map((s) => {
        const terms = found.filter(({ term }) => term.section === s);
        if (!terms.length) return "";
        return `<section class="pd-group" data-section="${s}" aria-labelledby="pd-h-${s}">
          <header class="pd-group-head">
            <h2 id="pd-h-${s}">${escapeHtml(t(`pedia.section.${s}`))}</h2>
            <span class="panel-note">${escapeHtml(t(`pedia.section.${s}Note`))}</span>
          </header>
          <div class="pd-cards">${terms.map(({ term, name }) => cardHtml(term, name)).join("")}</div>
        </section>`;
      })
      .join("");
    $("pd-groups").innerHTML = groups;
    const empty = !groups;
    $("pd-empty").hidden = !empty;
    // What the search finds that the section, era or source filters hide
    const hidden =
      all.length -
      found.filter(({ term }) => shown.includes(term.section)).length;
    if (empty)
      $("pd-empty").innerHTML = hidden
        ? `${escapeHtml(t("pedia.hidden", { n: hidden }))}
          <button type="button" class="mode-toggle cf-mini" data-clear-filters>${escapeHtml(t("pedia.showAll"))}</button>`
        : escapeHtml(t("pedia.noMatch"));
    drawRejected();
  }

  /** The filters' buttons, pressed as chosen */
  function markTools() {
    for (const [box, key] of [
      ["pd-era", "era"],
      ["pd-source", "source"],
      ["pd-order", "order"],
    ]) {
      for (const b of $(box).querySelectorAll("button")) {
        b.setAttribute("aria-pressed", String(b.dataset.value === pd[key]));
      }
    }
  }

  for (const [box, key] of [
    ["pd-era", "era"],
    ["pd-source", "source"],
    ["pd-order", "order"],
  ]) {
    $(box).addEventListener("click", (event) => {
      const button = event.target.closest("button[data-value]");
      if (!button) return;
      pd[key] = button.dataset.value;
      remember(key, pd[key]);
      drawIndex();
    });
  }

  let searchTimer = 0;
  $("pd-q").addEventListener("input", () => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      pd.q = $("pd-q").value;
      drawIndex();
    }, SEARCH_MS);
  });
  // Enter opens the best match
  $("pd-q").addEventListener("keydown", (event) => {
    if (event.key !== "Enter" || event.isComposing || !pd.list) return;
    pd.q = $("pd-q").value;
    const best = sortTerms(filtered())[0];
    if (best && pd.q.trim()) navigate(entryUrl(best.term.id));
  });

  // "Show them": the search's matches without the filters
  $("pd-empty").addEventListener("click", (event) => {
    if (!event.target.closest("[data-clear-filters]")) return;
    for (const key of ["era", "source"]) {
      pd[key] = "";
      remember(key, "");
    }
    if (pd.section) navigate(sectionUrl(""));
    else drawIndex();
  });

  $("pd-random").onclick = () => {
    const pool = pd.list ? filtered() : [];
    if (!pool.length) return;
    const pick = pool[Math.floor(Math.random() * pool.length)].term;
    navigate(entryUrl(pick.id));
  };

  /** A new term just rejected from its page, with Undo */
  function drawRejected() {
    const box = $("pd-notice");
    box.hidden = !pd.rejected;
    if (!pd.rejected) return;
    box.innerHTML = `${escapeHtml(t("pedia.rejected", { name: pd.rejected.name }))}
      <button type="button" class="mode-toggle cf-mini" data-undo-reject>${escapeHtml(t("slang.undo"))}</button>`;
  }

  $("pd-notice").addEventListener("click", async (event) => {
    if (!event.target.closest("[data-undo-reject]") || !pd.rejected) return;
    const { id } = pd.rejected;
    try {
      await api("knowledge/slang/term", "POST", { id, status: "approved" });
      pd.rejected = null;
      pd.list = null;
      navigate(entryUrl(id));
    } catch (error) {
      $("pd-notice").textContent = t("pedia.editFailed", {
        error: error.message,
      });
    }
  });

  // ------------------------------------------------------------ an entry

  async function loadEntry(id, { keepScroll = false } = {}) {
    const box = $("pd-entry");
    if (pd.entry?.term.id !== id) {
      box.innerHTML = `<p class="panel-note pd-loading">${escapeHtml(t("cf.loading"))}</p>`;
    }
    const y = window.scrollY;
    let entry;
    try {
      entry = await api(`pedia/${encodeURIComponent(id)}?quotes=${pd.quotes}`);
    } catch (error) {
      box.innerHTML = `<nav class="pd-crumbs"><a href="/cuttlefish/pedia">${escapeHtml(t("pedia.title"))}</a></nav>
        <p class="panel-note level-critical">${escapeHtml(t("pedia.noEntry", { id, error: error.message }))}</p>`;
      return;
    }
    // The address moved on meanwhile
    if (pd.id !== id) return;
    pd.entry = entry;
    drawEntry();
    if (keepScroll) window.scrollTo(0, y);
    else window.scrollTo(0, 0);
    if (entry.cards_pending) setTimeout(() => fillCards(entry), CARDS_MS);
  }

  /** Official names by language: the main languages first */
  function namesHtml(term) {
    const langs = Object.keys(term.forms).sort((a, b) => {
      const ia = FIRST_LANGS.indexOf(a);
      const ib = FIRST_LANGS.indexOf(b);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib) || a.localeCompare(b);
    });
    if (!langs.length) return "";
    return `<dl class="pd-names">${langs
      .map(
        (l) =>
          `<div><dt title="${escapeHtml(l)}">${escapeHtml(langName(l))}</dt><dd lang="${escapeHtml(l)}">${escapeHtml(term.forms[l].join(" · "))}</dd></div>`,
      )
      .join("")}</dl>`;
  }

  /** One alias: its text and language, its note, where it came from, and
   * Remove for the user file's */
  function aliasHtml(a) {
    const source = t(`slang.source.${a.source ?? "seed"}`);
    const remove = a.id
      ? `<button type="button" class="pd-x" data-remove-alias="${escapeHtml(a.id)}" data-source="${escapeHtml(a.source)}" data-text="${escapeHtml(a.text)}" title="${escapeHtml(t("pedia.alias.remove"))}" aria-label="${escapeHtml(t("pedia.alias.remove"))}">×</button>`
      : "";
    return `<li class="pd-alias">
      <span class="pd-alias-head">
        <b lang="${escapeHtml(a.lang)}">${escapeHtml(a.text)}</b>
        <span class="pd-lang">${escapeHtml(a.lang)}</span>
        <span class="pd-source" data-source="${escapeHtml(a.source ?? "seed")}">${escapeHtml(source)}${a.auto ? ` · ${escapeHtml(t("slang.autoApplied"))}` : ""}</span>
        ${remove}
      </span>
      ${a.note ? `<span class="pd-alias-note">${escapeHtml(a.note)}</span>` : ""}
    </li>`;
  }

  function pendingHtml(a) {
    return `<li class="pd-alias is-pending">
      <span class="pd-alias-head">
        <b lang="${escapeHtml(a.lang)}">${escapeHtml(a.text)}</b>
        <span class="pd-lang">${escapeHtml(a.lang)}</span>
        ${a.confidence != null ? `<span class="pd-source">${escapeHtml(t("slang.confidence", { p: Math.round(a.confidence * 100) }))}</span>` : ""}
      </span>
      ${a.note ? `<span class="pd-alias-note">${escapeHtml(a.note)}</span>` : ""}
      ${a.evidence ? `<q class="pd-alias-note">${escapeHtml(a.evidence)}</q>` : ""}
      <span class="pd-row">
        <button type="button" class="mode-toggle cf-mini" data-judge="${escapeHtml(a.id)}" data-status="approved">${escapeHtml(t("slang.approve"))}</button>
        <button type="button" class="mode-toggle cf-mini" data-judge="${escapeHtml(a.id)}" data-status="rejected">${escapeHtml(t("slang.reject"))}</button>
      </span>
    </li>`;
  }

  const relLink = (r) =>
    r.known
      ? `<a href="${entryUrl(r.id)}">${escapeHtml(nameOf(r))}</a>`
      : `<span>${escapeHtml(r.name)}</span>`;

  /** Its broader term, and the terms that name it as theirs, by relation */
  function relatedHtml(e) {
    const rows = [];
    for (const r of e.related.out) {
      rows.push(
        `<div><dt>${escapeHtml(t(`pedia.rel.${r.kind}`))}</dt><dd>${relLink(r)}</dd></div>`,
      );
    }
    for (const kind of RELATIONS) {
      const these = e.related.in.filter((r) => r.kind === kind);
      if (these.length) {
        rows.push(
          `<div><dt>${escapeHtml(t(`pedia.relIn.${kind}`))}</dt><dd>${these.map(relLink).join(", ")}</dd></div>`,
        );
      }
    }
    return rows.length
      ? `<dl class="pd-related">${rows.join("")}</dl>`
      : `<p class="panel-note">${escapeHtml(t("pedia.noRelated"))}</p>`;
  }

  /** A moment as the server writes it (`W2 :50`, `1:20 of the video`) in
   * the page's language */
  const momentLabel = (m) =>
    m.endsWith(" of the video")
      ? t("pedia.ofVideo", { at: m.slice(0, -" of the video".length) })
      : m;

  function quoteHtml(q, i, names) {
    const date = new Date(q.date).toLocaleDateString(i18nLocale(), {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
    const at = q.t_s != null ? clock(q.t_s).replace(/\.\d+$/, "") : "";
    const open = q.review
      ? `<a class="pd-open" href="/cuttlefish/review/${encodeURIComponent(q.review)}?t=${q.t_s}">${escapeHtml(t("pedia.openAt", { at }))}</a>`
      : "";
    return `<li class="pd-quote">
      <blockquote>${highlight(q.text, names)}</blockquote>
      <p class="pd-quote-meta">
        <button type="button" class="src-link pd-reviewer" data-context="${i}" title="${escapeHtml(t("pedia.contextTitle"))}">${escapeHtml(q.reviewer)}</button>
        <span>${escapeHtml(date)}</span>
        <span class="pd-game" data-game="${escapeHtml(q.game)}">${escapeHtml(q.game)}</span>
        ${q.moment ? `<span class="pd-moment">${escapeHtml(momentLabel(q.moment))}</span>` : ""}
        <span class="pd-quote-links">
          <button type="button" class="mode-toggle cf-mini" data-context="${i}">${escapeHtml(t("pedia.context"))}</button>
          ${open}
          <a class="pd-out" href="${escapeHtml(q.url)}" target="_blank" rel="noopener" title="${escapeHtml(t("src.discord"))}">Discord ↗</a>
        </span>
      </p>
    </li>`;
  }

  function noteHtml(n) {
    return `<li class="pd-note">
      <a class="pd-note-q" href="/cuttlefish/knowledge?note=${encodeURIComponent(n.id)}">${escapeHtml(n.question)}</a>
      <p class="pd-note-body">${escapeHtml(n.body.length > 480 ? `${n.body.slice(0, 480)}…` : n.body)}</p>
      <p class="panel-note">${escapeHtml(t("pedia.noteBy", { author: n.author, date: n.date, era: n.era }))}</p>
    </li>`;
  }

  /** A fact card's text (Markdown as the importer writes it) as HTML, for
   * cards imported before they kept their game data: its `# ` title and
   * the internal key left out (the card shows the title), `- ` lines as
   * lists, other lines as paragraphs, and each `## ` section (a weapon's
   * or special's parameters) folded under its heading */
  function factTextHtml(text) {
    const sections = [{ heading: "", lines: [] }];
    for (const line of text.split("\n")) {
      if (line.startsWith("# ") || line.startsWith("Internal key:")) continue;
      if (line.startsWith("## "))
        sections.push({ heading: line.slice(3), lines: [] });
      else sections.at(-1).lines.push(line);
    }
    const body = (lines) => {
      let out = "";
      let items = [];
      const flush = () => {
        if (items.length) out += `<ul>${items.join("")}</ul>`;
        items = [];
      };
      for (const line of lines) {
        if (line.startsWith("- ")) {
          items.push(`<li>${escapeHtml(line.slice(2))}</li>`);
          continue;
        }
        flush();
        if (line.trim()) out += `<p>${escapeHtml(line)}</p>`;
      }
      flush();
      return out;
    };
    return sections
      .map(({ heading, lines }) =>
        heading
          ? `<details class="pd-fact-more"><summary>${escapeHtml(heading)}</summary>${body(lines)}</details>`
          : body(lines),
      )
      .join("");
  }

  /** A number in the page's language, at most `digits` decimals */
  const decimal = (x, digits = 2) =>
    Number(x).toLocaleString(i18nLocale(), { maximumFractionDigits: digits });

  /** A value of a summary in its unit (cuttlefish::stats::Unit): damage
   * and HP as players see them, ink in percent of the tank, frames with
   * their seconds at 60 per second, distances in game units */
  function unitHtml(x, unit) {
    const n = escapeHtml(decimal(x, 3));
    const small = (key, values) =>
      `<span class="pd-unit">${escapeHtml(t(key, values))}</span>`;
    switch (unit) {
      case "percent":
        return `${n}${small("pedia.unit.percent")}`;
      case "percent_per_frame":
        return `${n}${small("pedia.unit.percentPerFrame")}`;
      case "frames":
        return `${n} ${small("pedia.unit.frames", { s: decimal(x / 60) })}`;
      case "units":
        return `${n} ${small("pedia.unit.units")}`;
      default:
        return n;
    }
  }

  /** A game key or English name of a summary in the page's language:
   * the glossary's name when the server found the term, else the key with
   * spaces between its words (`ZakoSpeedCoef` reads "Zako speed coef") */
  function summaryName(s, key) {
    const named = s.names?.[key];
    const names = named?.names ?? {};
    const name = (i18nLang() === "zh" && names.zh) || names.en;
    if (name) return name;
    const words = key
      .replace(/_/g, " ")
      .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
      .trim();
    return words.charAt(0).toUpperCase() + words.slice(1).toLowerCase();
  }

  /** A statistic's label: its name and what part it is of */
  function statLabel(s, row) {
    const level = s.kind === "level";
    const king = row.stat.endsWith("HPCoef") && row.stat.slice(0, -6);
    const stat = !level
      ? t(`pedia.stat.${row.stat}`)
      : king && s.names?.[king]
        ? t("pedia.stat.hpCoef", { name: summaryName(s, king) })
        : summaryName(s, row.stat);
    if (!row.group || level) return stat;
    const group =
      row.stat === "hp"
        ? t(`pedia.part.${row.group}`)
        : t(`pedia.group.${row.group}`);
    return t("pedia.stat.of", { stat, group });
  }

  /** A row's value: its text (a Salmonid's category, yes or no) or its
   * numbers, the largest and the smallest */
  function rowValueHtml(row) {
    if (row.text != null) {
      const key =
        row.stat === "category" ? `pedia.cat.${row.text}` : `pedia.${row.text}`;
      return escapeHtml(t(key));
    }
    const [max, min] = row.values.map((x) => unitHtml(x, row.unit));
    return min === undefined
      ? max
      : `${max}<span class="pd-unit pd-to">${escapeHtml(t("pedia.stat.downTo"))}</span>${min}`;
  }

  function statRowHtml(s, row) {
    return `<div class="pd-stat" title="${escapeHtml(row.from.join("\n"))}">
      <dt>${escapeHtml(statLabel(s, row))}</dt>
      <dd>${rowValueHtml(row)}</dd>
    </div>`;
  }

  /** A summary's statistics; a hazard level's under the occurrence (or
   * table) each sets */
  function statsHtml(s) {
    if (!s.rows.length) return "";
    if (s.kind !== "level")
      return `<dl class="pd-stats">${s.rows.map((r) => statRowHtml(s, r)).join("")}</dl>`;
    const groups = [...new Set(s.rows.map((r) => r.group ?? ""))];
    return groups
      .map((g) => {
        const rows = s.rows.filter((r) => (r.group ?? "") === g);
        return `<div class="pd-stat-group">
          ${g ? `<h3>${escapeHtml(summaryName(s, g))}</h3>` : ""}
          <dl class="pd-stats">${rows.map((r) => statRowHtml(s, r)).join("")}</dl>
        </div>`;
      })
      .join("");
  }

  /** Damage by distance, or a King Salmonid's HP by hazard level */
  function statTableHtml(table) {
    const falloff = table.stat === "falloff";
    const caption = falloff
      ? table.group
        ? t("pedia.stat.of", {
            stat: t("pedia.table.falloff"),
            group: t(`pedia.group.${table.group}`),
          })
        : t("pedia.table.falloff")
      : t("pedia.table.hpCoef");
    const [a, b] = falloff
      ? [t("pedia.table.upTo"), t("pedia.stat.damage")]
      : [t("pedia.table.hazard"), t("pedia.table.coef")];
    const rows = table.rows
      .map(
        ([x, y]) =>
          `<tr><td>${escapeHtml(falloff ? decimal(x) : `${decimal(x)}%`)}</td><td>${escapeHtml(decimal(y))}</td></tr>`,
      )
      .join("");
    return `<table class="pd-stat-table" title="${escapeHtml(table.from)}">
      <caption>${escapeHtml(caption)}</caption>
      <thead><tr><th>${escapeHtml(a)}</th><th>${escapeHtml(b)}</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>`;
  }

  /** Hits taken at the most damage per hit */
  const HITS_SHOWN = 20;

  /** How many hits the Salmonids take, as links to their entries: those
   * one hit defeats, then the others up to HITS_SHOWN hits */
  function hitsHtml(s) {
    if (!s.hits?.length) return "";
    const chip = (h) => {
      const named = s.names?.[h.name];
      const name = summaryName(s, h.name);
      const label = h.part ? t(`pedia.hit.${h.part}`, { name }) : name;
      const count =
        h.hits > 1
          ? `<span class="num">×${escapeHtml(decimal(h.hits))}</span>`
          : "";
      const title = t("pedia.hit.hp", { hp: decimal(h.hp) });
      return named
        ? `<a class="pd-hit" href="${entryUrl(named.id)}" title="${escapeHtml(title)}">${escapeHtml(label)}${count}</a>`
        : `<span class="pd-hit" title="${escapeHtml(title)}">${escapeHtml(label)}${count}</span>`;
    };
    const one = s.hits.filter((h) => h.hits === 1);
    const more = s.hits.filter((h) => h.hits > 1 && h.hits <= HITS_SHOWN);
    const line = (key, list) =>
      list.length
        ? `<p class="pd-hits"><span class="pd-hits-label">${escapeHtml(t(key))}</span> ${list.map(chip).join("")}</p>`
        : "";
    return `<div class="pd-hits-box">
      ${line("pedia.hit.one", one)}
      ${line(one.length ? "pedia.hit.more" : "pedia.hit.hits", more)}
      <p class="panel-note">${t("pedia.hit.note", {
        source: `<a href="${escapeHtml(s.hp_url)}" target="_blank" rel="noopener">Inkipedia</a>`,
      })}</p>
    </div>`;
  }

  /** Every parameter as the game has it, with the internal key */
  function rawHtml(s) {
    // A hazard level's key is its difficulty, among its parameters
    const keys = [
      ...(s.kind === "level" ? [] : [[t("pedia.raw.key"), s.key]]),
      ...(s.versus ? [[t("pedia.raw.versus"), s.versus]] : []),
    ];
    return `<details class="pd-fact-more pd-raw">
      <summary>${escapeHtml(t("pedia.raw", { n: s.raw.length }))}</summary>
      ${["weapon", "special"].includes(s.kind) ? `<p class="panel-note">${escapeHtml(t("pedia.raw.note"))}</p>` : ""}
      <table class="pd-raw-table"><tbody>
        ${[...keys, ...s.raw].map(([k, v]) => `<tr><th scope="row">${escapeHtml(k)}</th><td>${escapeHtml(v)}</td></tr>`).join("")}
      </tbody></table>
    </details>`;
  }

  /** A card's title: from its summary in the page's language, else the
   * importer's */
  function factTitle(c) {
    const s = c.summary;
    if (!s) return c.title;
    return s.kind === "level"
      ? t("pedia.fact.level", { hazard: decimal(Number(s.key) / 5) })
      : t(`pedia.fact.${s.kind}`);
  }

  /** A card read from its game data: what matters in Salmon Run in
   * players' units, the Eggstra Work events, and the raw parameters
   * folded */
  function summaryHtml(s) {
    const tags = [
      s.grizzco ? t("pedia.fact.grizzco") : "",
      t("pedia.fact.version", { version: s.version }),
    ].filter(Boolean);
    return `<p class="pd-fact-tags">${tags.map((x) => `<span class="pd-kind">${escapeHtml(x)}</span>`).join("")}</p>
      ${statsHtml(s)}
      ${s.tables?.length ? `<div class="pd-stat-tables">${s.tables.map(statTableHtml).join("")}</div>` : ""}
      ${hitsHtml(s)}
      ${s.hp_url && !s.hits?.length ? `<p class="panel-note">${t("pedia.hpFrom", { source: `<a href="${escapeHtml(s.hp_url)}" target="_blank" rel="noopener">Inkipedia</a>` })}</p>` : ""}
      ${s.events?.length ? `<p class="pd-fact-events">${escapeHtml(t("pedia.fact.events", { list: s.events.join(", ") }))}</p>` : ""}
      ${s.raw.length || s.key ? rawHtml(s) : ""}`;
  }

  /** A fact card; `folded`, only its title until opened */
  function cardFactHtml(c, folded) {
    const title = factTitle(c);
    const inner = `${c.summary ? summaryHtml(c.summary) : factTextHtml(c.text)}
      ${c.attribution ? `<p class="panel-note">${c.url ? `<a href="${escapeHtml(c.url)}" target="_blank" rel="noopener">${escapeHtml(c.attribution)}</a>` : escapeHtml(c.attribution)}</p>` : ""}`;
    return folded
      ? `<li class="pd-fact"><details><summary><b>${escapeHtml(title)}</b></summary>${inner}</details></li>`
      : `<li class="pd-fact"><b>${escapeHtml(title)}</b>${inner}</li>`;
  }

  function questionHtml(q) {
    const text = i18nLang() === "zh" ? q.zh : q.en;
    return `<li class="pd-question">
      <span>${escapeHtml(text)}</span>
      <span class="pd-row">
        ${q.reference ? `<a class="mode-toggle cf-mini" href="/cuttlefish/knowledge?note=${encodeURIComponent(q.reference)}">${escapeHtml(t("pedia.answered"))}</a>` : ""}
        <button type="button" class="mode-toggle cf-mini" data-ask="${escapeHtml(text)}">${escapeHtml(t("pedia.askThis"))}</button>
      </span>
    </li>`;
  }

  /** The fact cards of game data, when the term has some: a few open,
   * many (an Eggstra Work's events and waves) folded to their titles,
   * the first FACTS_SHOWN until "Show all" */
  function factsHtml(e) {
    if (!e.cards.length) return "";
    const folded = e.cards.length > FACTS_OPEN;
    // Hazard levels in their order (the server sorts by title: 0%, 100%, 20%)
    const level = (c) =>
      c.summary?.kind === "level" ? Number(c.summary.key) : -1;
    const cards = [...e.cards].sort((a, b) => level(a) - level(b));
    const shown = pd.allFacts ? cards : cards.slice(0, FACTS_SHOWN);
    return `<section class="panel pd-part" aria-labelledby="pd-h-facts">
      <header class="panel-head pd-part-head"><h2 id="pd-h-facts">${escapeHtml(t("pedia.facts"))}</h2>
        <span class="panel-note">${escapeHtml(t("pedia.factsNote"))}</span></header>
      <ul class="pd-facts">${shown.map((c) => cardFactHtml(c, folded)).join("")}</ul>
      ${shown.length < e.cards.length ? `<button type="button" class="mode-toggle" data-all-facts>${escapeHtml(t("pedia.allFacts", { n: e.cards.length }))}</button>` : ""}
    </section>`;
  }

  /** Asks for an entry's fact cards again while the server is still
   * reading them (`cards_pending`), and fills them in once they are there;
   * stops when another entry is shown */
  async function fillCards(entry) {
    if (pd.entry !== entry) return;
    let fresh;
    try {
      fresh = await api(
        `pedia/${encodeURIComponent(entry.term.id)}?quotes=${pd.quotes}`,
      );
    } catch {
      return;
    }
    if (pd.entry !== entry) return;
    entry.cards = fresh.cards;
    entry.cards_pending = fresh.cards_pending;
    const box = slot("facts");
    if (box) box.innerHTML = factsHtml(entry);
    if (entry.cards_pending) setTimeout(() => fillCards(entry), CARDS_MS);
  }

  /** The entry's article */
  function drawEntry() {
    const e = pd.entry;
    if (!e) return;
    const term = e.term;
    const shown = {
      ...term,
      names: Object.fromEntries(
        Object.entries(term.forms).map(([l, n]) => [l, n[0]]),
      ),
      name: e.name,
    };
    const title = nameOf(shown);
    const icon = e.icon
      ? `<img class="pd-entry-img" src="/api/cuttlefish/knowledge/thumb?id=${encodeURIComponent(e.icon)}" alt="" />`
      : classIcon(term)
        ? iconSvg(classIcon(term), "pd-entry-icon")
        : "";
    const alt = otherNames(shown);
    const names = [
      ...new Set([
        ...Object.values(term.forms).flat(),
        ...(term.aliases ?? [])
          .filter((a) => a.status === "approved")
          .map((a) => a.text),
      ]),
    ];
    const sectionName = t(`pedia.section.${e.section}`);
    const edited = e.override || e.user_term?.source === "user";
    const approved = e.aliases.filter((a) => a.status === "approved");
    $("pd-entry").dataset.section = e.section;
    $("pd-entry").innerHTML = `
      <nav class="pd-crumbs" aria-label="${escapeHtml(t("pedia.title"))}">
        <a href="/cuttlefish/pedia">${escapeHtml(t("pedia.title"))}</a>
        <span aria-hidden="true">›</span>
        <a href="${sectionUrl(e.section)}">${escapeHtml(sectionName)}</a>
      </nav>
      <header class="panel pd-entry-head" data-section="${e.section}">
        ${icon ? `<div class="pd-entry-art">${icon}</div>` : ""}
        <div class="pd-entry-titles">
          <p class="pd-kicker">
            <span>${escapeHtml(sectionName)}</span>
            ${term.kind ? `<span class="pd-kind">${escapeHtml(kindLabel(term.kind))}</span>` : ""}
            ${gamesHtml(e.games, true)}
          </p>
          <h1 class="pd-entry-title">${escapeHtml(title)}</h1>
          ${alt.length ? `<p class="pd-entry-alt">${escapeHtml(alt.join(" · "))}</p>` : ""}
          <p class="pd-facets">${facetsHtml(e.facets)}${e.in_scope ? "" : `<span class="pd-facet">${escapeHtml(t("pedia.outOfScope"))}</span>`}</p>
        </div>
      </header>
      <div class="pd-entry-body">
        <div class="pd-main">
          <section class="panel pd-def" id="pd-def">
            ${term.definition ? `<p class="pd-definition">${escapeHtml(term.definition)}</p>` : `<p class="pd-definition is-empty">${escapeHtml(t("pedia.noDefinition"))}</p>`}
            ${edited ? `<p class="panel-note pd-edited">${escapeHtml(t("pedia.editedByYou"))}${e.override ? ` <button type="button" class="src-link" data-reset>${escapeHtml(t("pedia.reset"))}</button>` : ""}</p>` : ""}
            <div class="pd-row pd-actions">
              <button type="button" class="btn pd-ask" data-ask-about>
                <svg class="cf-view-icon" viewBox="0 0 24 24" aria-hidden="true"><use href="/icons/app-cuttlefish.svg#i" /></svg>
                ${escapeHtml(t("pedia.ask"))}
              </button>
              <button type="button" class="mode-toggle" data-edit-def>${escapeHtml(t("pedia.edit"))}</button>
              <button type="button" class="mode-toggle" data-add-note>${escapeHtml(t("pedia.addNote"))}</button>
              <button type="button" class="mode-toggle pd-flag" data-flag>${escapeHtml(t("pedia.flag"))}</button>
            </div>
            <div class="pd-form-slot" data-slot="def"></div>
          </section>

          <div class="pd-facts-slot" data-slot="facts">${factsHtml(e)}</div>

          <section class="panel pd-part pd-wild" aria-labelledby="pd-h-wild">
            <header class="panel-head pd-part-head">
              <h2 id="pd-h-wild">${escapeHtml(t("pedia.wild"))}</h2>
              <span class="panel-note">${escapeHtml(e.mentions ? t("pedia.wildNote", { n: e.mentions }) : t("pedia.wildNone"))}</span>
            </header>
            ${e.quotes.length ? `<ol class="pd-quotes">${e.quotes.map((q, i) => quoteHtml(q, i, names)).join("")}</ol>` : ""}
            ${e.quotes.length < Math.min(e.mentions, 50) && e.quotes.length >= pd.quotes ? `<button type="button" class="mode-toggle" data-more>${escapeHtml(t("pedia.more"))}</button>` : ""}
          </section>

          <section class="panel pd-part" id="pd-examples" aria-labelledby="pd-h-examples" hidden></section>
          ${
            e.notes.length
              ? `<section class="panel pd-part" aria-labelledby="pd-h-notes">
            <header class="panel-head pd-part-head"><h2 id="pd-h-notes">${escapeHtml(t("pedia.notes"))}</h2></header>
            <ul class="pd-notes">${e.notes.map(noteHtml).join("")}</ul>
          </section>`
              : ""
          }
          ${
            e.questions.length
              ? `<section class="panel pd-part" aria-labelledby="pd-h-questions">
            <header class="panel-head pd-part-head"><h2 id="pd-h-questions">${escapeHtml(t("pedia.questions"))}</h2>
              <span class="panel-note">${escapeHtml(t("pedia.questionsNote"))}</span></header>
            <ul class="pd-questions">${e.questions.map(questionHtml).join("")}</ul>
          </section>`
              : ""
          }
        </div>

        <aside class="pd-side">
          <section class="panel pd-box" aria-labelledby="pd-h-names">
            <header class="panel-head"><h2 id="pd-h-names">${escapeHtml(t("pedia.names"))}</h2></header>
            ${namesHtml(term) || `<p class="panel-note">${escapeHtml(t("pedia.noNames"))}</p>`}
          </section>
          <section class="panel pd-box" aria-labelledby="pd-h-slang">
            <header class="panel-head"><h2 id="pd-h-slang">${escapeHtml(t("pedia.slang"))}</h2></header>
            ${approved.length ? `<ul class="pd-aliases">${approved.map(aliasHtml).join("")}</ul>` : `<p class="panel-note">${escapeHtml(t("pedia.noSlang"))}</p>`}
            ${e.pending.length ? `<h3>${escapeHtml(t("slang.pending"))}</h3><ul class="pd-aliases">${e.pending.map(pendingHtml).join("")}</ul>` : ""}
            <button type="button" class="mode-toggle cf-mini" data-add-alias>${escapeHtml(t("alias.add"))}</button>
            <div class="pd-form-slot" data-slot="alias"></div>
          </section>
          <section class="panel pd-box" aria-labelledby="pd-h-related">
            <header class="panel-head"><h2 id="pd-h-related">${escapeHtml(t("pedia.related"))}</h2></header>
            ${relatedHtml(e)}
            <button type="button" class="mode-toggle cf-mini" data-edit-rel>${escapeHtml(t(term.related ? "pedia.relChange" : "pedia.relSet"))}</button>
            <div class="pd-form-slot" data-slot="rel"></div>
          </section>
        </aside>
      </div>`;
    drawExamples(e);
  }

  /** Examples the user recorded of the entry's technique: the markers of
   * the sessions (techniques.js) with its term id or one of its names, each
   * a link to the Inkspector at its start. Shown for movement techniques,
   * and for any entry that has some. */
  async function drawExamples(e) {
    const id = e.term.id;
    let markers;
    try {
      markers = await loadAllMarkers();
    } catch {
      return;
    }
    const box = $("pd-examples");
    if (!box || pd.entry !== e) return;
    const names = new Set(Object.values(e.term.forms).flat());
    const found = markers.filter(
      (m) => m.term === id || (!m.term && names.has(m.label)),
    );
    if (!found.length && e.section !== "movement") return;
    const date = (ms) =>
      new Date(ms).toLocaleString(i18nLocale(), {
        dateStyle: "medium",
        timeStyle: "short",
      });
    const items = found.map((m) => {
      const state = { s: m.session };
      if (m.seg != null) state.seg = m.seg;
      if (m.n != null) state.n = m.n;
      const seconds = ((m.t_end_ms - m.t_start_ms) / 1000).toFixed(1);
      return `<li><a href="${escapeHtml(appUrl("inspect", state))}">${escapeHtml(date(m.t_start_ms))}</a>
        <span class="panel-note">${escapeHtml(m.label)} · ${seconds} s · ${escapeHtml(m.session)}</span></li>`;
    });
    box.innerHTML = `
      <header class="panel-head pd-part-head"><h2 id="pd-h-examples">${escapeHtml(t("pedia.examples"))}</h2>
        <span class="panel-note">${escapeHtml(found.length ? t("pedia.examplesNote", { n: found.length }) : t("pedia.examplesNone"))}</span></header>
      ${items.length ? `<ul class="pd-examples">${items.join("")}</ul>` : ""}`;
    box.hidden = false;
  }

  // ------------------------------------------------------------ editing

  /** A note under a form, or the entry's */
  function formNote(form, text) {
    const note = form?.querySelector("[data-form-note]");
    if (note) note.textContent = text;
  }

  /** The entry again after an edit (the index too, when shown next) */
  async function changed() {
    pd.list = null;
    await loadEntry(pd.id, { keepScroll: true });
  }

  function slot(name) {
    return $("pd-entry").querySelector(`[data-slot="${name}"]`);
  }

  /** The definition and kind, edited in place; `flag` asks what is wrong */
  function openDefinition(flag = false) {
    const e = pd.entry;
    const form = document.createElement("form");
    form.className = "pd-form";
    form.innerHTML = `
      ${flag ? `<p class="panel-note">${escapeHtml(t("pedia.flagHint"))}</p>` : ""}
      <label class="cf-alias-field"><span>${escapeHtml(t("pedia.definition"))}</span>
        <textarea class="select" data-definition rows="4" maxlength="300">${escapeHtml(e.term.definition ?? "")}</textarea></label>
      <label class="cf-alias-field"><span>${escapeHtml(t("pedia.kindLabel"))}</span>
        <input class="select" data-kind list="pd-kinds" value="${escapeHtml(e.term.kind ?? "")}" /></label>
      <datalist id="pd-kinds">${KINDS.map((k) => `<option value="${k}">${escapeHtml(kindLabel(k))}</option>`).join("")}</datalist>
      <div class="cf-alias-actions">
        <button type="submit" class="btn">${escapeHtml(t("alias.save"))}</button>
        <button type="button" class="mode-toggle" data-form-cancel>${escapeHtml(t("alias.cancel"))}</button>
        <span class="panel-note" data-form-note></span>
      </div>`;
    form.onsubmit = async (event) => {
      event.preventDefault();
      const fields = {};
      const definition = form.querySelector("[data-definition]").value.trim();
      const kind = form.querySelector("[data-kind]").value.trim();
      if (definition !== (e.term.definition ?? ""))
        fields.definition = definition;
      if (kind !== (e.term.kind ?? "")) fields.kind = kind;
      if (!Object.keys(fields).length) return form.remove();
      try {
        await editTerm(e.term.id, fields);
        await changed();
      } catch (error) {
        formNote(form, t("pedia.editFailed", { error: error.message }));
      }
    };
    slot("def").replaceChildren(form);
    form.querySelector("[data-definition]").focus();
  }

  /** The relation to a broader term, picked as you type */
  function openRelation() {
    const e = pd.entry;
    const current = e.term.related;
    const form = document.createElement("form");
    form.className = "pd-form";
    form.innerHTML = `
      <label class="cf-alias-field"><span>${escapeHtml(t("pedia.relKind"))}</span>
        <select class="select" data-rel-kind>
          ${RELATIONS.map((k) => `<option value="${k}" ${current?.kind === k ? "selected" : ""}>${escapeHtml(t(`pedia.rel.${k}`))}</option>`).join("")}
          ${current ? `<option value="">${escapeHtml(t("pedia.relNone"))}</option>` : ""}
        </select></label>
      <label class="cf-alias-field cf-term-pick"><span>${escapeHtml(t("alias.term"))}</span>
        <input class="select" data-term-search autocomplete="off" value="${escapeHtml(current?.name ?? "")}" placeholder="${escapeHtml(t("alias.termSearch"))}" />
        <ul class="cf-term-results" data-term-results hidden></ul></label>
      <input type="hidden" data-term value="${escapeHtml(current?.term ?? "")}" />
      <div class="cf-alias-actions">
        <button type="submit" class="btn">${escapeHtml(t("alias.save"))}</button>
        <button type="button" class="mode-toggle" data-form-cancel>${escapeHtml(t("alias.cancel"))}</button>
        <span class="panel-note" data-form-note></span>
      </div>`;
    const search = form.querySelector("[data-term-search]");
    const results = form.querySelector("[data-term-results]");
    let timer = 0;
    search.addEventListener("input", () => {
      form.querySelector("[data-term]").value = "";
      clearTimeout(timer);
      timer = setTimeout(async () => {
        const q = search.value.trim();
        if (!q) {
          results.hidden = true;
          return;
        }
        let data;
        try {
          data = await api(`knowledge/terms?${new URLSearchParams({ q })}`);
        } catch {
          return;
        }
        if (search.value.trim() !== q) return;
        results.innerHTML = data.terms.length
          ? data.terms
              .filter((term) => term.id !== e.term.id)
              .map((term) => {
                const label =
                  term.forms[i18nLang()]?.[0] ?? term.forms.en?.[0] ?? term.id;
                return `<li><button type="button" data-pick-term="${escapeHtml(term.id)}" data-label="${escapeHtml(label)}"><b>${escapeHtml(label)}</b>${term.kind ? ` <span class="cf-kind">${escapeHtml(kindLabel(term.kind))}</span>` : ""}</button></li>`;
              })
              .join("")
          : `<li class="panel-note">${escapeHtml(t("alias.noMatch"))}</li>`;
        results.hidden = false;
      }, SEARCH_MS);
    });
    results.addEventListener("click", (event) => {
      const pick = event.target.closest("[data-pick-term]");
      if (!pick) return;
      form.querySelector("[data-term]").value = pick.dataset.pickTerm;
      search.value = pick.dataset.label;
      results.hidden = true;
    });
    form.onsubmit = async (event) => {
      event.preventDefault();
      const kind = form.querySelector("[data-rel-kind]").value;
      const term = form.querySelector("[data-term]").value;
      if (kind && !term) return formNote(form, t("alias.pickTerm"));
      try {
        await editTerm(e.term.id, { relation: kind ? { kind, term } : null });
        await changed();
      } catch (error) {
        formNote(form, t("pedia.editFailed", { error: error.message }));
      }
    };
    slot("rel").replaceChildren(form);
    search.focus();
  }

  /** Teaches an alias of the entry's term */
  function openAlias() {
    const e = pd.entry;
    const lang = i18nLang() === "zh" ? "zh" : "en";
    const form = document.createElement("form");
    form.className = "pd-form";
    form.innerHTML = `
      <label class="cf-alias-field"><span>${escapeHtml(t("alias.text"))}</span>
        <input class="select" data-text required maxlength="40" /></label>
      <label class="cf-alias-field"><span>${escapeHtml(t("alias.lang"))}</span>
        <select class="select" data-lang>${LANGUAGES.map(([c, n]) => `<option value="${c}" ${c === lang ? "selected" : ""}>${escapeHtml(n)}</option>`).join("")}</select></label>
      <label class="cf-alias-field"><span>${escapeHtml(t("alias.note"))}</span>
        <input class="select" data-note maxlength="300" /></label>
      <div class="cf-alias-actions">
        <button type="submit" class="btn">${escapeHtml(t("alias.save"))}</button>
        <button type="button" class="mode-toggle" data-form-cancel>${escapeHtml(t("alias.cancel"))}</button>
        <span class="panel-note" data-form-note></span>
      </div>`;
    form.onsubmit = async (event) => {
      event.preventDefault();
      const field = (name) => form.querySelector(`[data-${name}]`).value;
      try {
        await api("knowledge/slang/add", "POST", {
          term: e.term.id,
          text: field("text"),
          lang: field("lang"),
          note: field("note"),
        });
        await changed();
      } catch (error) {
        formNote(form, t("pedia.editFailed", { error: error.message }));
      }
    };
    slot("alias").replaceChildren(form);
    form.querySelector("[data-text]").focus();
  }

  /** Asks Cuttlefish in the library's chat bar */
  function ask(question) {
    navigate("/cuttlefish");
    const input = $("cf-entry-text");
    input.value = question;
    input.dispatchEvent(new Event("input"));
    input.focus();
  }

  $("pd-entry").addEventListener("click", async (event) => {
    const e = pd.entry;
    if (!e) return;
    const on = (selector) => event.target.closest(selector);
    const button = on("button");
    if (!button) return;
    if (on("[data-form-cancel]")) return button.closest("form").remove();
    if (on("[data-edit-def]")) return openDefinition();
    if (on("[data-edit-rel]")) return openRelation();
    if (on("[data-add-alias]")) return openAlias();
    if (on("[data-ask-about]"))
      return ask(
        t("pedia.askQuestion", {
          name: nameOf({
            ...e.term,
            name: e.name,
            names: { zh: e.term.forms.zh?.[0] },
          }),
        }),
      );
    if (on("[data-ask]")) return ask(button.dataset.ask);
    const context = on("[data-context]");
    if (context) {
      const q = e.quotes[context.dataset.context];
      const names = [
        ...Object.values(e.term.forms).flat(),
        ...e.aliases.filter((a) => a.status === "approved").map((a) => a.text),
      ];
      return window.cuttlefishSource.open(context, {
        url: q.url,
        t_s: q.t_s,
        names,
      });
    }
    if (on("[data-all-facts]")) {
      pd.allFacts = true;
      slot("facts").innerHTML = factsHtml(e);
      return;
    }
    if (on("[data-more]")) {
      pd.quotes += MORE_QUOTES;
      return loadEntry(pd.id, { keepScroll: true });
    }
    if (on("[data-add-note]")) {
      const name = nameOf({
        ...e.term,
        name: e.name,
        names: { zh: e.term.forms.zh?.[0] },
      });
      return window.cuttlefishNotes?.edit({
        question: t("pedia.noteQuestion", { name }),
        terms: [e.term.id],
        tags: [e.section],
        from: `pedia ${e.term.id}`,
        onSaved: () => loadEntry(pd.id, { keepScroll: true }),
      });
    }
    try {
      if (on("[data-reset]")) {
        await resetTerm(e.term.id);
        return await changed();
      }
      if (on("[data-flag]")) {
        // A new term of the user file is rejected (never suggested
        // again); a glossary term is corrected in place
        if (!e.user_term) return openDefinition(true);
        if (!confirm(t("pedia.flagAsk", { name: e.name }))) return;
        await api("knowledge/slang/undo", "POST", { id: e.user_term.id });
        pd.rejected = { id: e.user_term.id, name: e.name };
        pd.list = null;
        return navigate(sectionUrl(e.section));
      }
      const remove = on("[data-remove-alias]");
      if (remove) {
        const { removeAlias: id, source, text } = remove.dataset;
        if (!confirm(t("pedia.alias.removeAsk", { text }))) return;
        // The user's own alias goes; a suggestion is rejected, so it is
        // never suggested again
        if (source === "user")
          await api("knowledge/slang/delete", "POST", { id });
        else await api("knowledge/slang/undo", "POST", { id });
        return await changed();
      }
      const judge = on("[data-judge]");
      if (judge) {
        await api("knowledge/slang/edit", "POST", {
          id: judge.dataset.judge,
          status: judge.dataset.status,
        });
        return await changed();
      }
    } catch (error) {
      alert(t("pedia.editFailed", { error: error.message }));
    }
  });

  // The source popover marks a term's names in a quote's conversation too
  window.cuttlefishPediaMark = highlight;

  // -------------------------------------------------------------- routing

  /** Shows the index or an entry, as the address says */
  function route(state) {
    const id = state.get("term") ?? "";
    pd.section = state.get("section") ?? "";
    $("pd-browse").hidden = Boolean(id);
    $("pd-entry").hidden = !id;
    if (id) {
      if (id !== pd.id) {
        pd.quotes = QUOTES;
        pd.allFacts = false;
      }
      pd.id = id;
      if (pd.entry?.term.id !== id || !pd.entry) loadEntry(id);
      else drawEntry();
      return;
    }
    pd.id = "";
    pd.entry = null;
    if (pd.list) drawIndex();
    else loadList();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    pd.shown = app === "cuttlefish" && state.get("view") === "pedia";
    $("cf-pedia").hidden = !pd.shown;
    if (pd.shown) route(state);
  });

  // What is drawn from JavaScript follows the language
  window.addEventListener("lang-change", () => {
    if (!pd.shown) return;
    if (pd.entry && pd.id) drawEntry();
    else drawIndex();
  });
})();
