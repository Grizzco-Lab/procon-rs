// Cuttlefish's knowledge view (#cuttlefish/view=knowledge): what the store
// holds, search, questions and translations, imports and the glossary,
// through /api/cuttlefish/knowledge/... (see src/knowledge.rs). Runs after
// cuttlefish.js, which hides its library and player for this view, and uses
// the helpers of app.js and inspect.js ($, escapeHtml).
"use strict";

(() => {
  /** How often a running import is asked about, in ms */
  const POLL_MS = 1000;
  /** Log lines shown per import */
  const LOG_SHOWN = 8;
  /** Source kinds as shown */
  const SOURCES = {
    web: "Web",
    wiki: "Wiki",
    guide: "Guide",
    video: "Video",
    "discord-vod-review": "#vod-review",
    discord: "Discord",
    file: "File",
  };

  const k = {
    /** Whether the view is shown */
    shown: false,
    stats: null,
    documents: [],
    /** Import form's kind */
    kind: "web",
    pollTimer: null,
    /** Whether an import was running at the last look */
    wasRunning: false,
  };

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-knowledge-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-knowledge-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  /** GET or POST JSON; throws with the server's message and status */
  async function api(path, body) {
    const response = await fetch(
      `/api/cuttlefish/knowledge/${path}`,
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          },
    );
    const data = await response.json();
    if (!response.ok) {
      const error = new Error(data.error ?? response.statusText);
      error.status = response.status;
      throw error;
    }
    return data;
  }

  function note(id, message) {
    const el = $(id);
    el.hidden = !message;
    el.textContent = message ?? "";
  }

  const sourceName = (source) => SOURCES[source] ?? source;

  /** A title linked to its url when it has one */
  function titleLink(title, url) {
    const text = escapeHtml(title);
    return url
      ? `<a href="${escapeHtml(url)}" target="_blank" rel="noopener">${text}</a>`
      : text;
  }

  // --------------------------------------------------------------- stats

  async function loadStats() {
    let stats;
    try {
      stats = await api("stats");
    } catch (error) {
      $("k-loading").hidden = true;
      note(
        "k-stats-error",
        `The knowledge store cannot open: ${error.message}`,
      );
      return;
    }
    k.stats = stats;
    note("k-stats-error", null);
    $("k-loading").hidden = true;
    $("k-tiles").hidden = false;
    $("k-data").textContent = stats.data;
    $("k-documents").textContent = stats.documents;
    $("k-sources-note").textContent =
      stats.sources
        .map((s) => `${sourceName(s.source)} ${s.documents}`)
        .join(" · ") || "nothing imported yet";
    $("k-chunks").textContent = stats.chunks;
    $("k-embedder").textContent = stats.embedder;
    $("k-terms").textContent = stats.glossary_terms;
    $("k-glossary-note").textContent = stats.own_glossary
      ? "glossary.toml in the data folder"
      : "the crate's seed glossary";
    $("k-digest").textContent = stats.digest ? "Yes" : "No";
    $("k-model").textContent = stats.model;
    const keyChip = (name, set, what) =>
      `<span class="chip" data-level="${set ? "good" : "off"}" title="${escapeHtml(what)}"><span class="chip-dot"></span><span class="chip-text">${name} ${set ? "set" : "not set"}</span></span>`;
    $("k-keys").innerHTML =
      keyChip(
        "ANTHROPIC_API_KEY",
        stats.anthropic_key,
        "Needed to ask and translate",
      ) +
      keyChip(
        "DISCORD_BOT_TOKEN",
        stats.discord_token,
        "Needed to import through a Discord bot",
      );
    $("k-key-note").hidden = stats.anthropic_key;
    for (const id of ["k-ask", "k-translate"]) {
      $(id).disabled = !stats.anthropic_key;
    }
    $("k-token-note").textContent = stats.discord_token
      ? ""
      : "(needs DISCORD_BOT_TOKEN where the studio runs)";
  }

  // -------------------------------------------------------------- search

  $("k-search-form").onsubmit = async (event) => {
    event.preventDefault();
    const q = $("k-q").value.trim();
    if (!q) return;
    note("k-search-error", null);
    const list = $("k-hits");
    list.innerHTML = `<li class="panel-note">Searching…</li>`;
    let data;
    try {
      data = await api(
        `search?${new URLSearchParams({ q, k: $("k-k").value })}`,
      );
    } catch (error) {
      list.replaceChildren();
      return note("k-search-error", error.message);
    }
    list.replaceChildren();
    if (!data.hits.length) {
      list.innerHTML = `<li class="panel-note">Nothing found: the store is empty. Import something first.</li>`;
    }
    for (const hit of data.hits) {
      const li = document.createElement("li");
      li.className = "k-hit";
      const place = hit.heading
        ? `${titleLink(hit.title, hit.url)} › ${escapeHtml(hit.heading)}`
        : titleLink(hit.title, hit.url);
      li.innerHTML = `
        <div class="k-hit-head"><span class="chip num">${hit.score.toFixed(3)}</span><span class="cf-kind">${escapeHtml(sourceName(hit.source))}</span><span class="k-hit-title">${place}</span></div>
        <p class="k-hit-text">${escapeHtml(hit.text)}</p>
        <span class="panel-note">${escapeHtml(hit.license ?? "license unknown")}${hit.language ? ` · ${escapeHtml(hit.language)}` : ""}</span>`;
      list.append(li);
    }
  };

  // ---------------------------------------------------- ask and translate

  /** The message for a failed model call */
  function modelError(error) {
    return error.status === 501
      ? "ANTHROPIC_API_KEY is not set where the studio runs."
      : error.message;
  }

  $("k-ask-form").onsubmit = async (event) => {
    event.preventDefault();
    const question = $("k-question").value.trim();
    if (!question) return;
    const status = $("k-ask-status");
    const out = $("k-answer");
    $("k-ask").disabled = true;
    status.textContent = "Cuttlefish is thinking… (up to a minute)";
    try {
      const answer = await api("ask", { question });
      const sources = answer.sources
        .map(
          (s) =>
            `<li><b>${escapeHtml(s.id)}</b> ${titleLink(s.title, s.url)}${s.heading ? ` › ${escapeHtml(s.heading)}` : ""} <span class="panel-note">${escapeHtml(sourceName(s.source))} · ${escapeHtml(s.license ?? "license unknown")}</span></li>`,
        )
        .join("");
      out.innerHTML = `<p class="cf-text">${escapeHtml(answer.text)}</p>${sources ? `<ol class="k-sources">${sources}</ol>` : ""}`;
      out.hidden = false;
      status.textContent = "";
    } catch (error) {
      status.textContent = modelError(error);
    } finally {
      $("k-ask").disabled = !k.stats?.anthropic_key;
    }
  };

  $("k-translate-form").onsubmit = async (event) => {
    event.preventDefault();
    const text = $("k-text").value.trim();
    if (!text) return;
    const status = $("k-translate-status");
    $("k-translate").disabled = true;
    status.textContent = "Translating…";
    remember("to", $("k-to").value);
    try {
      const data = await api("translate", { text, to: $("k-to").value });
      const out = $("k-translation");
      out.textContent = data.text;
      out.hidden = false;
      status.textContent = "";
    } catch (error) {
      status.textContent = modelError(error);
    } finally {
      $("k-translate").disabled = !k.stats?.anthropic_key;
    }
  };
  $("k-to").value = remembered("to", "ja");

  // --------------------------------------------------------------- import

  function setKind(kind) {
    k.kind = kind;
    remember("kind", kind);
    for (const button of $("k-kinds").querySelectorAll("[data-kind]")) {
      button.setAttribute("aria-pressed", String(button.dataset.kind === kind));
    }
    for (const el of $("k-import-form").querySelectorAll("[data-for]")) {
      el.hidden = !el.dataset.for.split(" ").includes(kind);
    }
  }

  $("k-kinds").addEventListener("click", (event) => {
    const button = event.target.closest("[data-kind]");
    if (button) setKind(button.dataset.kind);
  });

  const lines = (id) =>
    $(id)
      .value.split("\n")
      .map((l) => l.trim())
      .filter(Boolean);

  /** The import request the form describes */
  function importRequest() {
    const number = (id) => Number($(id).value) || undefined;
    const web = {
      max_pages: number("k-max-pages"),
      delay_s: number("k-delay"),
    };
    const request = {
      web: { kind: "web", urls: lines("k-urls"), ...web },
      sitemap: { kind: "web", sitemap: $("k-sitemap").value.trim(), ...web },
      wiki: {
        kind: "web",
        mediawiki: $("k-api").value.trim(),
        categories: lines("k-categories"),
        ...web,
      },
      youtube: {
        kind: "youtube",
        url: $("k-youtube").value.trim(),
        max: number("k-max-videos"),
      },
      file: {
        kind: "file",
        paths: lines("k-paths"),
        url: $("k-cite").value.trim() || undefined,
      },
      export: {
        kind: "discord-export",
        paths: lines("k-paths"),
        whole: $("k-whole").checked,
      },
      bot: {
        kind: "discord-bot",
        channels: lines("k-channels"),
        threads: $("k-threads").checked,
      },
    }[k.kind];
    request.meta = {
      source: $("k-source").value || undefined,
      license: $("k-license").value.trim() || undefined,
      refresh: $("k-refresh").checked,
    };
    return request;
  }

  $("k-import-form").onsubmit = async (event) => {
    event.preventDefault();
    note("k-import-error", null);
    try {
      await api("ingest", importRequest());
    } catch (error) {
      return note("k-import-error", error.message);
    }
    pollJobs();
  };

  /** Show the imports, and keep asking while one runs */
  async function pollJobs() {
    clearTimeout(k.pollTimer);
    let data;
    try {
      data = await api("jobs");
    } catch {
      return;
    }
    const list = $("k-jobs");
    list.replaceChildren(
      ...data.jobs.map((job) => {
        const li = document.createElement("li");
        li.className = "cf-download meter";
        li.dataset.state = job.state;
        const percent =
          job.state === "done"
            ? 100
            : job.total
              ? (100 * job.done) / job.total
              : 0;
        const count = job.total ? `${job.done}/${job.total}` : "";
        const log = job.lines.slice(-LOG_SHOWN).map(escapeHtml).join("\n");
        li.innerHTML = `
          <div class="cf-download-head"><span class="cf-download-url">${escapeHtml(job.what)}</span><span class="num">${job.state === "running" ? count || "running" : job.state} · ${job.added} added</span></div>
          <div class="meter-track"><div class="meter-fill" style="width:${percent}%"></div></div>
          ${job.error ? `<span class="panel-note level-critical">${escapeHtml(job.error)}</span>` : ""}
          <pre class="k-log">${log}</pre>
          ${job.state === "running" ? `<button type="button" class="mode-toggle" data-cancel>Cancel</button>` : ""}`;
        return li;
      }),
    );
    const running = data.jobs.some((job) => job.state === "running");
    if (running && k.shown && !document.hidden) {
      k.pollTimer = setTimeout(pollJobs, POLL_MS);
    }
    // Finished: what the store holds has changed
    if (k.wasRunning && !running) {
      loadStats();
      loadDocuments();
    }
    k.wasRunning = running;
  }

  $("k-jobs").addEventListener("click", async (event) => {
    if (!event.target.closest("[data-cancel]")) return;
    await api("cancel", {});
    pollJobs();
  });

  // ------------------------------------------------------------ documents

  async function loadDocuments() {
    try {
      k.documents = (await api("documents")).documents;
    } catch (error) {
      $("k-docs-note").textContent = error.message;
      return;
    }
    drawDocuments();
  }

  function drawDocuments() {
    const filter = $("k-filter").value.trim().toLowerCase();
    const shown = k.documents.filter(
      (d) =>
        !filter ||
        `${d.title} ${d.url ?? ""} ${d.source} ${d.license ?? ""}`
          .toLowerCase()
          .includes(filter),
    );
    $("k-docs").replaceChildren(
      ...shown.map((d) => {
        const tr = document.createElement("tr");
        tr.innerHTML = `
          <td>${titleLink(d.title, d.url)}${d.language ? ` <span class="panel-note">${escapeHtml(d.language)}</span>` : ""}</td>
          <td><span class="cf-kind">${escapeHtml(sourceName(d.source))}</span></td>
          <td class="num">${d.chunks}</td>
          <td>${escapeHtml(d.license ?? "–")}</td>
          <td>${escapeHtml(new Date(d.fetched_at).toLocaleDateString())}</td>`;
        return tr;
      }),
    );
    $("k-docs-note").textContent = k.documents.length
      ? `${shown.length} of ${k.documents.length}`
      : "No documents yet: import some.";
  }

  $("k-filter").oninput = drawDocuments;

  // ------------------------------------------------------------- glossary

  $("k-glossary-form").onsubmit = async (event) => {
    event.preventDefault();
    const list = $("k-glossary");
    let data;
    try {
      data = await api(
        `glossary?${new URLSearchParams({ q: $("k-term").value })}`,
      );
    } catch (error) {
      list.innerHTML = `<li class="notice">${escapeHtml(error.message)}</li>`;
      return;
    }
    $("k-glossary-size").textContent = `${data.size} terms`;
    list.replaceChildren();
    if (!data.terms.length) {
      list.innerHTML = `<li class="panel-note">No glossary term found.</li>`;
    }
    for (const term of data.terms) {
      const li = document.createElement("li");
      li.className = "k-term";
      const forms = Object.entries(term.forms)
        .map(
          ([lang, names]) =>
            `<span class="k-form"><b>${escapeHtml(lang)}</b> ${escapeHtml(names.join(", "))}</span>`,
        )
        .join("");
      li.innerHTML = `<div class="k-hit-head"><span class="cf-kind">${escapeHtml(term.id)}</span></div><div class="k-forms">${forms}</div><p class="k-hit-text">${escapeHtml(term.definition)}</p>`;
      list.append(li);
    }
  };

  // -------------------------------------------------------------- routing

  function show() {
    loadStats();
    loadDocuments();
    pollJobs();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const inPlayer = state.has("r") || state.has("kind");
    const view = state.get("view") === "knowledge";
    const wasShown = k.shown;
    k.shown = app === "cuttlefish" && view;
    $("cf-knowledge").hidden = !k.shown;
    $("cf-tabs").hidden = app !== "cuttlefish" || inPlayer;
    for (const tab of $("cf-tabs").querySelectorAll("[data-tab]")) {
      const current = (tab.dataset.tab === "knowledge") === view;
      if (current) tab.setAttribute("aria-current", "page");
      else tab.removeAttribute("aria-current");
    }
    if (k.shown && !wasShown) show();
    if (!k.shown) clearTimeout(k.pollTimer);
  });

  document.addEventListener("visibilitychange", () => {
    if (k.shown && !document.hidden) pollJobs();
  });

  setKind(remembered("kind", "web"));
})();
