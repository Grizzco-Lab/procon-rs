// The source popover of the Cuttlefish app: what an answer cites ([S1] in
// the chat, the sources under it) or where a Pedia quote comes from, shown
// in the page instead of sending the reader to Discord, where many are not
// members. A #vod-review comment shows in full with its conversation (the
// message it replies to and the replies to it), the reviewer, date and era,
// and a button per moment it places in the VOD that opens the community
// review there; a chunk of a wiki page, guide, game data or expert note
// shows its text with the title, section, licence and credit. The original
// stays a small link. Data from GET /api/cuttlefish/source (src/cuttlefish/pedia.rs).
// Beside the element clicked on a wide page, a sheet at the bottom on a
// phone; Escape, a click outside or leaving the view closes it. Shared as
// window.cuttlefishSource.open(anchor, ref) by cuttlefish.js and pedia.js;
// uses the helpers of i18n.js, app.js and player.js (t, escapeHtml,
// clock). window.cuttlefishSource.lookups(list) is what an answer looked
// up in the knowledge store (the tools' calls: searches with their
// filters, ids opened, Pedia entries, conversations, names), folded under
// it in the chat and the deep questions (cuttlefish.js, knowledge.js).
"use strict";

(() => {
  /** Space kept between the popover and the window's edges, in px */
  const EDGE = 12;
  /** Characters of a reply shown before it is cut */
  const REPLY_CHARS = 400;

  let pop = null;
  /** The element that opened it, to return the focus */
  let opener = null;
  /** The request being answered: a newer one wins */
  let asked = 0;

  /** The popover element, made on first use */
  function element() {
    if (pop) return pop;
    pop = document.createElement("div");
    pop.className = "src-pop";
    pop.id = "src-pop";
    pop.setAttribute("role", "dialog");
    pop.hidden = true;
    pop.innerHTML = `<button type="button" class="src-close" data-src-close>×</button><div class="src-body"></div>`;
    document.body.append(pop);
    pop.addEventListener("click", (event) => {
      if (event.target.closest("[data-src-close]")) close();
      // A review opens in the page; the popover goes with the view
      else if (event.target.closest("a[href^='/']")) setTimeout(close);
    });
    return pop;
  }

  function close() {
    if (!pop || pop.hidden) return;
    pop.hidden = true;
    opener?.focus?.();
    opener = null;
  }

  const phone = () =>
    document.documentElement.dataset.layout === "phone" ||
    window.innerWidth <= 720;

  /** Beside `anchor`: below it when there is room, else above; a sheet on
   * a phone */
  function place(anchor) {
    pop.classList.toggle("is-sheet", phone());
    if (phone()) {
      pop.style.left = pop.style.top = "";
      return;
    }
    const a = anchor.getBoundingClientRect();
    const w = pop.offsetWidth;
    const h = pop.offsetHeight;
    const left = Math.min(Math.max(EDGE, a.left), window.innerWidth - w - EDGE);
    const below = a.bottom + 6;
    const top =
      below + h <= window.innerHeight - EDGE
        ? below
        : Math.max(EDGE, a.top - h - 6);
    pop.style.left = `${left}px`;
    pop.style.top = `${top}px`;
  }

  const when = (time) =>
    new Date(time).toLocaleDateString(i18nLocale(), {
      year: "numeric",
      month: "short",
      day: "numeric",
    });

  /** `text` with `names` marked, escaped; plain when none are given */
  const marked = (text, names) =>
    names?.length && window.cuttlefishPediaMark
      ? window.cuttlefishPediaMark(text, names)
      : escapeHtml(text);

  const cut = (text, n) => (text.length > n ? `${text.slice(0, n)}…` : text);

  /** A message of the conversation, small */
  const aside = (m, label) =>
    `<div class="src-aside">
      <p class="src-aside-head">${escapeHtml(label)} <b>${escapeHtml(m.author)}</b> · ${escapeHtml(when(m.time))}</p>
      <p class="src-text">${escapeHtml(cut(m.text, REPLY_CHARS))}</p>
    </div>`;

  /** A #vod-review comment in its conversation */
  function commentHtml(data, ref) {
    const m = data.message;
    const at = (s) => clock(s).replace(/\.\d+$/, "");
    const moments = [...data.moments];
    // The moment the citing answer or the quote is about, when the message
    // itself does not name it
    const own = ref.expert?.t_s ?? ref.t_s;
    if (own != null && !moments.some((x) => Math.abs(x.t_s - own) < 1)) {
      moments.unshift({ raw: ref.expert?.moment ?? "", t_s: own });
    }
    const opens = data.review
      ? moments
          .map(
            (x) =>
              `<a class="btn src-open" href="/cuttlefish/review/${encodeURIComponent(data.review)}?t=${x.t_s}">${escapeHtml(t("src.openAt", { at: at(x.t_s) }))}${x.raw ? ` <span class="src-raw">${escapeHtml(x.raw)}</span>` : ""}</a>`,
          )
          .join("")
      : "";
    return `
      <header class="src-head">
        <span class="src-kind">#vod-review</span>
        <b>${escapeHtml(m.author)}</b>
        <span>${escapeHtml(when(m.time))}</span>
        <span class="pd-game" data-game="${escapeHtml(data.game)}">${escapeHtml(data.game)}</span>
      </header>
      <p class="panel-note src-vod">${escapeHtml(t(data.poster ? "src.ownVod" : "src.onVod", { poster: data.vod.poster, date: data.vod.date }))}</p>
      ${opens ? `<div class="src-opens">${opens}</div>` : ""}
      ${data.reply_to ? aside(data.reply_to, t("src.replyingTo")) : ""}
      <div class="src-text src-main">${marked(m.text, ref.names)}</div>
      ${data.replies.length ? `<p class="src-sub">${escapeHtml(t("src.replies", { n: data.replies.length }))}</p>${data.replies.map((r) => aside(r, "↳")).join("")}` : ""}
      <p class="src-foot"><a href="${escapeHtml(m.url)}" target="_blank" rel="noopener">${escapeHtml(t("src.discord"))}</a></p>`;
  }

  /** A chunk of a document */
  function chunkHtml(data, ref) {
    const kind = data.source ?? ref.source;
    const kindKey =
      {
        "discord-vod-review": "k.source.vodReview",
        "expert-note": "k.source.expertNote",
      }[kind] ?? `k.source.${kind}`;
    const kindName = kind ? t(kindKey) : "";
    const credit = [data.license ?? ref.license, data.attribution]
      .filter(Boolean)
      .map((x) => `<span>${escapeHtml(x)}</span>`)
      .join("");
    return `
      <header class="src-head">
        ${kindName && kindName !== kindKey ? `<span class="src-kind">${escapeHtml(kindName)}</span>` : ""}
        <b>${escapeHtml(data.title || ref.title || "")}</b>
        ${data.game === "S2" ? `<span class="pd-game" data-game="S2">S2</span>` : ""}
      </header>
      ${data.heading || ref.heading ? `<p class="panel-note src-vod">${escapeHtml(data.heading || ref.heading)}</p>` : ""}
      ${data.text ? `<div class="src-text src-main">${marked(data.text, ref.names)}</div>` : `<p class="panel-note">${escapeHtml(t("src.noText"))}</p>`}
      ${credit ? `<p class="src-credit">${credit}</p>` : ""}
      ${(data.url ?? ref.url) ? `<p class="src-foot"><a href="${escapeHtml(data.url ?? ref.url)}" target="_blank" rel="noopener">${escapeHtml(t("src.original"))}</a></p>` : ""}`;
  }

  /** Opens the popover beside `anchor` for `ref`: a cited source ({url,
   * doc, ordinal, title, heading, source, license, expert}) or a quote
   * ({url, t_s}); `names` are marked in the text */
  async function open(anchor, ref) {
    const box = element();
    const body = box.querySelector(".src-body");
    box
      .querySelector("[data-src-close]")
      .setAttribute("aria-label", t("src.close"));
    box.setAttribute("aria-label", t("src.title"));
    opener = anchor;
    body.innerHTML = `<p class="panel-note">${escapeHtml(t("cf.loading"))}</p>`;
    box.hidden = false;
    place(anchor);
    const n = ++asked;
    const query = new URLSearchParams();
    for (const key of ["url", "doc", "ordinal", "title", "heading"]) {
      if (ref[key] != null && ref[key] !== "") query.set(key, ref[key]);
    }
    let data;
    try {
      const response = await fetch(`/api/cuttlefish/source?${query}`);
      data = await response.json();
      if (!response.ok) throw new Error(data.error ?? response.statusText);
    } catch (error) {
      if (n !== asked) return;
      body.innerHTML = `<p class="panel-note level-critical">${escapeHtml(t("src.failed", { error: error.message }))}</p>
        ${ref.url ? `<p class="src-foot"><a href="${escapeHtml(ref.url)}" target="_blank" rel="noopener">${escapeHtml(t("src.original"))}</a></p>` : ""}`;
      return;
    }
    if (n !== asked || box.hidden) return;
    body.innerHTML =
      data.kind === "comment" ? commentHtml(data, ref) : chunkHtml(data, ref);
    body.scrollTop = 0;
    place(anchor);
    box.querySelector("[data-src-close]").focus({ preventScroll: true });
  }

  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && pop && !pop.hidden) {
      event.stopPropagation();
      close();
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (!pop || pop.hidden) return;
    if (pop.contains(event.target) || opener?.contains?.(event.target)) return;
    close();
  });
  window.addEventListener("app-route", close);
  window.addEventListener("resize", close);

  /** One call of what an answer looked up, as a line: what it asked (the
   * query with its filters, the id opened, the term, the conversation, the
   * names) and what it showed; the titles shown are its tooltip */
  function lookupItem(l) {
    const input = l.input ?? {};
    const found = l.found ?? [];
    let asked;
    switch (l.tool) {
      case "search": {
        const filters = [...(input.kinds ?? []), input.era].filter(Boolean);
        asked = escapeHtml(t("cf.look.search", { query: input.query ?? "" }));
        if (filters.length)
          asked += ` <span class="cf-kind">${escapeHtml(filters.join(", "))}</span>`;
        break;
      }
      case "open":
        asked = escapeHtml(t("cf.look.open", { id: input.id ?? "" }));
        break;
      case "pedia":
        asked = escapeHtml(t("cf.look.pedia", { term: input.term ?? "" }));
        break;
      case "thread":
        asked = escapeHtml(t("cf.look.thread", { id: input.id ?? "" }));
        break;
      case "names":
        asked = escapeHtml(t("cf.look.names", { text: input.text ?? "" }));
        break;
      default:
        asked = escapeHtml(`${l.tool} ${JSON.stringify(input)}`);
    }
    let shown;
    if (l.error) {
      shown = `<span class="level-critical">${escapeHtml(t("cf.look.failed", { error: l.error }))}</span>`;
    } else if (!found.length) {
      shown = escapeHtml(t("cf.look.nothing"));
    } else if (l.tool === "search") {
      shown = escapeHtml(t("cf.look.found", { n: found.length }));
    } else {
      shown = escapeHtml(found.map((f) => f.title).join(" · "));
    }
    const titles = found.map((f) => `${f.id} ${f.title}`).join("\n");
    return `<li${titles ? ` title="${escapeHtml(titles)}"` : ""}>${asked} → ${shown}</li>`;
  }

  /** What an answer looked up in the knowledge store (its `lookups`, the
   * agentic path), folded under it; nothing for an answer without them */
  function lookups(list) {
    if (!list?.length) return "";
    return `<details class="cf-sources cf-lookups"><summary>${escapeHtml(t("cf.look.title", { n: list.length }))}</summary><ol class="k-sources">${list.map(lookupItem).join("")}</ol></details>`;
  }

  window.cuttlefishSource = { open, close, lookups };
})();
