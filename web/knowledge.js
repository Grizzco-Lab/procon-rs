// Cuttlefish's knowledge view (/cuttlefish/knowledge): managing what
// Cuttlefish knows. Imports (uploads into the inbox and its import report
// included), what the store holds, documents, the asset browser and search,
// through /api/cuttlefish/knowledge/... (see src/knowledge.rs). Questions
// are the chat's (cuttlefish.js), glossary lookups and translations the
// translator's (translate.js). Runs after cuttlefish.js, which hides its
// library and player for this view and marks the tab, and uses the helpers
// of i18n.js, app.js and inspect.js (t, $, escapeHtml).
"use strict";

(() => {
  /** How often a running import is asked about, in ms */
  const POLL_MS = 1000;
  /** Log lines shown per import */
  const LOG_SHOWN = 8;
  /** Source kinds as shown, as i18n keys */
  const SOURCES = {
    web: "k.source.web",
    wiki: "k.source.wiki",
    guide: "k.source.guide",
    video: "k.source.video",
    "discord-vod-review": "k.source.vodReview",
    discord: "k.source.discord",
    file: "k.source.file",
  };

  /** Document formats as shown: names, or i18n keys (`k.`); others are
   * file extensions, shown in capitals */
  const FORMATS = {
    html: "HTML",
    md: "Markdown",
    markdown: "Markdown",
    txt: "k.format.text",
    docx: "Word",
    "google-doc": "k.format.googleDoc",
    "google-sheet": "k.format.googleSheet",
    "google-slides": "k.format.googleSlides",
    subtitles: "k.format.subtitles",
    messages: "k.format.messages",
    file: "k.source.file",
    other: "k.format.other",
  };

  const k = {
    /** Whether the view is shown */
    shown: false,
    stats: null,
    documents: [],
    /** Import form's kind */
    kind: "web",
    /** What the Wiki / site kind imports: `mediawiki` or `site` */
    topic: "mediawiki",
    pollTimer: null,
    /** The uploads so far, one after another */
    uploads: Promise.resolve(),
    /** The import report shown, if any */
    report: null,
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

  const sourceName = (source) =>
    SOURCES[source] ? t(SOURCES[source]) : source;

  function formatName(format) {
    const name = FORMATS[format];
    if (!name) return format.toUpperCase();
    return name.startsWith("k.") ? t(name) : name;
  }

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
      note("k-stats-error", t("k.stats.cannotOpen", { error: error.message }));
      return;
    }
    k.stats = stats;
    drawStats();
  }

  /** The store's numbers and the keys' state */
  function drawStats() {
    const stats = k.stats;
    if (!stats) return;
    note("k-stats-error", null);
    $("k-loading").hidden = true;
    $("k-tiles").hidden = false;
    $("k-data").textContent = stats.data;
    $("k-documents").textContent = stats.documents;
    $("k-sources-note").textContent =
      stats.sources
        .map((s) => `${sourceName(s.source)} ${s.documents}`)
        .join(" · ") || t("k.stats.nothing");
    $("k-chunks").textContent = stats.chunks;
    $("k-embedder").textContent = stats.embedder;
    $("k-terms").textContent = stats.glossary_terms;
    $("k-glossary-note").textContent = stats.own_glossary
      ? t("k.stats.ownGlossary")
      : t("k.stats.seedGlossary");
    $("k-digest").textContent = stats.digest ? t("k.yes") : t("k.no");
    const keyChip = (name, set, what) =>
      `<span class="chip" data-level="${set ? "good" : "off"}" title="${escapeHtml(what)}"><span class="chip-dot"></span><span class="chip-text">${name} ${set ? t("k.key.set") : t("k.key.notSet")}</span></span>`;
    // The model backend: the API, the Claude Code CLI or none; never a secret
    const backend = t(`k.backend.${stats.backend || "none"}`);
    const model = stats.model ? ` · ${escapeHtml(stats.model)}` : "";
    $("k-keys").innerHTML =
      `<span class="chip" data-level="${stats.backend ? "good" : "off"}" title="${escapeHtml(t("k.backend.note"))}"><span class="chip-dot"></span><span class="chip-text">${escapeHtml(backend)}${model}</span></span>` +
      keyChip("DISCORD_BOT_TOKEN", stats.discord_token, t("k.key.discord"));
    $("k-token-note").textContent = stats.discord_token
      ? ""
      : t("k.key.discordNote");
  }

  // -------------------------------------------------------------- search

  $("k-search-form").onsubmit = async (event) => {
    event.preventDefault();
    const q = $("k-q").value.trim();
    if (!q) return;
    note("k-search-error", null);
    const list = $("k-hits");
    list.innerHTML = `<li class="panel-note">${escapeHtml(t("k.search.running"))}</li>`;
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
      list.innerHTML = `<li class="panel-note">${escapeHtml(t("k.search.none"))}</li>`;
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
        <span class="panel-note">${escapeHtml(hit.license ?? t("k.licenseUnknown"))}${hit.language ? ` · ${escapeHtml(hit.language)}` : ""}</span>`;
      list.append(li);
    }
  };

  // --------------------------------------------------------------- import

  /** The import button's words: the inbox's, a dry run's or an import's */
  function goLabel() {
    if (k.kind === "inbox") return t("k.inbox.import");
    if (k.kind === "wiki" && $("k-dry-run").checked) return t("k.countPages");
    return t("k.import");
  }

  /** Shows the fields of the kind (`data-for`) and, for a wiki or site,
   * of the topic (`data-topic`) */
  function showFields() {
    const form = $("k-import-form");
    for (const el of form.querySelectorAll("[data-for], [data-topic]")) {
      const kind =
        !el.dataset.for || el.dataset.for.split(" ").includes(k.kind);
      const topic = !el.dataset.topic || el.dataset.topic === k.topic;
      el.hidden = !(kind && topic);
    }
    for (const button of $("k-topics").querySelectorAll("[data-topic-pick]")) {
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.topicPick === k.topic),
      );
    }
    $("k-import-go").textContent = goLabel();
  }

  function setKind(kind) {
    k.kind = kind;
    remember("kind", kind);
    for (const button of $("k-kinds").querySelectorAll("[data-kind]")) {
      button.setAttribute("aria-pressed", String(button.dataset.kind === kind));
    }
    showFields();
    if (kind === "inbox" && k.shown) loadInbox();
  }

  $("k-kinds").addEventListener("click", (event) => {
    const button = event.target.closest("[data-kind]");
    if (button) setKind(button.dataset.kind);
  });

  $("k-topics").addEventListener("click", (event) => {
    const button = event.target.closest("[data-topic-pick]");
    if (!button) return;
    k.topic = button.dataset.topicPick;
    remember("topic", k.topic);
    showFields();
  });

  $("k-dry-run").addEventListener("change", () => {
    $("k-import-go").textContent = goLabel();
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
    const topic = {
      max_pages: number("k-topic-max"),
      delay_s: number("k-topic-delay"),
      dry_run: $("k-dry-run").checked,
    };
    const request = {
      inbox: { kind: "inbox" },
      web: {
        kind: "web",
        urls: lines("k-urls"),
        all_tabs: $("k-all-tabs").checked,
        ...web,
      },
      sitemap: { kind: "web", sitemap: $("k-sitemap").value.trim(), ...web },
      wiki:
        k.topic === "site"
          ? {
              kind: "site",
              start: $("k-site-start").value.trim(),
              skip: lines("k-skip"),
              ...topic,
            }
          : {
              kind: "wiki",
              start: lines("k-wiki-start"),
              api: $("k-api").value.trim() || undefined,
              depth: Number($("k-depth").value),
              exclude: lines("k-exclude"),
              link_match: lines("k-link-match"),
              ...topic,
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
        const state =
          job.state === "running"
            ? count || t("k.job.running")
            : t(`k.job.${job.state}`);
        const log = job.lines.slice(-LOG_SHOWN).map(escapeHtml).join("\n");
        // A running job (loading the store and embedding included) has a
        // Stop button above its log, where it is seen without scrolling
        const stop =
          job.state === "running"
            ? job.stopping
              ? `<button type="button" class="btn btn-small k-stop" disabled>${escapeHtml(t("k.stopping"))}</button>
                 <span class="panel-note">${escapeHtml(t("k.stoppingNote"))}</span>`
              : `<button type="button" class="btn btn-small k-stop" data-cancel title="${escapeHtml(t("k.stopTitle"))}"><span class="k-stop-glyph" aria-hidden="true"></span>${escapeHtml(t("k.stop"))}</button>`
            : "";
        li.innerHTML = `
          <div class="cf-download-head"><span class="cf-download-url" title="${escapeHtml(job.what)}">${escapeHtml(job.what)}</span><span class="num">${escapeHtml(state)} · ${escapeHtml(t("k.job.added", { n: job.added }))}</span></div>
          <div class="meter-track"><div class="meter-fill" style="width:${percent}%"></div></div>
          ${stop ? `<div class="k-job-tools">${stop}</div>` : ""}
          ${job.error ? `<span class="panel-note level-critical">${escapeHtml(job.error)}</span>` : ""}
          ${job.summary ? `<span class="panel-note">${escapeHtml(job.summary)}</span>` : ""}
          <pre class="k-log">${log}</pre>
          ${job.report ? `<button type="button" class="mode-toggle" data-report="${escapeHtml(job.report)}">${escapeHtml(t("k.report.show"))}</button>` : ""}`;
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
      loadOverview();
      loadAssets();
      loadInbox();
    }
    k.wasRunning = running;
  }

  $("k-jobs").addEventListener("click", async (event) => {
    const report = event.target.closest("[data-report]");
    if (report) return showReport(report.dataset.report);
    const stop = event.target.closest("[data-cancel]");
    if (!stop) return;
    stop.disabled = true;
    stop.textContent = t("k.stopping");
    try {
      await api("cancel", {});
    } catch (error) {
      note("k-import-error", error.message);
    }
    pollJobs();
  });

  // ------------------------------------------------- #vod-review reviews

  /** Starts the job that builds the #vod-review corpus, reads the HUD of
   * the videos on disk and creates or updates a review for every VOD whose
   * video is on disk (POST /api/cuttlefish/community-reviews: `cuttlefish
   * corpus align` then `corpus reviews`); it shows in the jobs list above
   * the button, with Stop, and its last line holds the counts */
  $("k-corpus-go").onclick = async () => {
    const button = $("k-corpus-go");
    button.disabled = true;
    note("k-corpus-result", null);
    try {
      const response = await fetch("/api/cuttlefish/community-reviews", {
        method: "POST",
      });
      const data = await response.json();
      if (!response.ok) throw new Error(data.error ?? response.statusText);
    } catch (error) {
      note("k-corpus-result", t("k.corpus.failed", { error: error.message }));
    }
    button.disabled = false;
    pollJobs();
  };

  // ---------------------------------------------------------------- inbox

  /** Largest file uploaded (as the server's MAX_UPLOAD) */
  const MAX_UPLOAD = 4 * 2 ** 30;
  /** Folders not uploaded: dependencies and build output (the import
   * skips them too), and hidden ones such as .git */
  const SKIP_DIRS = new Set([
    "node_modules",
    "bower_components",
    "target",
    "build",
    "dist",
    "out",
    "bin",
    "obj",
    "__pycache__",
    "venv",
    "vendor",
    "coverage",
    "Pods",
    "DerivedData",
  ]);

  /** Bytes as KB, MB or GB */
  function size(bytes) {
    const units = ["B", "KB", "MB", "GB"];
    let i = 0;
    while (bytes >= 1024 && i < units.length - 1) {
      bytes /= 1024;
      i++;
    }
    return `${bytes.toFixed(i && bytes < 10 ? 1 : 0)} ${units[i]}`;
  }

  async function loadInbox() {
    let pending;
    try {
      pending = await api("inbox");
    } catch (error) {
      $("k-inbox-status").textContent = error.message;
      return;
    }
    $("k-inbox-folder").textContent = pending.folder;
    $("k-inbox-status").textContent = pending.files
      ? t("k.inbox.status", {
          files: pending.files,
          size: size(pending.bytes),
          state: pending.new
            ? t("k.inbox.new", { n: pending.new })
            : t("k.inbox.allImported"),
        })
      : t("k.inbox.empty");
  }

  /** Every file of a dropped folder, with its path */
  async function entryFiles(entry, path, out) {
    if (entry.isFile) {
      const file = await new Promise((resolve, reject) =>
        entry.file(resolve, reject),
      );
      out.push({ file, path: `${path}${file.name}` });
    } else if (entry.isDirectory) {
      const reader = entry.createReader();
      // readEntries answers in batches until it answers none
      for (;;) {
        const batch = await new Promise((resolve, reject) =>
          reader.readEntries(resolve, reject),
        );
        if (!batch.length) break;
        for (const child of batch) {
          await entryFiles(child, `${path}${entry.name}/`, out);
        }
      }
    }
  }

  /** Uploads files ({file, path}) one after another into the inbox */
  async function upload(files) {
    const into = $("k-into")
      .value.trim()
      .replace(/^\/+|\/+$/g, "");
    const skipped = [];
    const queue = files.filter(({ file, path }) => {
      const parts = path.split("/");
      const hidden = parts.some((p) => p.startsWith("."));
      const dependency = parts.slice(0, -1).some((p) => SKIP_DIRS.has(p));
      if (hidden || dependency || file.size > MAX_UPLOAD) {
        skipped.push(path);
        return false;
      }
      return true;
    });
    const total = queue.reduce((sum, { file }) => sum + file.size, 0);
    const box = $("k-upload");
    const log = $("k-upload-log");
    box.hidden = false;
    log.textContent = skipped.length
      ? `${t("k.upload.left", { n: skipped.length })}\n`
      : "";
    let sent = 0;
    let failed = 0;
    const show = (done, current) => {
      $("k-upload-what").textContent = current ?? t("k.upload.done");
      $("k-upload-what").title = current ?? "";
      $("k-upload-count").textContent = t("k.upload.count", {
        done,
        total: queue.length,
        sent: size(sent),
        size: size(total),
      });
      $("k-upload-fill").style.width = `${total ? (100 * sent) / total : 100}%`;
    };
    for (const [i, { file, path }] of queue.entries()) {
      const target = into ? `${into}/${path}` : path;
      show(i, target);
      const before = sent;
      try {
        await new Promise((resolve, reject) => {
          const xhr = new XMLHttpRequest();
          xhr.open(
            "POST",
            `/api/cuttlefish/knowledge/upload?${new URLSearchParams({ path: target })}`,
          );
          xhr.upload.onprogress = (event) => {
            sent = before + event.loaded;
            show(i, target);
          };
          xhr.onload = () =>
            xhr.status === 200
              ? resolve()
              : reject(
                  new Error(
                    (() => {
                      try {
                        return JSON.parse(xhr.responseText).error;
                      } catch {
                        return xhr.statusText;
                      }
                    })(),
                  ),
                );
          xhr.onerror = () => reject(new Error(t("k.upload.lost")));
          xhr.send(file);
        });
      } catch (error) {
        failed++;
        log.textContent += `${t("k.upload.failed", { path: target, error: error.message })}\n`;
      }
      sent = before + file.size;
    }
    show(queue.length);
    log.textContent += `${t(failed ? "k.upload.someFailed" : "k.upload.summary", { n: queue.length - failed, failed })}\n`;
    loadInbox();
  }

  /** Uploads after the ones already started */
  function queueUpload(files) {
    k.uploads = k.uploads.then(() => upload(files));
  }

  $("k-pick-files").onclick = () => $("k-files").click();
  $("k-pick-folder").onclick = () => $("k-folder").click();
  for (const id of ["k-files", "k-folder"]) {
    $(id).onchange = (event) => {
      const files = [...event.target.files].map((file) => ({
        file,
        path: file.webkitRelativePath || file.name,
      }));
      event.target.value = "";
      if (files.length) queueUpload(files);
    };
  }

  const drop = $("k-drop");
  drop.addEventListener("dragover", (event) => {
    event.preventDefault();
    drop.classList.add("is-over");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("is-over"));
  drop.addEventListener("drop", async (event) => {
    event.preventDefault();
    drop.classList.remove("is-over");
    // Entries must be taken before the first await
    const entries = [...event.dataTransfer.items]
      .map((item) => item.webkitGetAsEntry?.())
      .filter(Boolean);
    const files = [];
    if (entries.length) {
      for (const entry of entries) await entryFiles(entry, "", files);
    } else {
      for (const file of event.dataTransfer.files) {
        files.push({ file, path: file.name });
      }
    }
    if (files.length) queueUpload(files);
  });

  // -------------------------------------------------------------- reports

  /** A report of an inbox import, as HTML */
  function reportHtml(report) {
    const byKind = {};
    for (const t of report.taken) (byKind[t.kind] ??= []).push(t);
    const again = escapeHtml(t("k.report.reimport"));
    const list = (items) =>
      `<ul>${items.map((t) => `<li><span class="path">${escapeHtml(t.path)}</span> <span class="panel-note">${escapeHtml(t.detail)}</span> <button type="button" class="mode-toggle" data-reimport="${escapeHtml(t.path)}">${again}</button></li>`).join("")}</ul>`;
    const taken = Object.entries(byKind)
      .map(
        ([kind, items]) =>
          `<details${items.length <= 12 ? " open" : ""}><summary><b>${escapeHtml(t(`k.report.kind.${kind}`))}</b> ${items.length}</summary>${list(items)}</details>`,
      )
      .join("");
    const skipped = report.skipped
      .map(
        (s) =>
          `<details><summary>${escapeHtml(s.reason)} <span class="num">${s.count}</span></summary><ul>${s.examples.map((e) => `<li class="path">${escapeHtml(e)}</li>`).join("")}${s.count > s.examples.length ? `<li class="panel-note">${escapeHtml(t("k.report.more", { n: s.count - s.examples.length }))}</li>` : ""}</ul></details>`,
      )
      .join("");
    const failed = report.failed.length
      ? `<details open><summary class="level-critical"><b>${escapeHtml(t("k.report.failed"))}</b> ${report.failed.length}</summary>${list(report.failed)}</details>`
      : "";
    const gone = report.gone.length
      ? `<details><summary>${escapeHtml(t("k.report.gone"))} ${report.gone.length}</summary><ul>${report.gone.map((g) => `<li class="path">${escapeHtml(g)}</li>`).join("")}</ul></details>`
      : "";
    const notes = report.notes.length
      ? `<ul class="k-notes">${report.notes.map((n) => `<li>${escapeHtml(n)}</li>`).join("")}</ul>`
      : "";
    return `
      <div class="k-report-head"><b>${escapeHtml(t("k.report.title", { time: new Date(report.started).toLocaleString(i18nLocale()) }))}</b>
        <span class="panel-note">${escapeHtml(t(report.cancelled ? "k.report.statsCancelled" : "k.report.stats", { files: report.files, unchanged: report.unchanged }))}</span>
        <button type="button" class="mode-toggle" data-close-report>${escapeHtml(t("k.report.close"))}</button></div>
      ${taken || `<p class="panel-note">${escapeHtml(t("k.report.nothing"))}</p>`}
      ${failed}
      ${skipped ? `<h3 class="readout-label">${escapeHtml(t("k.report.skipped"))}</h3>${skipped}` : ""}
      ${gone}${notes}`;
  }

  /** Shows the report last opened (again, in another language) */
  function drawReport() {
    $("k-report").innerHTML = reportHtml(k.report);
  }

  async function showReport(id) {
    const box = $("k-report");
    try {
      k.report = await api(`report?${new URLSearchParams({ id })}`);
      drawReport();
    } catch (error) {
      k.report = null;
      box.innerHTML = `<p class="notice">${escapeHtml(error.message)}</p>`;
    }
    box.hidden = false;
    box.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }

  $("k-report").addEventListener("click", async (event) => {
    const again = event.target.closest("[data-reimport]");
    if (again) {
      // Forgets what the file gave and reads it again, as an inbox import
      again.disabled = true;
      try {
        await api("ingest", {
          kind: "inbox",
          reimport: again.dataset.reimport,
        });
      } catch (error) {
        again.disabled = false;
        return note("k-import-error", error.message);
      }
      return pollJobs();
    }
    if (!event.target.closest("[data-close-report]")) return;
    k.report = null;
    $("k-report").hidden = true;
  });

  // ------------------------------------------------------------- overview

  /** `name count` chips from an object of counts, largest first, after a
   * label when given */
  function counts(object, name = (key) => key, label = "") {
    const entries = Object.entries(object).sort((a, b) => b[1] - a[1]);
    const head = label
      ? `<span class="k-counts-label">${escapeHtml(label)}</span>`
      : "";
    return (
      head +
      (entries.length
        ? entries
            .map(
              ([key, n]) =>
                `<span class="k-count" title="${escapeHtml(name(key))}">${escapeHtml(name(key))} <b class="num">${n}</b></span>`,
            )
            .join("")
        : `<span class="panel-note">${escapeHtml(t("k.none"))}</span>`)
    );
  }

  async function loadOverview() {
    let o;
    try {
      o = await api("overview");
    } catch (error) {
      $("k-overview").innerHTML =
        `<p class="notice">${escapeHtml(error.message)}</p>`;
      return;
    }
    const tables = o.glossary.tables
      .map(
        (table) =>
          `<li><span class="path">${escapeHtml(table.source)}</span> ${escapeHtml(t("k.ov.tableTerms", { n: table.terms }))} · ${escapeHtml(table.languages.join(", "))}<br /><span class="panel-note">${escapeHtml(table.note)}</span></li>`,
      )
      .join("");
    const folders = Object.fromEntries(
      Object.entries(o.assets.folders).map(([f, n]) => [
        f || t("k.ov.inboxFolder"),
        n,
      ]),
    );
    const reports = o.reports
      .map(
        (r) =>
          `<li><button type="button" class="mode-toggle" data-report="${escapeHtml(r.id)}">${escapeHtml(new Date(r.started).toLocaleString(i18nLocale()))}</button> <span class="panel-note">${escapeHtml(r.summary)}</span></li>`,
      )
      .join("");
    const aside = o.moved_aside
      .map(
        (path) =>
          `<p class="notice is-info">${t("k.ov.movedAside", { path: `<span class="path">${escapeHtml(path)}</span>` })}</p>`,
      )
      .join("");
    $("k-overview").innerHTML = `${aside}
      <div class="k-ov-block"><h3 class="readout-label">${escapeHtml(t("k.documents"))} <b class="num">${o.documents.total}</b></h3>
        <div class="k-counts">${counts(o.documents.sources, sourceName, t("k.ov.bySource"))}</div>
        <div class="k-counts">${counts(o.documents.formats, formatName, t("k.ov.byFormat"))}</div></div>
      <div class="k-ov-block"><h3 class="readout-label">${escapeHtml(t("k.glossary"))} <b class="num">${o.glossary.terms}</b> · ${escapeHtml(t("k.ov.imported", { n: o.glossary.imported }))}</h3>
        <div class="k-counts">${counts(o.glossary.languages)}</div>
        ${tables ? `<ul class="k-tables">${tables}</ul>` : `<p class="panel-note">${escapeHtml(t("k.ov.noTables"))}</p>`}</div>
      <div class="k-ov-block"><h3 class="readout-label">${escapeHtml(t("k.assets"))} <b class="num">${o.assets.total}</b> · ${escapeHtml(t("k.ov.linked", { n: o.assets.linked }))}</h3>
        <div class="k-counts">${counts(folders)}</div></div>
      <div class="k-ov-block"><h3 class="readout-label">${escapeHtml(t("k.kind.inbox"))}</h3>
        <p class="panel-note">${escapeHtml(t("k.ov.inboxNote", { files: o.inbox.files, size: size(o.inbox.bytes), n: o.inbox.new }))} · <span class="path">${escapeHtml(o.inbox.folder)}</span></p></div>
      <div class="k-ov-block"><h3 class="readout-label">${escapeHtml(t("k.ov.reports"))}</h3>
        ${reports ? `<ul class="k-reports">${reports}</ul>` : `<p class="panel-note">${escapeHtml(t("k.ov.noReports"))}</p>`}</div>`;
  }

  $("k-overview").addEventListener("click", (event) => {
    const report = event.target.closest("[data-report]");
    if (report) showReport(report.dataset.report);
  });

  // --------------------------------------------------------------- assets

  async function loadAssets() {
    const q = $("k-asset-q").value.trim();
    const folder = $("k-asset-folder").value;
    let data;
    try {
      data = await api(`assets?${new URLSearchParams({ q, folder })}`);
    } catch (error) {
      $("k-assets-note").textContent = error.message;
      return;
    }
    const select = $("k-asset-folder");
    const options = Object.entries(data.folders)
      .map(
        ([f, n]) =>
          `<option value="${escapeHtml(f)}">${escapeHtml(f || t("k.ov.inboxFolder"))} · ${n}</option>`,
      )
      .join("");
    select.innerHTML = `<option value="">${escapeHtml(t("k.assets.allFolders"))}</option>${options}`;
    select.value = folder;
    $("k-assets-note").textContent = data.total
      ? t(
          data.matching > data.assets.length
            ? "k.assets.firstShown"
            : "k.docs.shown",
          { n: data.matching, total: data.total, shown: data.assets.length },
        )
      : "";
    const list = $("k-assets");
    if (!data.total) {
      list.innerHTML = `<li class="panel-note">${escapeHtml(t("k.assets.none"))}</li>`;
      return;
    }
    list.replaceChildren(
      ...data.assets.map((a) => {
        const li = document.createElement("li");
        li.className = "k-asset";
        li.title = a.path;
        const names = a.names
          ? Object.entries(a.names)
              .filter(([lang]) => ["en", "ja", "zh"].includes(lang))
              .map(
                ([lang, n]) =>
                  `<span class="k-form"><b>${escapeHtml(lang)}</b> ${escapeHtml(n[0])}</span>`,
              )
              .join("")
          : "";
        const dims = a.width ? `${a.width}×${a.height} · ` : "";
        li.innerHTML = `
          <div class="k-asset-img"><img loading="lazy" alt="" src="/api/cuttlefish/knowledge/thumb?${new URLSearchParams({ id: a.id, v: a.bytes })}" /></div>
          <span class="k-asset-name">${escapeHtml(a.name)}</span>
          ${a.term ? `<span class="cf-kind">${escapeHtml(a.term)}</span><div class="k-forms">${names}</div>` : ""}
          <span class="panel-note">${dims}${escapeHtml(a.format)} · ${size(a.bytes)}</span>`;
        return li;
      }),
    );
  }

  let assetTimer = null;
  $("k-asset-q").oninput = () => {
    clearTimeout(assetTimer);
    assetTimer = setTimeout(loadAssets, 250);
  };
  $("k-asset-folder").onchange = loadAssets;

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
        `${d.title} ${d.url ?? ""} ${d.path ?? ""} ${d.source} ${d.license ?? ""}`
          .toLowerCase()
          .includes(filter),
    );
    $("k-docs").replaceChildren(
      ...shown.map((d) => {
        const tr = document.createElement("tr");
        tr.innerHTML = `
          <td>${titleLink(d.title, d.url)}${d.language ? ` <span class="panel-note">${escapeHtml(d.language)}</span>` : ""}${d.path ? `<br /><span class="panel-note path">${escapeHtml(d.path)}</span>` : ""}</td>
          <td><span class="cf-kind">${escapeHtml(sourceName(d.source))}</span></td>
          <td class="num">${d.chunks}</td>
          <td>${escapeHtml(d.license ?? "–")}</td>
          <td>${escapeHtml(new Date(d.fetched_at).toLocaleDateString(i18nLocale()))}</td>
          <td><button type="button" class="mode-toggle" data-delete="${escapeHtml(d.id)}">${escapeHtml(t("k.delete"))}</button></td>`;
        return tr;
      }),
    );
    $("k-docs-note").textContent = k.documents.length
      ? t("k.docs.shown", { n: shown.length, total: k.documents.length })
      : t("k.docs.none");
  }

  $("k-filter").oninput = drawDocuments;

  $("k-docs").addEventListener("click", async (event) => {
    const button = event.target.closest("[data-delete]");
    if (!button) return;
    const doc = k.documents.find((d) => d.id === button.dataset.delete);
    const question = t(doc?.path ? "k.delete.askInbox" : "k.delete.ask", {
      title: doc?.title,
      n: doc?.chunks,
    });
    if (!doc || !confirm(question)) return;
    button.disabled = true;
    try {
      await api("delete", { ids: [doc.id] });
    } catch (error) {
      button.disabled = false;
      $("k-docs-note").textContent = error.message;
      return;
    }
    loadDocuments();
    loadStats();
    loadOverview();
  });

  // -------------------------------------------------------------- routing

  function show() {
    loadStats();
    loadDocuments();
    loadOverview();
    loadAssets();
    loadInbox();
    pollJobs();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const wasShown = k.shown;
    k.shown = app === "cuttlefish" && state.get("view") === "knowledge";
    $("cf-knowledge").hidden = !k.shown;
    if (k.shown && !wasShown) show();
    if (!k.shown) clearTimeout(k.pollTimer);
  });

  document.addEventListener("visibilitychange", () => {
    if (k.shown && !document.hidden) pollJobs();
  });

  // What is drawn from JavaScript follows the language
  window.addEventListener("lang-change", () => {
    drawStats();
    if (k.documents.length) drawDocuments();
    $("k-import-go").textContent = goLabel();
    if (k.shown) {
      pollJobs();
      loadOverview();
      loadAssets();
      loadInbox();
      if (k.report) drawReport();
    }
  });

  k.topic = remembered("topic", "mediawiki") === "site" ? "site" : "mediawiki";
  setKind(remembered("kind", "inbox"));
})();
