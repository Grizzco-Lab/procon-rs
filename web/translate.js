// Cuttlefish's Translate view (/cuttlefish/translate): jargon and
// callouts across languages, in the names each community uses. A chat-like
// page: the history above, the box at the bottom of the window with the
// mentor's avatar, a target-language picker and Send. A bare term shows its
// glossary entry at once (GET knowledge/glossary, no key needed), then the
// model's explanation; a sentence gets the terms it uses and the model's
// translation (POST translate, see src/cuttlefish.rs). The history is
// kept by the server in <reviews>/translations.jsonl. Runs after
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

  /** Whether the studio has the model's key; asked once. Without it the box
   * says the glossary answers alone. */
  async function checkKey() {
    if (tr.key == null) {
      try {
        tr.key = Boolean((await api("knowledge/model")).anthropic_key);
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

  /** The form of `term` the text mentions, for "熊刷 → Grizzco Roller";
   * the English name or the id when none is found (an imported term) */
  function sourceForm(term, text) {
    const hay = text.toLowerCase();
    let best = null;
    for (const form of Object.values(term.forms).flat()) {
      if (
        hay.includes(form.toLowerCase()) &&
        (!best || form.length > best.length)
      )
        best = form;
    }
    return best ?? term.forms.en?.[0] ?? term.id;
  }

  /** One glossary term as it was used: the form in the text → its name in
   * the target language, the definition and every other name */
  function termHtml(term, entry) {
    const lang = entry.target.split("-")[0];
    const to = term.forms[lang]?.[0];
    const forms = Object.entries(term.forms)
      .map(
        ([code, names]) =>
          `<span class="k-form"><b>${escapeHtml(code)}</b> ${escapeHtml(names.join(", "))}</span>`,
      )
      .join("");
    return `<li class="k-term">
      <div class="k-hit-head">
        <b class="cf-tr-from">${escapeHtml(sourceForm(term, entry.text))}</b>
        <span class="cf-tr-arrow">→</span>
        ${to ? `<b class="cf-tr-to">${escapeHtml(to)}</b>` : `<span class="panel-note">${escapeHtml(t("tr.noName", { lang: langName(entry.target) }))}</span>`}
        <span class="cf-kind">${escapeHtml(term.id)}</span>
      </div>
      ${term.definition ? `<p class="k-hit-text">${escapeHtml(term.definition)}</p>` : ""}
      <div class="k-forms">${forms}</div>
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
      ${terms}`;
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
      Object.values(term.forms)
        .flat()
        .some((form) => form.toLowerCase() === wanted)
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

  // -------------------------------------------------------------- routing

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const wasShown = tr.shown;
    tr.shown = app === "cuttlefish" && state.get("view") === "translate";
    $("cf-translate").hidden = !tr.shown;
    if (tr.shown && !wasShown) {
      load();
      checkKey();
    }
  });

  // What is drawn from JavaScript follows the language
  window.addEventListener("lang-change", () => {
    drawChips();
    draw();
    markKey();
  });

  drawChips();
})();
