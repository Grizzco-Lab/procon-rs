// Cuttlefish's Translate view (/cuttlefish/translate): jargon and
// callouts across languages, in the names each community uses. A chat-like
// page: the history above, the box at the bottom of the window with the
// mentor's avatar, a target-language picker and Send. A bare term shows its
// glossary entry at once (GET knowledge/glossary, no key needed), then the
// model's explanation; a sentence gets the terms it uses and the model's
// translation (POST translate, see src/cuttlefish.rs). The history is
// kept by the server in <reviews>/translations.jsonl. A term shows its
// slang too, with "Add alias"; a sentence has "Teach a word" (select it,
// pick its term as you type); the Slang panel lists what was taught and
// the model's suggestions to review (see the slang section). Runs after
// cuttlefish.js, which hides its library and player for this view, and uses
// the helpers of i18n.js and app.js (t, $, escapeHtml).
"use strict";

(() => {
  /** Target languages offered, each named in itself */
  const LANGUAGES = [
    ["en", "English"],
    ["zh", "中文"],
    ["ja", "日本語"],
    ["es", "Español"],
    ["fr", "Français"],
    ["ru", "Русский"],
    ["ko", "한국어"],
  ];
  /** The examples: the player's own jargon into English (data, like the
   * glossary, shown in every language) and two callouts into Chinese */
  const EXAMPLES = [
    { text: "惯性取消搬蛋快", target: "en" },
    { text: "我刚拿的熊刷，不应该上柱子拍的", target: "en" },
    {
      text: "小枪可以优先出差回收一些外围蛋，但不要待太久卡新一波怪",
      target: "en",
    },
    { text: "我还剩一个镭射", target: "en" },
    {
      text: "Someone take the Stinger on the shore, I'll run eggs",
      target: "zh",
    },
    { text: "Save your wail for the Flyfish", target: "zh" },
  ];
  /** Lines the input grows to before it scrolls */
  const INPUT_ROWS = 6;
  /** "Copied" shows this long, in ms */
  const COPIED_MS = 1500;

  const tr = {
    /** Whether the view is shown */
    shown: false,
    /** The history, oldest first; a pending entry has `pending: true` */
    entries: [],
    /** The history file, as the server names it */
    file: "",
    /** Whether ANTHROPIC_API_KEY is set where the studio runs; null until
     * asked */
    key: null,
    sending: false,
  };

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-cuttlefish-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-cuttlefish-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  /** GET, POST (with a JSON body) or DELETE under /api/cuttlefish/; throws
   * with the server's message */
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

  const langName = (code) => LANGUAGES.find(([c]) => c === code)?.[1] ?? code;

  // ------------------------------------------------------------ the box

  const input = $("cf-tr-text");
  const target = $("cf-tr-target");

  target.replaceChildren(
    ...LANGUAGES.map(([code, name]) => {
      const option = new Option(name, code);
      option.lang = code;
      return option;
    }),
  );

  function setTarget(code) {
    target.value = code;
    remember("target", code);
  }
  setTarget(remembered("target", "en"));
  target.onchange = () => setTarget(target.value);

  /** The input grows with its text, up to INPUT_ROWS lines */
  function grow() {
    input.style.height = "auto";
    const line = parseFloat(getComputedStyle(input).lineHeight) || 20;
    input.style.height = `${Math.min(input.scrollHeight, line * INPUT_ROWS + 14)}px`;
  }

  input.addEventListener("input", grow);
  // Enter sends, Shift+Enter breaks the line
  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      $("cf-tr-form").requestSubmit();
    }
  });

  /** The examples as chips: one fills the box and picks its language */
  function drawChips() {
    $("cf-tr-chips").replaceChildren(
      ...EXAMPLES.map((example) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "cf-chip";
        button.textContent = example.text;
        button.title = t("tr.exampleInto", { lang: langName(example.target) });
        button.onclick = () => {
          input.value = example.text;
          setTarget(example.target);
          grow();
          input.focus();
        };
        return button;
      }),
    );
  }

  function composerNote(text, kind) {
    const note = $("cf-tr-composer-note");
    note.textContent = text;
    note.classList.toggle("is-error", kind === "error");
    note.classList.toggle("is-warning", kind === "warning");
  }

  /** Whether the studio has a model backend (the API key, or the Claude
   * Code CLI); asked once. Without one the box says the glossary answers
   * alone. */
  async function checkKey() {
    if (tr.key == null) {
      try {
        tr.key = Boolean((await api("knowledge/model")).backend);
      } catch {
        return;
      }
    }
    markKey();
  }

  function markKey() {
    if (tr.key === false && !tr.sending) composerNote(t("tr.noKey"), "warning");
  }

  // ------------------------------------------------------------ history

  async function load() {
    let data;
    try {
      data = await api("translations");
    } catch (error) {
      $("cf-tr-note").textContent = error.message;
      return;
    }
    tr.file = data.file;
    // Newest first from the server; oldest first on the page, like a chat
    tr.entries = data.entries.reverse();
    draw();
  }

  /** Every name of a term: its official names and approved aliases */
  const termNames = (term) => [
    ...Object.values(term.forms).flat(),
    ...(term.aliases ?? [])
      .filter((a) => !a.status || a.status === "approved")
      .map((a) => a.text),
  ];

  /** The name of `term` the text mentions (an official name or slang),
   * for "slang → official name"; the English name or the id when none is
   * found (an imported term) */
  function sourceForm(term, text) {
    const hay = text.toLowerCase();
    let best = null;
    for (const form of termNames(term)) {
      if (
        hay.includes(form.toLowerCase()) &&
        (!best || form.length > best.length)
      )
        best = form;
    }
    return best ?? term.forms.en?.[0] ?? term.id;
  }

  /** A term's approved aliases, per language, each with its note as the
   * tooltip */
  function aliasesHtml(term) {
    const byLang = {};
    for (const alias of term.aliases ?? []) {
      if (alias.status && alias.status !== "approved") continue;
      (byLang[alias.lang] ??= []).push(alias);
    }
    return Object.entries(byLang)
      .map(
        ([code, aliases]) =>
          `<span class="k-form cf-alias-names"><b>${escapeHtml(t("slang.label", { lang: code }))}</b> ${aliases
            .map(
              (a) =>
                `<span title="${escapeHtml([a.note, t(`slang.source.${a.source ?? "seed"}`)].filter(Boolean).join(" · "))}">${escapeHtml(a.text)}</span>`,
            )
            .join(", ")}</span>`,
      )
      .join("");
  }

  /** One glossary term as it was used: the form in the text → its name in
   * the target language, the definition, every other name and the slang,
   * and "Add alias" */
  function termHtml(term, entry) {
    const lang = entry.target.split("-")[0];
    const to = term.forms[lang]?.[0];
    const forms = Object.entries(term.forms)
      .map(
        ([code, names]) =>
          `<span class="k-form"><b>${escapeHtml(code)}</b> ${escapeHtml(names.join(", "))}</span>`,
      )
      .join("");
    return `<li class="k-term" data-term="${escapeHtml(term.id)}">
      <div class="k-hit-head">
        <b class="cf-tr-from">${escapeHtml(sourceForm(term, entry.text))}</b>
        <span class="cf-tr-arrow">→</span>
        ${to ? `<b class="cf-tr-to">${escapeHtml(to)}</b>` : `<span class="panel-note">${escapeHtml(t("tr.noName", { lang: langName(entry.target) }))}</span>`}
        <span class="cf-kind">${escapeHtml(term.id)}</span>
        <button type="button" class="mode-toggle cf-mini" data-add-alias>${escapeHtml(t("alias.add"))}</button>
      </div>
      ${term.definition ? `<p class="k-hit-text">${escapeHtml(term.definition)}</p>` : ""}
      <div class="k-forms">${forms}${aliasesHtml(term)}</div>
    </li>`;
  }

  /** The translation, or why there is none yet */
  function resultHtml(entry) {
    if (entry.pending) {
      return `<p class="panel-note">${escapeHtml(t("tr.working"))}</p>`;
    }
    if (entry.error) {
      return `<p class="panel-note level-critical">${escapeHtml(t("tr.failed", { error: entry.error }))}</p>`;
    }
    const text = entry.translation
      ? `<p class="cf-tr-text" lang="${escapeHtml(entry.target)}">${escapeHtml(entry.translation)}</p>
         <button type="button" class="mode-toggle cf-tr-copy" data-copy>${escapeHtml(t("tr.copy"))}</button>`
      : "";
    // A term named by the glossary needs no note; the box carries one
    const note =
      entry.needs_key && !entry.translation
        ? `<p class="panel-note cf-tr-key">${escapeHtml(t("tr.noKeyTranslation"))}</p>`
        : "";
    return text + note;
  }

  function entryElement(entry) {
    const li = document.createElement("li");
    li.className = "cf-tr";
    li.dataset.id = entry.id;
    if (entry.term) li.classList.add("is-term");
    const when = new Date(entry.created_ms).toLocaleString(i18nLocale(), {
      dateStyle: "short",
      timeStyle: "short",
    });
    const terms = entry.terms?.length
      ? `<details class="cf-tr-terms-box" ${entry.term ? "open" : ""}>
          <summary>${escapeHtml(entry.term ? t("tr.entry") : t("tr.terms", { n: entry.terms.length }))}</summary>
          <ul class="k-terms cf-tr-terms">${entry.terms.map((term) => termHtml(term, entry)).join("")}</ul>
        </details>`
      : entry.pending
        ? ""
        : `<p class="panel-note">${escapeHtml(t("tr.noTerms"))}</p>`;
    li.innerHTML = `
      <div class="cf-tr-head">
        <span class="cf-tr-source">${escapeHtml(entry.text)}</span>
        <span class="cf-kind">→ ${escapeHtml(langName(entry.target))}</span>
        <span class="cf-meta panel-note num">${escapeHtml(when)}</span>
      </div>
      <div class="cf-tr-result">${resultHtml(entry)}</div>
      ${entry.explanation ? `<p class="cf-tr-explain"><b>${escapeHtml(t("cf.name"))}</b> ${escapeHtml(entry.explanation)}</p>` : ""}
      ${terms}
      ${entry.term || entry.pending ? "" : `<div class="cf-tr-teach"><button type="button" class="mode-toggle cf-mini" data-teach>${escapeHtml(t("tr.teach"))}</button></div>`}`;
    return li;
  }

  /** The panel's head: how many are kept and where (the file by name, its
   * whole path as the tooltip), Clear while there is something to clear */
  function drawHead() {
    $("cf-tr-empty").hidden = tr.entries.length > 0;
    $("cf-tr-clear").hidden = !tr.entries.some((e) => !e.pending);
    const note = $("cf-tr-note");
    note.textContent = tr.entries.length
      ? t("tr.count", { n: tr.entries.length, file: tr.file.split("/").pop() })
      : "";
    note.title = tr.file;
  }

  function draw() {
    drawHead();
    $("cf-translations").replaceChildren(...tr.entries.map(entryElement));
  }

  /** Draw `entry` in the place of the entry `id` (its own by default) */
  function redraw(entry, id = entry.id) {
    const old = $("cf-translations").querySelector(`[data-id="${id}"]`);
    if (!old) return draw();
    old.replaceWith(entryElement(entry));
    drawHead();
  }

  // --------------------------------------------------------------- send

  /** Translate `text` into `target`: the glossary's part shows at once,
   * the model's when it comes; the server keeps the entry */
  async function send(text, targetCode) {
    text = text.trim();
    if (!text || tr.sending) return;
    tr.sending = true;
    $("cf-tr-send").disabled = true;
    composerNote(t("tr.working"));
    input.value = "";
    grow();
    const pending = {
      id: `p${Date.now().toString(36)}`,
      text,
      target: targetCode,
      pending: true,
      terms: [],
      created_ms: Date.now(),
    };
    tr.entries.push(pending);
    draw();
    $("cf-translations").lastElementChild?.scrollIntoView({ block: "nearest" });
    // The glossary answers without waiting for the model
    api(`knowledge/glossary?${new URLSearchParams({ q: text })}`)
      .then((data) => {
        if (!pending.pending) return;
        pending.terms = data.terms;
        pending.term =
          data.terms.length === 1 && isTheTerm(data.terms[0], text);
        redraw(pending);
      })
      .catch(() => {
        // The translation brings the terms too
      });
    try {
      const entry = await api("translate", "POST", {
        text,
        target: targetCode,
      });
      tr.entries.splice(tr.entries.indexOf(pending), 1, entry);
      redraw(entry, pending.id);
      $("cf-translations")
        .querySelector(`[data-id="${entry.id}"]`)
        ?.scrollIntoView({ block: "nearest" });
      composerNote("");
    } catch (error) {
      pending.pending = false;
      pending.error = error.message;
      redraw(pending);
      composerNote("");
    } finally {
      tr.sending = false;
      $("cf-tr-send").disabled = false;
      markKey();
      input.focus();
    }
  }

  /** Whether `text` is one of the term's names (a lookup, not a mention) */
  const isTheTerm = (term, text) => {
    const wanted = text.trim().toLowerCase();
    return (
      term.id === wanted ||
      termNames(term).some((form) => form.toLowerCase() === wanted)
    );
  };

  $("cf-tr-form").onsubmit = (event) => {
    event.preventDefault();
    send(input.value, target.value);
  };

  /** Copy a translation; the button says so for a moment */
  async function copy(button, text) {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const area = document.createElement("textarea");
      area.value = text;
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    button.textContent = t("tr.copied");
    setTimeout(() => {
      button.textContent = t("tr.copy");
    }, COPIED_MS);
  }

  $("cf-translations").addEventListener("click", (event) => {
    const button = event.target.closest("[data-copy]");
    if (!button) return;
    const text = button
      .closest(".cf-tr-result")
      .querySelector(".cf-tr-text").textContent;
    copy(button, text);
  });

  $("cf-tr-clear").onclick = async () => {
    if (!confirm(t("tr.clearAsk", { file: tr.file }))) return;
    try {
      // A body, as the server wants a content length
      await api("translations", "DELETE", {});
    } catch (error) {
      $("cf-tr-note").textContent = error.message;
      return;
    }
    load();
  };

  // ---------------------------------------------------------------- slang
  //
  // The user teaches aliases (slang) of glossary terms: "Add alias" on a
  // term, or "Teach a word" on a sentence (select the word, pick its term
  // as you type). The Slang panel lists them, the suggestions the model
  // found in the knowledge base (Approve / Reject / Edit) and runs a
  // suggestion job after a dry run. All in <knowledge>/glossary-user.toml
  // through the knowledge/slang endpoints (src/knowledge.rs).

  /** Poll interval of a suggestion run, in ms */
  const RUN_POLL_MS = 1500;
  /** Wait after a keystroke before searching terms, in ms */
  const SEARCH_MS = 150;

  const slang = {
    /** The user glossary as the server lists it */
    aliases: [],
    file: "",
    pending: 0,
    /** Whether the panel is open */
    open: remembered("slang-open", "0") === "1",
    /** The suggestion run's job id while it runs */
    job: null,
    /** The last text selected in a translation's source: `{id, text}` */
    selected: { id: null, text: "" },
  };

  /** A term's name for the page: in its language, else English, else the
   * id */
  const termLabel = (forms, id) =>
    forms?.[i18nLang()]?.[0] ?? forms?.en?.[0] ?? id;

  /** The language a word is probably in, from its script */
  function guessLang(text) {
    if (/[぀-ヿ]/.test(text)) return "ja";
    if (/[가-힯]/.test(text)) return "ko";
    if (/[㐀-鿿]/.test(text)) return "zh";
    if (/[Ѐ-ӿ]/.test(text)) return "ru";
    return "en";
  }

  const number = (n) => Number(n ?? 0).toLocaleString(i18nLocale());

  /** The form teaching or editing an alias. `term` is the term's id, with
   * `termName` shown in the search box when the term can be picked
   * (`pick`); `mode` is "add" or "edit" (of `id`); `approve` makes Save
   * approve a suggestion; `entry` is the translation to refresh after */
  function aliasForm({
    mode,
    id = "",
    term = "",
    termName = "",
    pick = false,
    text = "",
    lang = "en",
    note = "",
    approve = false,
    entry = "",
  }) {
    const form = document.createElement("form");
    form.className = "cf-alias-form";
    Object.assign(form.dataset, {
      mode,
      id,
      approve: approve ? "1" : "",
      entry,
    });
    const languages = LANGUAGES.map(
      ([code, name]) =>
        `<option value="${code}" ${code === lang ? "selected" : ""}>${escapeHtml(name)}</option>`,
    ).join("");
    form.innerHTML = `
      ${
        pick
          ? `<label class="cf-alias-field cf-term-pick"><span>${escapeHtml(t("alias.term"))}</span>
              <input class="select" data-term-search autocomplete="off" value="${escapeHtml(termName)}" placeholder="${escapeHtml(t("alias.termSearch"))}" />
              <ul class="cf-term-results" data-term-results hidden></ul></label>`
          : ""
      }
      <input type="hidden" data-term value="${escapeHtml(term)}" />
      <label class="cf-alias-field"><span>${escapeHtml(t("alias.text"))}</span>
        <input class="select" data-text required maxlength="40" value="${escapeHtml(text)}" /></label>
      <label class="cf-alias-field"><span>${escapeHtml(t("alias.lang"))}</span>
        <select class="select" data-lang>${languages}</select></label>
      <label class="cf-alias-field cf-alias-note"><span>${escapeHtml(t("alias.note"))}</span>
        <input class="select" data-note maxlength="300" value="${escapeHtml(note)}" /></label>
      <div class="cf-alias-actions">
        <button type="submit" class="btn">${escapeHtml(t(approve ? "alias.saveApprove" : "alias.save"))}</button>
        <button type="button" class="mode-toggle" data-form-cancel>${escapeHtml(t("alias.cancel"))}</button>
        <span class="panel-note" data-form-note></span>
      </div>`;
    return form;
  }

  /** Terms named like the search box's text, as buttons to pick */
  async function searchTerms(input) {
    const form = input.closest("form");
    const list = form.querySelector("[data-term-results]");
    const query = input.value.trim();
    form.querySelector("[data-term]").value = "";
    if (!query) {
      list.hidden = true;
      return;
    }
    let data;
    try {
      data = await api(`knowledge/terms?${new URLSearchParams({ q: query })}`);
    } catch {
      return;
    }
    // A newer keystroke took over
    if (input.value.trim() !== query) return;
    list.innerHTML = data.terms.length
      ? data.terms
          .map((term) => {
            const names = ["en", "zh", "ja"]
              .map((code) => term.forms[code]?.[0])
              .filter(Boolean);
            const label = termLabel(term.forms, term.id);
            return `<li><button type="button" data-pick-term="${escapeHtml(term.id)}" data-label="${escapeHtml(label)}">
              <b>${escapeHtml(label)}</b>
              <span class="panel-note">${escapeHtml([...new Set(names)].filter((n) => n !== label).join(" · "))}</span>
              ${term.kind ? `<span class="cf-kind">${escapeHtml(term.kind)}${term.game && term.game !== "S3" ? ` · ${escapeHtml(term.game)}` : ""}</span>` : ""}
            </button></li>`;
          })
          .join("")
      : `<li class="panel-note">${escapeHtml(t("alias.noMatch"))}</li>`;
    list.hidden = false;
  }

  let searchTimer = 0;
  document.addEventListener("input", (event) => {
    const input = event.target.closest?.("#cf-translate [data-term-search]");
    if (!input) return;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => searchTerms(input), SEARCH_MS);
  });

  /** Saves an alias form: teaches (add) or changes (edit) an alias */
  async function saveAlias(form) {
    const field = (name) => form.querySelector(`[data-${name}]`)?.value ?? "";
    const note = form.querySelector("[data-form-note]");
    const term = field("term");
    if (form.querySelector("[data-term-search]") && !term) {
      note.textContent = t("alias.pickTerm");
      return;
    }
    const body = {
      text: field("text"),
      lang: field("lang"),
      note: field("note"),
    };
    try {
      const alias =
        form.dataset.mode === "add"
          ? await api("knowledge/slang/add", "POST", { ...body, term })
          : await api("knowledge/slang/edit", "POST", {
              ...body,
              id: form.dataset.id,
              ...(term && { term }),
              ...(form.dataset.approve && { status: "approved" }),
            });
      composerNote(
        t("alias.saved", {
          text: alias.text,
          term: alias.term_name || alias.term,
        }),
      );
      form.remove();
      await loadSlang();
      const entry = tr.entries.find((e) => e.id === form.dataset.entry);
      if (entry) refreshTerms(entry);
    } catch (error) {
      note.textContent = t("alias.failed", { error: error.message });
    }
  }

  /** A translation's terms looked up again, after the glossary changed
   * (the page's copy; the history keeps what the translation used) */
  async function refreshTerms(entry) {
    try {
      const data = await api(
        `knowledge/glossary?${new URLSearchParams({ q: entry.text })}`,
      );
      entry.terms = data.terms;
      redraw(entry);
    } catch {
      // The old terms stay
    }
  }

  // Remember what is selected in a translation's source; an open "Teach
  // a word" form takes it as the word
  document.addEventListener("selectionchange", () => {
    const selection = document.getSelection();
    const text = selection?.toString().trim() ?? "";
    const node = selection?.anchorNode;
    const source = (
      node?.nodeType === Node.TEXT_NODE ? node.parentElement : node
    )?.closest?.("#cf-translations .cf-tr-source");
    if (!text || !source || text.length > 40) return;
    const li = source.closest(".cf-tr");
    slang.selected = { id: li.dataset.id, text };
    const input = li.querySelector(".cf-tr-teach [data-text]");
    if (input) {
      input.value = text;
      li.querySelector(".cf-tr-teach [data-lang]").value = guessLang(text);
    }
  });

  $("cf-translations").addEventListener("click", (event) => {
    const add = event.target.closest("[data-add-alias]");
    if (add) {
      const card = add.closest(".k-term");
      if (card.querySelector(".cf-alias-form")) return;
      const li = add.closest(".cf-tr");
      const entry = tr.entries.find((e) => e.id === li.dataset.id);
      const form = aliasForm({
        mode: "add",
        term: card.dataset.term,
        lang: guessLang(entry?.text ?? ""),
        entry: li.dataset.id,
      });
      card.append(form);
      form.querySelector("[data-text]").focus();
      return;
    }
    const teach = event.target.closest("[data-teach]");
    if (teach) {
      const li = teach.closest(".cf-tr");
      const box = teach.closest(".cf-tr-teach");
      if (box.querySelector(".cf-alias-form")) return;
      const word =
        slang.selected.id === li.dataset.id ? slang.selected.text : "";
      const hint = document.createElement("p");
      hint.className = "panel-note";
      hint.textContent = t("tr.teachHint");
      const form = aliasForm({
        mode: "add",
        pick: true,
        text: word,
        lang: guessLang(word || li.querySelector(".cf-tr-source").textContent),
        entry: li.dataset.id,
      });
      box.append(hint, form);
      form.querySelector(word ? "[data-term-search]" : "[data-text]").focus();
    }
  });

  // Forms in the translations and in the Slang panel
  for (const root of [$("cf-translations"), $("cf-slang")]) {
    root.addEventListener("submit", (event) => {
      const form = event.target.closest(".cf-alias-form");
      if (!form) return;
      event.preventDefault();
      saveAlias(form);
    });
    root.addEventListener("click", (event) => {
      const pick = event.target.closest("[data-pick-term]");
      if (pick) {
        const form = pick.closest("form");
        form.querySelector("[data-term]").value = pick.dataset.pickTerm;
        form.querySelector("[data-term-search]").value = pick.dataset.label;
        form.querySelector("[data-term-results]").hidden = true;
        form.querySelector("[data-form-note]").textContent = "";
        return;
      }
      if (event.target.closest("[data-form-cancel]")) {
        const form = event.target.closest("form");
        form.closest(".cf-tr-teach")?.querySelector(".panel-note")?.remove();
        const inPanel = Boolean(form.closest("#cf-slang"));
        form.remove();
        // The panel's item comes back in place of its form
        if (inPanel) drawSlang();
      }
    });
    // Enter in the term search picks the first match
    root.addEventListener("keydown", (event) => {
      const input = event.target.closest?.("[data-term-search]");
      if (!input || event.key !== "Enter" || event.isComposing) return;
      const first = input
        .closest("form")
        .querySelector("[data-term-results]:not([hidden]) [data-pick-term]");
      if (first) {
        event.preventDefault();
        first.click();
      }
    });
  }

  // --------------------------------------------------------- the panel

  /** The user glossary from the server, drawn */
  async function loadSlang() {
    try {
      const data = await api("knowledge/slang");
      Object.assign(slang, {
        aliases: data.aliases,
        file: data.file,
        pending: data.pending,
        backend: data.backend,
      });
    } catch (error) {
      $("cf-slang-note").textContent = error.message;
      return;
    }
    drawSlang();
  }

  /** One alias of the user glossary: the alias → its term, where it came
   * from, the evidence of a suggestion, and its actions */
  function aliasItem(alias) {
    const li = document.createElement("li");
    li.className = "cf-alias";
    li.dataset.id = alias.id;
    const term = alias.term_forms
      ? `<b class="cf-tr-to">${escapeHtml(termLabel(alias.term_forms, alias.term))}</b>`
      : `<span class="panel-note level-critical">${escapeHtml(t("slang.termGone"))}</span>`;
    const facts = [
      t(`slang.source.${alias.source}`),
      alias.confidence != null &&
        t("slang.confidence", { p: Math.round(alias.confidence * 100) }),
      alias.document && t("slang.from", { doc: alias.document }),
    ].filter(Boolean);
    const pending = alias.status === "pending";
    li.innerHTML = `
      <div class="k-hit-head">
        <b class="cf-tr-from" lang="${escapeHtml(alias.lang)}">${escapeHtml(alias.text)}</b>
        <span class="cf-kind">${escapeHtml(alias.lang)}</span>
        <span class="cf-tr-arrow">→</span>
        ${term}
        <span class="panel-note">${escapeHtml(facts.join(" · "))}</span>
      </div>
      ${alias.note ? `<p class="k-hit-text">${escapeHtml(alias.note)}</p>` : ""}
      ${alias.evidence ? `<blockquote class="cf-alias-evidence">${escapeHtml(alias.evidence)}</blockquote>` : ""}
      <div class="cf-alias-actions">
        ${
          pending
            ? `<button type="button" class="mode-toggle cf-approve" data-approve>${escapeHtml(t("slang.approve"))}</button>
               <button type="button" class="mode-toggle" data-reject>${escapeHtml(t("slang.reject"))}</button>`
            : ""
        }
        <button type="button" class="mode-toggle" data-edit>${escapeHtml(t("slang.edit"))}</button>
        ${pending ? "" : `<button type="button" class="mode-toggle" data-delete>${escapeHtml(t("slang.delete"))}</button>`}
      </div>`;
    return li;
  }

  function drawSlang() {
    const badge = $("cf-slang-badge");
    badge.hidden = !slang.pending;
    badge.textContent = slang.pending;
    $("cf-slang").hidden = !slang.open;
    $("cf-slang-toggle").setAttribute("aria-expanded", String(slang.open));
    const pending = slang.aliases.filter((a) => a.status === "pending");
    const taught = slang.aliases.filter((a) => a.status === "approved");
    $("cf-slang-note").textContent = slang.file
      ? t("slang.note", { n: taught.length, file: slang.file.split("/").pop() })
      : "";
    $("cf-slang-note").title = slang.file;
    const list = (id, aliases, empty) => {
      $(id).replaceChildren(
        ...(aliases.length
          ? aliases.map(aliasItem)
          : [
              Object.assign(document.createElement("li"), {
                className: "panel-note",
                textContent: t(empty),
              }),
            ]),
      );
    };
    list("cf-slang-pending", pending, "slang.noPending");
    list("cf-slang-taught", taught, "slang.noTaught");
  }

  /** Changes an alias of the panel (approve, reject) and draws the list */
  async function changeAlias(body) {
    try {
      await api("knowledge/slang/edit", "POST", body);
    } catch (error) {
      $("cf-slang-note").textContent = error.message;
    }
    loadSlang();
  }

  $("cf-slang").addEventListener("click", async (event) => {
    const li = event.target.closest(".cf-alias");
    if (!li || event.target.closest(".cf-alias-form")) return;
    const alias = slang.aliases.find((a) => a.id === li.dataset.id);
    if (!alias) return;
    if (event.target.closest("[data-approve]")) {
      changeAlias({ id: alias.id, status: "approved" });
    } else if (event.target.closest("[data-reject]")) {
      changeAlias({ id: alias.id, status: "rejected" });
    } else if (event.target.closest("[data-edit]")) {
      const form = aliasForm({
        mode: "edit",
        id: alias.id,
        pick: true,
        term: alias.term,
        termName: termLabel(alias.term_forms, alias.term),
        text: alias.text,
        lang: alias.lang,
        note: alias.note ?? "",
        approve: alias.status === "pending",
      });
      li.replaceChildren(form);
      form.querySelector("[data-text]").focus();
    } else if (event.target.closest("[data-delete]")) {
      if (!confirm(t("slang.deleteAsk", { text: alias.text }))) return;
      try {
        await api("knowledge/slang/delete", "POST", { id: alias.id });
      } catch (error) {
        $("cf-slang-note").textContent = error.message;
      }
      loadSlang();
    }
  });

  $("cf-slang-toggle").onclick = () => {
    slang.open = !slang.open;
    remember("slang-open", slang.open ? "1" : "0");
    drawSlang();
    if (slang.open) resumeRun();
  };

  // ------------------------------------------------- the suggestion run

  const runBox = $("cf-slang-run");

  function runNote(text, kind) {
    runBox.hidden = false;
    runBox.innerHTML = `<p class="panel-note ${kind === "error" ? "level-critical" : ""}">${escapeHtml(text)}</p>`;
  }

  /** The dry run: what a run of `max_batches` would read, with Run */
  async function planRun() {
    const batches = Number(remembered("slang-batches", "5")) || 5;
    runNote(t("slang.planning"));
    let plan;
    try {
      plan = await api("knowledge/slang/suggest", "POST", {
        dry_run: true,
        max_batches: batches,
      });
    } catch (error) {
      runNote(error.message, "error");
      return;
    }
    if (!plan.batches_total) {
      runNote(t("slang.nothing"));
      return;
    }
    runBox.innerHTML = `
      <p>${escapeHtml(
        t("slang.plan", {
          documents: number(plan.documents),
          chars: number(plan.chars),
          total: number(plan.batches_total),
          batches: number(plan.batches),
          charsRun: number(plan.chars_run),
        }),
      )}</p>
      ${plan.backend ? "" : `<p class="panel-note level-critical">${escapeHtml(t("slang.noBackend"))}</p>`}
      <div class="cf-alias-actions">
        <label class="check">${escapeHtml(t("slang.batches"))}
          <input type="number" class="select num cf-batches" min="1" max="50" value="${batches}" data-batches /></label>
        <button type="button" class="btn" data-run ${plan.backend ? "" : "disabled"}>${escapeHtml(t("slang.run"))}</button>
        <button type="button" class="mode-toggle" data-run-close>${escapeHtml(t("alias.cancel"))}</button>
      </div>`;
  }

  /** Follows the run's job until it ends, drawing new suggestions as
   * they come */
  async function followRun() {
    let seen = -1;
    while (slang.job != null) {
      let job;
      try {
        job = (await api("knowledge/jobs")).jobs.find(
          (j) => j.id === slang.job,
        );
      } catch {
        job = null;
      }
      if (!job) break;
      if (job.done !== seen) {
        seen = job.done;
        loadSlang();
      }
      if (job.state === "running") {
        runBox.innerHTML = `<p class="panel-note">${escapeHtml(t("slang.running", { done: job.done + 1, total: job.total ?? "?" }))} ${escapeHtml(job.lines.at(-1) ?? "")}</p>
          <div class="cf-alias-actions"><button type="button" class="mode-toggle" data-run-cancel>${escapeHtml(t("k.cancel"))}</button></div>`;
        runBox.hidden = false;
        await new Promise((resolve) => setTimeout(resolve, RUN_POLL_MS));
        continue;
      }
      slang.job = null;
      if (job.state === "done") {
        runNote(t("slang.done", { line: job.lines.at(-1) ?? "" }));
      } else {
        runNote(
          t("slang.failed", {
            error: job.error ?? job.lines.at(-1) ?? job.state,
          }),
          job.state === "failed" ? "error" : "",
        );
      }
      loadSlang();
    }
  }

  /** A run started earlier (before a reload) is followed again */
  async function resumeRun() {
    if (slang.job != null) return;
    try {
      const running = (await api("knowledge/jobs")).jobs.find(
        (j) => j.state === "running" && j.what.startsWith("Slang"),
      );
      if (!running) return;
      slang.job = running.id;
      followRun();
    } catch {
      // No run to follow
    }
  }

  $("cf-slang-suggest").onclick = () => {
    if (slang.job == null) planRun();
  };

  runBox.addEventListener("change", (event) => {
    const input = event.target.closest("[data-batches]");
    if (!input) return;
    const n = Math.min(50, Math.max(1, Math.round(Number(input.value) || 1)));
    remember("slang-batches", String(n));
    planRun();
  });

  runBox.addEventListener("click", async (event) => {
    if (event.target.closest("[data-run-close]")) {
      runBox.hidden = true;
    } else if (event.target.closest("[data-run-cancel]")) {
      try {
        await api("knowledge/cancel", "POST", {});
      } catch {
        // The run ends on its own
      }
    } else if (event.target.closest("[data-run]")) {
      const batches = Number(remembered("slang-batches", "5")) || 5;
      try {
        const started = await api("knowledge/slang/suggest", "POST", {
          max_batches: batches,
        });
        slang.job = started.job.id;
        followRun();
      } catch (error) {
        runNote(error.message, "error");
      }
    }
  });

  // -------------------------------------------------------------- routing

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const wasShown = tr.shown;
    tr.shown = app === "cuttlefish" && state.get("view") === "translate";
    $("cf-translate").hidden = !tr.shown;
    if (tr.shown && !wasShown) {
      load();
      checkKey();
      loadSlang();
      if (slang.open) resumeRun();
    }
  });

  // What is drawn from JavaScript follows the language
  window.addEventListener("lang-change", () => {
    drawChips();
    draw();
    markKey();
    drawSlang();
  });

  drawChips();
})();
