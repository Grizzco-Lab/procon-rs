// Cuttlefish app: chat with Cuttlefish (the AI), review a video with comments
// at its times and drawings on its paused frames, and notes on the whole
// video. The video plays in the shared player (player.js: transport, keys,
// scrubber with the comments as marks, neighbours every half second), with
// the drawings and the danmaku as layers over it. Runs after i18n.js,
// app.js, sketch.js, player.js and stages.js and uses their helpers (t, $,
// Sketch, Player, clock, escapeHtml, appUrl, StageMap). Its state lives in the address:
// /cuttlefish (the library: the reviews, "open a video" and the chat bar,
// whose first message starts a review without a video),
// /cuttlefish/review/<review>?t=<s> (a saved review),
// /cuttlefish/video?kind=<kind>&ref=<ref>&start_s=&end_s= (a video not
// reviewed yet), /cuttlefish/translate (the translator, see translate.js),
// /cuttlefish/knowledge (the knowledge view, see knowledge.js) or
// /cuttlefish/pedia[/<term>] (the Overfishing Pedia, see pedia.js). The
// tab strip above the library, #cf-tabs, switches the four views. Cited
// sources open in the source popover (source.js).
// Reviews are saved as JSON through /api/cuttlefish/reviews/<id>, each in a
// folder of its own with its YouTube (or copied) video and its chat; the
// format is in src/cuttlefish.rs. The page owns the review: a chat message
// goes to /api/cuttlefish/chat with the conversation so far, and both turns
// are saved with the review by the page.
"use strict";

(() => {
  /** A comment without an end shows its drawings this long, in seconds */
  const HOLD_S = 2;
  /** The chat inputs' placeholder changes this often, in ms */
  const PLACEHOLDER_MS = 5000;
  /** Characters of a chat's first message that name a review without a
   * video */
  const TOPIC_CHARS = 60;
  /** Lines a chat input grows to before it scrolls */
  const INPUT_ROWS = 6;
  /** A danmaku comment, and its drawings, show this long, in seconds */
  const DANMAKU_S = 5;
  /** Danmaku comments floating in the corner at most */
  const DANMAKU_STACK = 5;
  /** Sliding danmaku keeps to this band of the picture (fractions of its
   * height), below the game's top row of counters */
  const SLIDE_TOP = 0.12;
  const SLIDE_BOTTOM = 0.75;
  /** Characters of a comment shown as danmaku */
  const DANMAKU_CHARS = 120;
  /** Range reviewed by default: this long up to the current time */
  const RANGE_S = 15;
  /** What the lab sends with a message about the video, as in
   * `cuttlefish::sampling`: 15 frames around a moment at 720p, a range at
   * the chosen rate and height (at most MAX_RANGE_S long, MAX_FRAMES
   * frames), and past TWO_PASS_S an overview at SCOUT_FPS and SCOUT_HEIGHT,
   * then KEY_FRAMES at most around the key moments. Never above the
   * video's own height; an image costs about width × height / 750 tokens. */
  const AI = {
    MOMENT_FRAMES: 15,
    MOMENT_HEIGHT: 720,
    MAX_RANGE_S: 100,
    MAX_FRAMES: 60,
    TWO_PASS_S: 20,
    SCOUT_FPS: 0.5,
    SCOUT_HEIGHT: 360,
    KEY_FRAMES: 25,
  };
  /** Frame rate assumed until the video's is known */
  const DEFAULT_FPS = 30;
  /** Thumbnails on each side of the playhead in the neighbours strip */
  const STRIP_SIDE = 6;
  /** How often a missing YouTube title is asked about, in ms, and how many
   * times */
  const META_POLL_MS = 2500;
  const META_POLLS = 12;
  /** Drawing colors */
  const SWATCHES = ["#ff5c8a", "#ffd23f", "#4fb3ff", "#8bd450", "#ffffff"];
  const TOOLS = {
    v: "select",
    r: "rect",
    e: "ellipse",
    a: "arrow",
    f: "freehand",
  };

  const screen = $("cf-screen");
  const danmakuLayer = $("cf-danmaku");

  const cf = {
    /** Whether the Cuttlefish app is shown */
    shown: false,
    /** Session summaries from the Inkspector's API */
    sessions: null,
    /** The last reviews listing, drawn again when the language changes */
    listing: null,
    /** The open review: {video?, comments, notes, messages}, or null in the
     * library */
    review: null,
    /** Its folder name once saved */
    id: null,
    /** Id of the comment being edited, whose drawings can change */
    active: null,
    /** Id of the note being edited */
    editingNote: null,
    saveTimer: null,
    /** Saves run one after another */
    saving: Promise.resolve(),
    /** The chip's save state, [state, error] */
    saved: ["new"],
    /** Download to open once it is done */
    waiting: null,
    pollTimer: null,
    listTimer: null,
    metaTimer: null,
    /** Whole second the YouTube links point at */
    linkSecond: null,
    /** The open video's size, once known: {width, height} */
    meta: null,
  };

  /** The chat with Cuttlefish */
  const chat = {
    /** A message is on its way */
    sending: false,
    /** Whether ANTHROPIC_API_KEY is set where the lab runs; null until
     * asked */
    key: null,
    /** Which example the placeholders show */
    example: 0,
    placeholderTimer: null,
  };

  /** Danmaku: comments shown over the video as playback reaches them */
  const danmaku = {
    on: remembered("danmaku", "true") === "true",
    /** "float" (stacked in a corner) or "slide" (across the screen) */
    style: remembered("danmaku-style", "float"),
    /** Video time of the last look, or null after a seek */
    lastT: null,
  };

  const sketch = new Sketch(screen, {
    onChange: shapesChanged,
    onSelect: markTools,
    onDraw: startDrawing,
  });

  /** The shared player; the drawing layer and the danmaku are over it */
  const player = new Player({
    screen,
    controls: $("cf-player-controls"),
    scrubber: $("cf-scrubber"),
    strip: $("cf-strip"),
    stripNote: $("cf-strip-note"),
    remember: "cuttlefish",
    neighbours: { radius: STRIP_SIDE, seconds: 0.5 },
    onFrame(n) {
      if (!cf.review) return;
      const now = player.time();
      drawShapes();
      markLive(now);
      followLinks(now);
      if (player.playing) danmakuTick(now);
      else writeUrl();
      markCost();
    },
    onSeek() {
      clearDanmaku();
      danmaku.lastT = null;
    },
    onPlay(playing) {
      if (playing) {
        // Playing closes the open comment; a comment right at the start
        // of playback shows too
        cf.active = null;
        drawComments();
        sketch.setEditable(false);
        danmaku.lastT = player.time() - 0.05;
        danmakuLayer.classList.remove("is-paused");
      } else {
        sketch.setEditable(true);
        danmakuLayer.classList.add("is-paused");
        writeUrl();
      }
    },
    onMark(tick) {
      openComment(tick.id);
    },
    onError(message) {
      if (!cf.review) return;
      const note = $("cf-video-note");
      note.hidden = false;
      note.textContent ||= message;
    },
  });

  const playing = () => player.playing;

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

  // ------------------------------------------------------------- helpers

  /** "90", "1:30", "1:02.5" or "90s" in seconds; null if empty or bad */
  function parseTime(text) {
    const value = String(text ?? "")
      .trim()
      .replace(/s$/, "");
    if (!value) return null;
    const parts = value.split(":").map(Number);
    if (parts.some((p) => isNaN(p) || p < 0)) return null;
    return parts.reduce((total, part) => total * 60 + part, 0);
  }

  /** Seconds as "1:02.5", for inputs */
  function shortTime(seconds) {
    const minutes = Math.floor(seconds / 60);
    const rest = seconds - 60 * minutes;
    return `${minutes}:${rest.toFixed(1).padStart(4, "0")}`;
  }

  /** The query naming a video, for the video and meta endpoints and the
   * address; with the review `id`, a video in its folder plays from there */
  function videoQuery(v, id) {
    const query = new URLSearchParams({ kind: v.kind, ref: v.ref });
    if (v.start_s != null) query.set("start_s", v.start_s);
    if (v.end_s != null) query.set("end_s", v.end_s);
    if (v.file && id) {
      query.set("file", v.file);
      query.set("r", id);
    }
    return query;
  }

  function videoOf(state) {
    const time = (key) => (state.get(key) ? parseFloat(state.get(key)) : null);
    const v = { kind: state.get("kind"), ref: state.get("ref") ?? "" };
    if (time("start_s") != null) v.start_s = time("start_s");
    if (time("end_s") != null) v.end_s = time("end_s");
    return v;
  }

  const sameVideo = (a, b) =>
    a.kind === b.kind &&
    a.ref === b.ref &&
    (a.start_s ?? null) === (b.start_s ?? null) &&
    (a.end_s ?? null) === (b.end_s ?? null);

  /** A YouTube range as " · 4:56.0–5:56.0" */
  function rangeText(v) {
    if (v.start_s == null && v.end_s == null) return "";
    const end = v.end_s != null ? shortTime(v.end_s) : t("cf.range.end");
    return ` · ${shortTime(v.start_s ?? 0)}–${end}`;
  }

  /** What a video is called: a YouTube video by its title, a file by its
   * title when it has one (an imported VOD), else its name */
  function videoName(v) {
    if (v.kind === "file") return v.title ?? v.ref.split("/").pop();
    if (v.kind === "youtube")
      return v.title ?? v.ref.replace(/^https?:\/\/(www\.)?/, "");
    return v.ref;
  }

  /** Channel and upload date of a YouTube video */
  const channelText = (v) =>
    [v.channel, v.upload_date].filter(Boolean).join(" · ");

  /** The original YouTube video at `seconds` into the range, or null */
  function youtubeAt(v, seconds = 0) {
    if (v?.kind !== "youtube") return null;
    try {
      const url = new URL(v.ref);
      const at = Math.max(0, Math.floor((v.start_s ?? 0) + seconds));
      url.searchParams.set("t", `${at}s`);
      return url.href;
    } catch {
      return null;
    }
  }

  const kindName = (kind) =>
    ({
      session: t("cf.kind.session"),
      file: t("cf.kind.file"),
      youtube: t("cf.kind.youtube"),
      chat: t("cf.kind.chat"),
    })[kind] ?? kind;

  /** The first message of a chat, shortened, as a review's name */
  function topicOf(topic) {
    const text = String(topic ?? "")
      .trim()
      .split("\n")[0];
    if (!text) return t("cf.kind.chat");
    return text.length > TOPIC_CHARS ? `${text.slice(0, TOPIC_CHARS)}…` : text;
  }

  /** What a review is called: its title (an imported review's: the poster
   * and the day), its video, or the chat's first message */
  function reviewName(review) {
    if (review.title) return review.title;
    if (review.video) return videoName(review.video);
    return topicOf(review.messages?.find((m) => m.role === "user")?.text);
  }

  /** A comment or note from the #vod-review archive */
  const isCommunity = (c) => c.source?.from === "discord";

  /** The game era as shown */
  const eraName = (game) => (game ? t(`cf.era.${game}`) : "");

  /** The library's filter: all, mine (made here) or community (imported) */
  const FILTER_KEY = "procon-cuttlefish-filter";
  function libraryFilter() {
    try {
      const f = localStorage.getItem(FILTER_KEY);
      return ["mine", "community"].includes(f) ? f : "all";
    } catch {
      return "all";
    }
  }

  /** Author as shown */
  const authorName = (author) =>
    author === "user"
      ? t("cf.author.you")
      : author === "Cuttlefish"
        ? t("cf.name")
        : author;

  /** A new id: a comment's or a note's, or a review's from the local time */
  const newId = (prefix) =>
    `${prefix}${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

  function newReviewId() {
    const d = new Date();
    const pad = (n) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}-${pad(d.getMinutes())}-${pad(d.getSeconds())}`;
  }

  /** The last view, for the app link after leaving or a reload */
  function rememberView(url) {
    document.querySelector('.app-nav [data-app="cuttlefish"]').href = url;
    remember("view", url);
  }

  /** The top bar's chip: the video (a link to YouTube when it is one) and
   * `text` */
  function setChip(text, level = "off") {
    const chip = $("cf-chip");
    chip.hidden = !text;
    chip.dataset.level = level;
    const label = chip.querySelector(".chip-text");
    const v = cf.review?.video;
    const href = youtubeAt(v, player.time());
    if (!text) return label.replaceChildren();
    const name = cf.review ? reviewName(cf.review) : "";
    label.innerHTML = href
      ? `<a class="cf-yt" href="${escapeHtml(href)}" target="_blank" rel="noopener" title="${escapeHtml(t("cf.youtube.open"))}">${escapeHtml(name)} ↗</a> · ${escapeHtml(text)}`
      : escapeHtml(name ? `${name} · ${text}` : text);
  }

  /** Point the YouTube links of the open review at the playhead */
  function followLinks(seconds) {
    const second = Math.floor(seconds);
    if (second === cf.linkSecond) return;
    cf.linkSecond = second;
    const href = youtubeAt(cf.review?.video, seconds);
    if (!href) return;
    for (const link of document.querySelectorAll(
      "#cf-chip .cf-yt, #cf-info .cf-yt",
    )) {
      link.href = href;
    }
  }

  // ------------------------------------------------------------- library

  async function showLibrary() {
    leavePlayer();
    $("cf-player").hidden = true;
    $("cf-library").hidden = false;
    setChip("");
    rememberView("/cuttlefish");
    loadSessions();
    loadReviews();
    pollDownloads();
    drawChips($("cf-entry-chips"), $("cf-entry-text"), entryChips());
    checkKey();
    loadDeep();
  }

  /** The session pickers: the library's "Open a video" and the player's
   * "Attach a video" */
  const SESSION_PICKERS = [
    ["cf-session", "cf-segment"],
    ["cf-attach-session-pick", "cf-attach-segment"],
  ];

  async function loadSessions() {
    if (cf.sessions) return;
    try {
      const response = await fetch("/api/inspect/sessions");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      cf.sessions = data.sessions;
    } catch (error) {
      // Aborted (the app was left): read when shown again
      if (isAbort(error)) return;
      cf.sessions = [];
      for (const [sessions] of SESSION_PICKERS) {
        $(sessions).replaceChildren(
          new Option(
            t("cf.open.noSessionsBecause", { error: error.message }),
            "",
          ),
        );
      }
      return;
    }
    for (const [sessions, segments] of SESSION_PICKERS) {
      const select = $(sessions);
      select.replaceChildren(
        ...cf.sessions.map((s) => new Option(s.name, s.name)),
      );
      if (!cf.sessions.length)
        select.add(new Option(t("cf.open.noSessions"), ""));
      fillSegments(sessions, segments);
    }
  }

  function fillSegments(sessions, segments) {
    const summary = cf.sessions?.find((s) => s.name === $(sessions).value);
    const select = $(segments);
    select.replaceChildren(
      ...(summary?.segments ?? []).map(
        (s) => new Option(s.file + (s.sound ? " ♪" : ""), s.file),
      ),
    );
    select.hidden = (summary?.segments.length ?? 0) < 2;
  }

  /** The video a session picker names */
  function pickedSession(sessions, segments) {
    const session = $(sessions).value;
    if (!session) return null;
    const segment = $(segments).value;
    return {
      kind: "session",
      ref: segment ? `${session}/${segment}` : session,
    };
  }

  async function loadReviews() {
    clearTimeout(cf.listTimer);
    const note = $("cf-reviews-note");
    let data;
    try {
      const response = await fetch("/api/cuttlefish/reviews");
      data = await response.json();
      if (!response.ok) throw new Error(data.error);
    } catch (error) {
      if (!isAbort(error)) note.textContent = error.message;
      return;
    }
    cf.listing = data;
    drawReviews();
    // Titles being looked up, and the list being read again, come in a
    // moment
    if ((data.fetching?.length || data.refreshing) && cf.shown && !cf.review) {
      cf.listTimer = setTimeout(loadReviews, 2000);
    }
  }

  /** When a review changed: the date, then the time without seconds, on
   * two lines where the table is narrow */
  function changedText(ms) {
    const date = new Date(ms);
    const locale = i18nLocale();
    return `${date.toLocaleDateString(locale)}\n${date.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" })}`;
  }

  function drawReviews() {
    const data = cf.listing;
    if (!data) return;
    const body = $("cf-reviews");
    const filter = libraryFilter();
    for (const button of $("cf-filter").querySelectorAll("[data-filter]")) {
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.filter === filter),
      );
    }
    const community = (r) => r.from === "discord";
    const reviews = data.reviews.filter((r) =>
      filter === "all"
        ? true
        : filter === "community"
          ? community(r)
          : !community(r),
    );
    $("cf-reviews-note").textContent = t("cf.reviews.count", {
      n: reviews.length,
      dir: data.dir,
    });
    body.replaceChildren();
    if (!reviews.length) {
      const empty = data.reviews.length
        ? t("cf.reviews.noneFiltered")
        : t("cf.reviews.none");
      body.innerHTML = `<tr><td colspan="4" class="panel-note">${escapeHtml(empty)}</td></tr>`;
    }
    const fetching = new Set(data.fetching ?? []);
    for (const review of reviews) {
      const v = review.video;
      const tr = document.createElement("tr");
      const details = [];
      if (review.title && v) details.push(videoName(v));
      if (v?.kind === "youtube") {
        if (!v.title && fetching.has(review.id))
          details.push(t("cf.youtube.lookingUp"));
        if (channelText(v)) details.push(channelText(v));
      }
      if (v?.file) details.push(t("cf.reviews.fileIn", { file: v.file }));
      if (!v) details.push(t("cf.reviews.noVideo"));
      if (review.game) details.push(eraName(review.game));
      if (stageById(review.stage))
        details.push(stageName(stageById(review.stage)));
      if (community(review)) details.push(t("cf.reviews.community"));
      if (review.eggstra_event)
        details.push(t("cf.reviews.eggstra", { n: review.eggstra_event }));
      if (review.messages)
        details.push(t("cf.reviews.messages", { n: review.messages }));
      details.push(review.id);
      const href = v ? youtubeAt(v) : null;
      const link = href
        ? ` <a class="cf-yt" href="${escapeHtml(href)}" target="_blank" rel="noopener" title="${escapeHtml(t("cf.youtube.openStart"))}">YouTube ↗</a>`
        : "";
      // A review without a video is named by its first message
      const kind = v ? kindName(v.kind) : kindName("chat");
      const name = review.title ?? (v ? videoName(v) : topicOf(review.topic));
      tr.innerHTML = `
        <td><span class="cf-kind">${escapeHtml(kind)}</span> <span class="cf-video-name">${escapeHtml(name)}</span>${v ? escapeHtml(rangeText(v)) : ""}<br><span class="panel-note">${escapeHtml(details.join(" · "))}${link}</span></td>
        <td class="num">${review.comments}</td>
        <td class="cf-changed">${escapeHtml(changedText(review.modified_ms))}</td>
        <td><button type="button" class="mode-toggle" data-delete>${escapeHtml(t("cf.delete"))}</button></td>`;
      tr.onclick = (event) => {
        if (event.target.closest("a")) return;
        if (event.target.closest("[data-delete]")) {
          deleteReview(review.id, v?.file);
          return;
        }
        navigate(appUrl("cuttlefish", { r: review.id }));
      };
      body.append(tr);
    }
  }

  $("cf-filter").addEventListener("click", (event) => {
    const button = event.target.closest("[data-filter]");
    if (!button) return;
    try {
      localStorage.setItem(FILTER_KEY, button.dataset.filter);
    } catch {
      // The choice holds until reload
    }
    drawReviews();
  });

  async function deleteReview(id, file) {
    const question = file
      ? t("cf.reviews.deleteWithVideo", { id, file })
      : t("cf.reviews.deleteAsk", { id });
    if (!confirm(question)) return;
    const response = await fetch(
      `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
      { method: "DELETE", body: "{}" },
    );
    if (!response.ok) alert((await response.json()).error);
    loadReviews();
  }

  function openReviewUrl(id) {
    navigate(appUrl("cuttlefish", { r: id }));
  }

  /** Open a video not reviewed yet */
  function openVideoUrl(v) {
    navigate(appUrl("cuttlefish", videoQuery(v)));
  }

  function openError(message) {
    const note = $("cf-open-error");
    note.hidden = !message;
    note.textContent = message ?? "";
  }

  for (const [sessions, segments] of SESSION_PICKERS) {
    $(sessions).onchange = () => fillSegments(sessions, segments);
  }
  $("cf-form-session").onsubmit = (event) => {
    event.preventDefault();
    const v = pickedSession("cf-session", "cf-segment");
    if (v) openVideoUrl(v);
  };
  $("cf-form-file").onsubmit = (event) => {
    event.preventDefault();
    openVideoUrl({ kind: "file", ref: $("cf-file").value.trim() });
  };

  /** Start downloading a YouTube range: into a new review, or into the
   * review `into`, which has no video. `url`, `from` and `to` are the form's
   * inputs; errors go to `showError`. */
  async function startDownload(url, from, to, into, showError) {
    showError(null);
    const body = { url: $(url).value.trim() };
    for (const [key, id] of [
      ["start_s", from],
      ["end_s", to],
    ]) {
      const text = $(id).value;
      const seconds = parseTime(text);
      if (text.trim() && seconds == null)
        return showError(t("cf.open.badTime", { text }));
      if (seconds != null) body[key] = seconds;
    }
    if (into) body.review = into;
    const response = await fetch("/api/cuttlefish/download", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const download = await response.json();
    if (!response.ok) return showError(download.error);
    // The download is the new review (or the one that has the video)
    if (download.state === "done") return downloaded(download.id);
    cf.waiting = download.id;
    pollDownloads();
  }

  /** A download finished: open its review, or give the open review (the
   * one it went into) the video the server wrote for it; the page's copy
   * keeps its newer messages */
  async function downloaded(id) {
    if (cf.review && cf.id === id) {
      const response = await fetch(
        `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
      );
      const stored = await response.json();
      if (!response.ok) return attachError(stored.error);
      if (cf.id !== id || !stored.video) return;
      attachVideo(stored.video);
      return;
    }
    openReviewUrl(id);
  }

  $("cf-form-youtube").onsubmit = (event) => {
    event.preventDefault();
    startDownload("cf-url", "cf-from", "cf-to", null, openError);
  };

  /** Show the downloads (in the library, or under "Attach a video"), and
   * keep asking while one runs */
  async function pollDownloads() {
    clearTimeout(cf.pollTimer);
    let data;
    try {
      data = await (await fetch("/api/cuttlefish/downloads")).json();
    } catch {
      return;
    }
    const states = {
      done: t("cf.download.done"),
      failed: t("cf.download.failed"),
    };
    const list = cf.review ? $("cf-attach-downloads") : $("cf-downloads");
    const showError = cf.review ? attachError : openError;
    list.replaceChildren(
      ...data.downloads.map((d) => {
        const li = document.createElement("li");
        li.className = "cf-download meter";
        li.dataset.state = d.state;
        const range = rangeText(d);
        const percent = d.percent ?? 0;
        li.innerHTML = `
          <div class="cf-download-head"><span class="cf-download-url">${escapeHtml(d.url)}${escapeHtml(range)}</span><span class="num">${d.state === "running" ? `${percent.toFixed(0)}%` : escapeHtml(states[d.state] ?? d.state)}</span></div>
          <div class="meter-track"><div class="meter-fill" style="width:${d.state === "done" ? 100 : percent}%"></div></div>
          <span class="panel-note ${d.state === "failed" ? "level-critical" : ""}">${escapeHtml(d.message)}</span>`;
        if (d.state === "done") li.onclick = () => downloaded(d.id);
        return li;
      }),
    );
    const waiting = data.downloads.find((d) => d.id === cf.waiting);
    if (waiting?.state === "failed") {
      cf.waiting = null;
      showError(waiting.message);
    } else if (waiting?.state === "done") {
      cf.waiting = null;
      return downloaded(waiting.id);
    }
    if (cf.shown && data.downloads.some((d) => d.state === "running")) {
      cf.pollTimer = setTimeout(pollDownloads, 1000);
    }
  }

  // ------------------------------------------------------ attach a video

  function attachError(message) {
    const note = $("cf-attach-error");
    note.hidden = !message;
    note.textContent = message ?? "";
  }

  /** Give the open review (started from the chat) its video and show it */
  function attachVideo(v) {
    if (!cf.review || cf.review.video) return;
    const review = cf.review;
    const id = cf.id;
    review.video = v;
    leavePlayer();
    openReview(review, id, 0);
    scheduleSave();
  }

  $("cf-attach-session").onsubmit = (event) => {
    event.preventDefault();
    const v = pickedSession("cf-attach-session-pick", "cf-attach-segment");
    if (v) attachVideo(v);
  };
  $("cf-attach-file").onsubmit = (event) => {
    event.preventDefault();
    const ref = $("cf-attach-path").value.trim();
    if (ref) attachVideo({ kind: "file", ref });
  };
  $("cf-attach-youtube").onsubmit = (event) => {
    event.preventDefault();
    if (!cf.id) return;
    startDownload(
      "cf-attach-url",
      "cf-attach-from",
      "cf-attach-to",
      cf.id,
      attachError,
    );
  };

  // -------------------------------------------------------------- player

  /** Show a review (or a new one of `v`) at time `t`; a review without a
   * video shows its chat and "Attach a video" */
  async function openReview(review, id, at) {
    clearTimeout(cf.pollTimer);
    clearTimeout(cf.listTimer);
    review.notes ??= [];
    review.messages ??= [];
    cf.review = review;
    cf.id = id;
    cf.active = null;
    cf.editingNote = null;
    cf.linkSecond = null;
    $("cf-library").hidden = true;
    $("cf-player").hidden = false;
    const withVideo = Boolean(review.video);
    $("cf-player").classList.toggle("is-chat", !withVideo);
    for (const panel of ["p-cf-video", "p-cf-comments", "p-cf-notes"]) {
      document.querySelector(`.${panel}`).hidden = !withVideo;
    }
    document.querySelector(".p-cf-attach").hidden = withVideo;
    attachError(null);
    const note = $("cf-video-note");
    note.hidden = true;
    note.textContent = "";
    screen.style.aspectRatio = "";
    cf.meta = null;
    clearDanmaku();
    markSaved(id ? "saved" : "new");
    drawChat();
    checkKey();
    loadDeep();
    if (!withVideo) {
      writeUrl(0);
      pollDownloads();
      return;
    }
    // The frame rate is assumed until the metadata comes; the time holds
    player.open(videoSource(review.video, id), Math.round(at * DEFAULT_FPS));
    markCopy();
    drawInfo();
    drawStage();
    drawNotes();
    // A video opens paused, ready to draw on
    sketch.setEditable(true);
    drawComments();
    drawMarkers();
    writeUrl(at);
    lookForMeta(review, id, META_POLLS);
    try {
      const response = await fetch(
        `/api/cuttlefish/meta?${videoQuery(review.video, id)}`,
      );
      const meta = await response.json();
      if (!response.ok) throw new Error(meta.error);
      if (cf.review !== review) return;
      player.setFps(meta.fps || DEFAULT_FPS);
      cf.meta = { width: meta.width, height: meta.height };
      markCost();
      drawMarkers();
      // The layer covers the picture exactly, so its fractions are the frame's
      if (meta.width && meta.height)
        screen.style.aspectRatio = `${meta.width} / ${meta.height}`;
    } catch (error) {
      if (isAbort(error)) return;
      note.hidden = false;
      note.textContent = error.message;
    }
  }

  /** A video as the player's source: played and thumbnailed by the lab */
  function videoSource(v, id) {
    const query = videoQuery(v, id);
    return {
      video: `/api/cuttlefish/video?${query}`,
      fps: player.fps,
      sound: true,
      thumb(n) {
        const q = new URLSearchParams(query);
        q.set("t_ms", Math.round((n * 1000) / player.fps));
        return `/api/cuttlefish/thumb?${q}`;
      },
    };
  }

  /** Take the title, channel and date the server has for the video */
  function takeMeta(v) {
    if (!cf.review?.video || !v || !sameVideo(cf.review.video, v)) return;
    let changed = false;
    for (const key of ["title", "channel", "upload_date"]) {
      if (v[key] && cf.review.video[key] !== v[key]) {
        cf.review.video[key] = v[key];
        changed = true;
      }
    }
    if (!changed) return;
    drawInfo();
    drawStage();
    markSaved(...cf.saved);
  }

  /** A saved YouTube review without its title gets it in the background:
   * ask again for a while */
  function lookForMeta(review, id, tries) {
    clearTimeout(cf.metaTimer);
    if (!id || review.video?.kind !== "youtube" || review.video.title) return;
    if (tries <= 0) return;
    cf.metaTimer = setTimeout(async () => {
      if (cf.review !== review) return;
      try {
        const response = await fetch(
          `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
        );
        if (response.ok) takeMeta((await response.json()).video);
      } catch {
        // Asked again below
      }
      if (cf.review === review) lookForMeta(review, id, tries - 1);
    }, META_POLL_MS);
  }

  /** The video's title, channel, date and range above the picture */
  function drawInfo() {
    const box = $("cf-info");
    const v = cf.review?.video;
    if (!v) return box.replaceChildren();
    const href = youtubeAt(v, player.time());
    const name = escapeHtml(videoName(v));
    const title = href
      ? `<a class="cf-yt cf-info-title" href="${escapeHtml(href)}" target="_blank" rel="noopener" title="${escapeHtml(t("cf.youtube.open"))}">${name} ↗</a>`
      : `<span class="cf-info-title">${name}</span>`;
    const details = [kindName(v.kind)];
    if (v.kind === "youtube") {
      if (channelText(v)) details.push(channelText(v));
      else if (!v.title && cf.id) details.push(t("cf.youtube.lookingUp"));
      if (v.start_s != null || v.end_s != null)
        details.push(
          t("cf.youtube.range", {
            range: rangeText(v).slice(3),
          }),
        );
    }
    box.innerHTML = `${title}<span class="panel-note">${escapeHtml(details.join(" · "))}</span>`;
  }

  /** The review's stage and the links to Gungee's maps: the stage picked
   * (saved with the review), else the one the video's title names */
  const stageMap = new StageMap($("cf-stage"), (id) => {
    if (!cf.review) return;
    if (id) cf.review.stage = id;
    else delete cf.review.stage;
    scheduleSave();
  });

  function drawStage() {
    const review = cf.review;
    stageMap.set(
      review?.stage ??
        stageFromText(review?.video?.title) ??
        stageFromText(review?.title) ??
        "",
    );
  }

  /** "Copy into review": a saved review of a local file not copied yet */
  function markCopy() {
    const v = cf.review?.video;
    $("cf-copy").hidden = !(cf.id && v?.kind === "file" && !v.file);
  }

  $("cf-copy").onclick = async () => {
    const id = cf.id;
    const button = $("cf-copy");
    button.disabled = true;
    button.textContent = t("cf.copy.running");
    try {
      const response = await fetch(
        `/api/cuttlefish/reviews/${encodeURIComponent(id)}/copy`,
        { method: "POST", body: "{}" },
      );
      const review = await response.json();
      if (!response.ok) throw new Error(review.error);
      if (cf.id !== id) return;
      // Play the copy from the review folder, where it stays
      cf.review.video = review.video;
      player.open(videoSource(review.video, id), player.frame);
      drawMarkers();
    } catch (error) {
      alert(t("cf.copy.failed", { error: error.message }));
    } finally {
      button.disabled = false;
      button.textContent = t("cf.copy");
      markCopy();
    }
  };

  /** Stop the video when leaving it */
  function leavePlayer() {
    flushSave();
    player.close();
    clearTimeout(cf.metaTimer);
    cf.review = null;
    cf.id = null;
    cf.active = null;
    cf.editingNote = null;
    clearDanmaku();
    sketch.set([]);
  }

  /** Keep the review and its playhead in the address, replacing the
   * current history entry */
  function writeUrl(at = player.time()) {
    if (!cf.review) return;
    const params = cf.id
      ? new URLSearchParams({ r: cf.id })
      : videoQuery(cf.review.video);
    if (cf.review.video && at > 0) params.set("t", at.toFixed(3));
    rememberView(replaceRoute("cuttlefish", params));
  }

  /** Pause at a time in seconds */
  function seek(at) {
    player.seek(at);
  }

  // ----------------------------------------------------------- drawings

  const activeComment = () =>
    cf.review?.comments.find((c) => c.id === cf.active) ?? null;

  /** Comments whose drawings show at time `at`: while danmaku plays, as
   * long as the comment floats */
  function visibleAt(at, comment) {
    const end =
      danmaku.on && playing()
        ? Math.max(comment.t_end_s ?? 0, comment.t_s + DANMAKU_S)
        : (comment.t_end_s ?? comment.t_s + HOLD_S);
    return at >= comment.t_s - 1e-3 && at <= end;
  }

  /** The drawings shown now: the open comment's (editable while paused) and
   * those of the comments playback is passing */
  function drawShapes() {
    if (!cf.review || sketch.drag) return;
    const now = player.time();
    const active = activeComment();
    const shapes = [];
    for (const comment of cf.review.comments) {
      if (comment === active || !visibleAt(now, comment)) continue;
      for (const shape of comment.shapes)
        shapes.push({ ...shape, locked: true, faded: !!active });
    }
    if (active) shapes.push(...active.shapes.map((s) => ({ ...s })));
    sketch.set(shapes);
  }

  /** The layer changed: its editable shapes are the open comment's */
  function shapesChanged(shapes) {
    const active = activeComment();
    if (!active) return;
    active.shapes = shapes
      .filter((s) => !s.locked)
      .map((s) => ({
        kind: s.kind,
        points: s.points.map(([x, y]) => [
          Math.round(x * 1e4) / 1e4,
          Math.round(y * 1e4) / 1e4,
        ]),
        ...(s.color && { color: s.color }),
      }));
    drawComments();
    scheduleSave();
  }

  /** Drawing on a paused frame with no comment open starts one here */
  function startDrawing() {
    if (!cf.review || playing()) return false;
    if (!activeComment()) {
      addComment(false);
    }
    return true;
  }

  function setTool(tool) {
    sketch.setTool(tool);
    remember("tool", tool);
    markTools();
  }

  function setColor(color) {
    sketch.color = color;
    remember("color", color);
    markTools();
  }

  function markTools() {
    for (const button of document.querySelectorAll(".cf-tools [data-tool]")) {
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.tool === sketch.tool),
      );
    }
    for (const button of $("cf-swatches").children) {
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.color === sketch.color),
      );
    }
    const shape = sketch.shapes[sketch.selected];
    $("cf-delete-shape").disabled = !shape || shape.locked;
  }

  // ------------------------------------------------------------- danmaku

  /** Comments playback passed since the last look float up */
  function danmakuTick(now) {
    if (!danmaku.on || !cf.review) return;
    const last = danmaku.lastT;
    danmaku.lastT = now;
    // A jump is a seek, not playback
    if (last == null || now < last || now - last > 1) return;
    for (const comment of cf.review.comments) {
      if (comment.t_s > last && comment.t_s <= now) shoot(comment);
    }
  }

  /** A comment as danmaku text: its first words, or its drawings */
  function danmakuText(comment) {
    const text = comment.text.trim().split("\n")[0];
    if (!text)
      return `✎ ${drawingsText(comment.shapes.length) || t("cf.noText")}`;
    return text.length > DANMAKU_CHARS
      ? `${text.slice(0, DANMAKU_CHARS)}…`
      : text;
  }

  /** Show one comment over the video in the chosen style */
  function shoot(comment) {
    const item = document.createElement("div");
    item.className = "cf-dm";
    if (comment.author !== "user") item.classList.add("is-ai");
    item.innerHTML = `<b>${escapeHtml(authorName(comment.author))}</b> ${escapeHtml(danmakuText(comment))}`;
    item.style.setProperty("--dm-s", `${DANMAKU_S}s`);
    item.addEventListener("animationend", () => item.remove());
    if (danmaku.style === "slide") {
      item.classList.add("is-slide");
      danmakuLayer.append(item);
      const layer = danmakuLayer.getBoundingClientRect();
      const height = item.offsetHeight + 4;
      const lanes = Math.max(
        1,
        Math.floor((layer.height * (SLIDE_BOTTOM - SLIDE_TOP)) / height),
      );
      // The first lane whose last comment has fully come in, else the one
      // whose last comment is furthest along
      let best = 0;
      let bestRight = Infinity;
      for (let lane = 0; lane < lanes; lane++) {
        const last = [...danmakuLayer.querySelectorAll(".is-slide")]
          .filter((el) => el !== item && Number(el.dataset.lane) === lane)
          .pop();
        const right = last ? last.getBoundingClientRect().right : -Infinity;
        if (right < layer.right - 24) {
          best = lane;
          break;
        }
        if (right < bestRight) {
          bestRight = right;
          best = lane;
        }
      }
      item.dataset.lane = best;
      item.style.top = `${layer.height * SLIDE_TOP + best * height}px`;
      item.style.setProperty(
        "--dm-travel",
        `${layer.width + item.offsetWidth}px`,
      );
    } else {
      let stack = danmakuLayer.querySelector(".cf-dm-stack");
      if (!stack) {
        stack = document.createElement("div");
        stack.className = "cf-dm-stack";
        danmakuLayer.append(stack);
      }
      stack.append(item);
      while (stack.children.length > DANMAKU_STACK)
        stack.firstElementChild.remove();
    }
  }

  function clearDanmaku() {
    danmakuLayer.replaceChildren();
  }

  function markDanmaku() {
    const button = $("cf-danmaku-toggle");
    button.setAttribute("aria-pressed", String(danmaku.on));
    $("cf-danmaku-style").value = danmaku.style;
    $("cf-danmaku-style").disabled = !danmaku.on;
  }

  $("cf-danmaku-toggle").onclick = () => {
    danmaku.on = !danmaku.on;
    remember("danmaku", String(danmaku.on));
    if (!danmaku.on) clearDanmaku();
    markDanmaku();
    drawShapes();
  };
  $("cf-danmaku-style").onchange = () => {
    danmaku.style = $("cf-danmaku-style").value;
    remember("danmaku-style", danmaku.style);
    clearDanmaku();
  };

  // ----------------------------------------------------- neighbours strip

  // The strip is the player's; the spacing is chosen here
  $("cf-spacing").value = String(player.spacing);
  $("cf-spacing").onchange = () => {
    player.setSpacing(parseFloat($("cf-spacing").value) || 0.5);
  };

  // ------------------------------------------------------------ comments

  /** A comment by the user at the current time, opened for editing */
  function addComment(focus = true) {
    if (!cf.review?.video) return;
    player.pause();
    const comment = {
      id: newId("c"),
      t_s: Math.round(player.time() * 1000) / 1000,
      author: "user",
      text: "",
      shapes: [],
      created_ms: Date.now(),
    };
    cf.review.comments.push(comment);
    cf.active = comment.id;
    commentsChanged();
    if (focus) $("cf-comments").querySelector("textarea")?.focus();
  }

  /** Comments were added, moved or removed: redraw what shows them */
  function commentsChanged() {
    drawComments();
    drawMarkers();
    drawShapes();
    scheduleSave();
  }

  /** Open a comment: pause at its time and make its drawings editable */
  function openComment(id) {
    const comment = cf.review?.comments.find((c) => c.id === id);
    if (!comment) return;
    cf.active = id;
    seek(comment.t_s);
    drawComments();
  }

  function closeComment() {
    cf.active = null;
    sketch.select(-1);
    drawComments();
    drawShapes();
  }

  function deleteComment(id) {
    const comment = cf.review.comments.find((c) => c.id === id);
    if (!comment) return;
    if (
      (comment.text || comment.shapes.length) &&
      !confirm(t("cf.comment.deleteAsk"))
    )
      return;
    cf.review.comments = cf.review.comments.filter((c) => c.id !== id);
    if (cf.active === id) cf.active = null;
    commentsChanged();
  }

  const timeText = (c) =>
    c.t_end_s != null ? `${clock(c.t_s)} – ${clock(c.t_end_s)}` : clock(c.t_s);

  const drawingsText = (n) => (n ? t("cf.drawings", { n }) : "");

  /** The list of comments by time; the open one as an editor */
  function drawComments() {
    const list = $("cf-comments");
    if (!cf.review) return list.replaceChildren();
    const comments = [...cf.review.comments].sort((a, b) => a.t_s - b.t_s);
    $("cf-count").textContent = comments.length
      ? t("cf.comments.count", { n: comments.length })
      : t("cf.comments.none");
    // Keep the caret where it is when the open editor stays
    const editing = document.activeElement?.closest?.(".cf-comment.is-active");
    if (editing && editing.dataset.id === cf.active) {
      editing.querySelector(".cf-meta").textContent = drawingsText(
        activeComment().shapes.length,
      );
      editing.querySelector(".cf-time").textContent = timeText(activeComment());
      return;
    }
    list.replaceChildren(
      ...comments.map((comment) => {
        const li = document.createElement("li");
        li.className = "cf-comment";
        li.dataset.id = comment.id;
        const community = isCommunity(comment);
        const ai = comment.author !== "user" && !community;
        if (ai) li.classList.add("is-ai");
        if (community) li.classList.add("is-community");
        const active = comment.id === cf.active;
        li.classList.toggle("is-active", active);
        li.innerHTML = `
          <div class="cf-comment-head">
            <button type="button" class="cf-time num" data-act="open">${timeText(comment)}</button>
            <span class="cf-author">${escapeHtml(authorName(comment.author))}</span>
            <span class="cf-meta panel-note">${escapeHtml(drawingsText(comment.shapes.length))}${sourceLink(comment)}</span>
            <button type="button" class="cf-x" data-act="delete" title="${escapeHtml(t("cf.comment.delete"))}" aria-label="${escapeHtml(t("cf.comment.delete"))}">✕</button>
          </div>
          ${
            active
              ? `<textarea class="select cf-edit" rows="3" placeholder="${escapeHtml(t("cf.comment.placeholder"))}">${escapeHtml(comment.text)}</textarea>
                 <div class="cf-row">
                   <button type="button" class="mode-toggle" data-act="end" title="${escapeHtml(t("cf.comment.endHereTitle"))}">${escapeHtml(t("cf.comment.endHere"))}</button>
                   ${comment.t_end_s != null ? `<button type="button" class="mode-toggle" data-act="no-end">${escapeHtml(t("cf.comment.noEnd"))}</button>` : ""}
                   <button type="button" class="mode-toggle" data-act="start" title="${escapeHtml(t("cf.comment.moveHereTitle"))}">${escapeHtml(t("cf.comment.moveHere"))}</button>
                   <button type="button" class="btn cf-done" data-act="close">${escapeHtml(t("cf.done"))}</button>
                 </div>`
              : `<p class="cf-text">${comment.text ? escapeHtml(comment.text) : `<span class="panel-note">${escapeHtml(t("cf.noText"))}</span>`}</p>`
          }`;
        return li;
      }),
    );
    markLive(player.time());
  }

  /** A link to the Discord message an imported comment or note came from */
  function sourceLink(c) {
    if (!isCommunity(c)) return "";
    return ` <a class="cf-source" href="${escapeHtml(c.source.url)}" target="_blank" rel="noopener" title="${escapeHtml(t("cf.source.discord"))}">Discord ↗</a>`;
  }

  /** The moments of an imported note that wait for the video's HUD to be
   * read (wave timers), as chips */
  function unplacedChips(note) {
    if (!note.unplaced?.length) return "";
    const chips = note.unplaced.map((m) => {
      const what =
        m.timer_s != null
          ? t("cf.notes.unplacedTimer", { wave: m.wave ?? "?", s: m.timer_s })
          : t("cf.notes.unplacedWave", { wave: m.wave ?? "?" });
      return `<span class="cf-unplaced num" title="${escapeHtml(what)}">${escapeHtml(m.raw)}</span>`;
    });
    return `<div class="cf-unplaced-row"><span class="panel-note">${escapeHtml(t("cf.notes.unplaced"))}</span> ${chips.join(" ")}</div>`;
  }

  /** Mark the comments playback is passing */
  function markLive(now) {
    for (const li of $("cf-comments").children) {
      const comment = cf.review?.comments.find((c) => c.id === li.dataset.id);
      li.classList.toggle("is-live", !!comment && visibleAt(now, comment));
    }
  }

  $("cf-comments").addEventListener("click", (event) => {
    const li = event.target.closest(".cf-comment");
    if (!li) return;
    const id = li.dataset.id;
    const comment = cf.review.comments.find((c) => c.id === id);
    const act = event.target.closest("[data-act]")?.dataset.act;
    const now = Math.round(player.time() * 1000) / 1000;
    if (act === "delete") return deleteComment(id);
    if (act === "close") return closeComment();
    if (act === "end") {
      if (now <= comment.t_s) return alert(t("cf.comment.goPast"));
      comment.t_end_s = now;
    } else if (act === "no-end") delete comment.t_end_s;
    else if (act === "start") {
      comment.t_s = now;
      if (comment.t_end_s != null && comment.t_end_s <= now)
        delete comment.t_end_s;
    } else {
      if (event.target.closest("textarea")) return;
      if (id !== cf.active) return openComment(id);
      return;
    }
    commentsChanged();
  });
  $("cf-comments").addEventListener("input", (event) => {
    const comment = activeComment();
    if (!comment || !event.target.matches("textarea")) return;
    comment.text = event.target.value;
    scheduleSave();
  });

  /** The comments as marks on the player's scrubber and strip: ticks at
   * their times (the AI's in its color), bands over ranges */
  function drawMarkers() {
    if (!cf.review) return player.setMarks({});
    const at = (seconds) => Math.round(seconds * player.fps);
    const kind = (comment) =>
      comment.author === "user"
        ? "comment"
        : isCommunity(comment)
          ? "community"
          : "ai";
    const comments = cf.review.comments;
    player.setMarks({
      ticks: comments.map((comment) => ({
        n: at(comment.t_s),
        kind: kind(comment),
        id: comment.id,
        title: `${timeText(comment)} ${comment.text || drawingsText(comment.shapes.length)}`,
      })),
      ranges: comments
        .filter((comment) => comment.t_end_s != null)
        .map((comment) => ({
          a: at(comment.t_s),
          b: at(comment.t_end_s),
          kind: kind(comment),
        })),
    });
  }

  // --------------------------------------------------------------- notes

  /** Notes on the whole video, newest first; the one being edited as an
   * editor */
  function drawNotes() {
    const list = $("cf-notes");
    const notes = [...(cf.review?.notes ?? [])].sort(
      (a, b) => b.created_ms - a.created_ms,
    );
    $("cf-notes-count").textContent = notes.length
      ? t("cf.notes.count", { n: notes.length })
      : t("cf.notes.none");
    list.replaceChildren(
      ...notes.map((note) => {
        const li = document.createElement("li");
        li.className = "cf-note";
        li.dataset.id = note.id;
        const community = isCommunity(note);
        if (note.author !== "user" && !community) li.classList.add("is-ai");
        if (community) li.classList.add("is-community");
        const when = new Date(note.created_ms).toLocaleString(i18nLocale());
        const edited = note.edited_ms ? ` · ${t("cf.notes.edited")}` : "";
        const editing = note.id === cf.editingNote;
        li.innerHTML = `
          <div class="cf-comment-head">
            <span class="cf-author">${escapeHtml(authorName(note.author))}</span>
            <span class="cf-meta panel-note">${escapeHtml(when + edited)}${sourceLink(note)}</span>
            ${editing ? "" : `<button type="button" class="mode-toggle" data-act="edit">${escapeHtml(t("cf.notes.edit"))}</button>`}
            <button type="button" class="cf-x" data-act="delete" title="${escapeHtml(t("cf.notes.delete"))}" aria-label="${escapeHtml(t("cf.notes.delete"))}">✕</button>
          </div>
          ${
            editing
              ? `<textarea class="select cf-edit" rows="4">${escapeHtml(note.text)}</textarea>
                 <div class="cf-row">
                   <button type="button" class="mode-toggle" data-act="cancel">${escapeHtml(t("cf.cancel"))}</button>
                   <button type="button" class="btn cf-done" data-act="save">${escapeHtml(t("cf.notes.save"))}</button>
                 </div>`
              : `<p class="cf-text">${escapeHtml(note.text)}</p>${unplacedChips(note)}`
          }`;
        return li;
      }),
    );
  }

  $("cf-note-form").onsubmit = (event) => {
    event.preventDefault();
    const text = $("cf-note-text").value.trim();
    if (!cf.review || !text) return;
    cf.review.notes.push({
      id: newId("n"),
      author: "user",
      text,
      created_ms: Date.now(),
    });
    $("cf-note-text").value = "";
    drawNotes();
    scheduleSave();
  };

  $("cf-notes").addEventListener("click", (event) => {
    const act = event.target.closest("[data-act]")?.dataset.act;
    const li = event.target.closest(".cf-note");
    if (!act || !li || !cf.review) return;
    const note = cf.review.notes.find((n) => n.id === li.dataset.id);
    if (!note) return;
    if (act === "delete") {
      if (!confirm(t("cf.notes.deleteAsk"))) return;
      cf.review.notes = cf.review.notes.filter((n) => n !== note);
      if (cf.editingNote === note.id) cf.editingNote = null;
      scheduleSave();
    } else if (act === "edit") {
      cf.editingNote = note.id;
    } else if (act === "cancel") {
      cf.editingNote = null;
    } else if (act === "save") {
      const text = li.querySelector("textarea").value.trim();
      if (!text) return;
      if (text !== note.text) {
        note.text = text;
        note.edited_ms = Date.now();
        scheduleSave();
      }
      cf.editingNote = null;
    }
    drawNotes();
    if (act === "edit") $("cf-notes").querySelector("textarea")?.focus();
  });

  // --------------------------------------------------------------- saving

  function markSaved(state, error) {
    cf.saved = [state, error];
    const text = {
      new: t("cf.save.new"),
      dirty: t("cf.save.dirty"),
      saving: t("cf.save.saving"),
      saved: t("cf.save.saved", { id: cf.id }),
      error: t("cf.save.error", { error }),
    }[state];
    const level = { saved: "good", error: "critical", dirty: "warning" }[state];
    setChip(text, level ?? "off");
  }

  function scheduleSave() {
    markSaved("dirty");
    clearTimeout(cf.saveTimer);
    cf.saveTimer = setTimeout(save, 500);
  }

  function flushSave() {
    if (cf.saveTimer) {
      clearTimeout(cf.saveTimer);
      save();
    }
  }

  /** Name the open review if it has no folder yet, and put it in the address */
  function ensureId() {
    if (!cf.id) {
      cf.id = newReviewId();
      writeUrl();
    }
    return cf.id;
  }

  /** Write the review; the first save names it and puts it in the address */
  function save() {
    cf.saveTimer = null;
    const review = cf.review;
    if (!review) return;
    putReview(review, ensureId());
  }

  /** PUT a review, after the saves before it; the chip follows while the
   * review is the open one */
  function putReview(review, id) {
    const body = JSON.stringify(review);
    if (cf.review === review) markSaved("saving");
    cf.saving = cf.saving.then(async () => {
      try {
        const response = await fetch(
          `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
          {
            method: "PUT",
            headers: { "content-type": "application/json" },
            body,
          },
        );
        const data = await response.json();
        if (!response.ok) throw new Error(data.error);
        if (cf.review !== review) return;
        if (!cf.saveTimer) markSaved("saved");
        markCopy();
        takeMeta(data.video);
      } catch (error) {
        if (cf.review === review) markSaved("error", error.message);
      }
    });
  }

  // ------------------------------------------------------ ask Cuttlefish

  /** The chat's example messages: the placeholders and the chips */
  const examples = () => t("cf.chat.examples");

  /** Whether the lab has a model backend (the API key, or the Claude
   * Code CLI); asked once, shown in the composers' notes when it does not */
  async function checkKey() {
    if (chat.key == null) {
      try {
        const data = await (
          await fetch("/api/cuttlefish/knowledge/model")
        ).json();
        chat.key = Boolean(data.backend);
      } catch {
        return;
      }
    }
    markKey();
  }

  function markKey() {
    if (chat.key !== false || chat.sending) return;
    for (const id of ["cf-entry-note", "cf-chat-note"]) {
      $(id).textContent = t("cf.chat.noKey");
      $(id).classList.add("is-warning");
    }
  }

  /** Example messages as chips that fill `input`: `{text, ctx?, title?,
   * deep?}`, a chip with `ctx` also choosing what the message takes of the
   * video; a deep question's chip is marked */
  function drawChips(box, input, chips) {
    box.replaceChildren(
      ...chips.map(({ text, ctx, title, deep }) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "cf-chip";
        if (deep) button.classList.add("is-deep");
        if (title) button.title = title;
        button.textContent = text;
        button.onclick = () => {
          input.value = text;
          grow(input);
          if (ctx) {
            $("cf-ctx").value = ctx;
            remember("context", ctx);
            markContext();
          }
          input.focus();
        };
        return button;
      }),
    );
  }

  /** Deep questions offered as chips at once */
  const DEEP_CHIPS = 3;
  /** How many demo questions join the chips */
  const DEMO_CHIPS = 3;

  /** A few of the demo questions (web/demo-questions.js: questions the
   * imported game data answers with exact numbers, some needing the
   * #vod-review knowledge too), in the page's language, drawn anew each
   * time the chips are drawn */
  function demoChips() {
    const pool = window.DEMO_QUESTIONS?.[i18nLang()] ?? [];
    const picked = [...pool].sort(() => Math.random() - 0.5);
    return picked.slice(0, DEMO_CHIPS).map((text) => ({ text }));
  }

  /** The deep question bank (crates/cuttlefish/questions/deep.toml), once
   * fetched; the chips draw a few at random from it */
  let deepBank = null;

  async function loadDeep() {
    if (deepBank) return;
    try {
      const data = await (
        await fetch("/api/cuttlefish/knowledge/questions")
      ).json();
      if (!Array.isArray(data.questions)) return;
      deepBank = data.questions;
    } catch {
      return;
    }
    // Drawn without them until now
    if (cf.shown && !cf.review)
      drawChips($("cf-entry-chips"), $("cf-entry-text"), entryChips());
    if (cf.review && !cf.review.messages.length)
      drawChips($("cf-chat-chips"), $("cf-chat-text"), chatChips());
  }

  /** `n` random items of a list */
  function sample(list, n) {
    const pool = [...list];
    const out = [];
    while (pool.length && out.length < n) {
      out.push(pool.splice(Math.floor(Math.random() * pool.length), 1)[0]);
    }
    return out;
  }

  /** A question of the bank as a chip, in the page's language, its
   * category as the tooltip; a video question also picks the moment or
   * the range */
  function deepChip(q) {
    const ctx = { video_moment: "moment", hud: "moment", video_range: "range" }[
      q.needs
    ];
    return {
      text: i18nLang() === "zh" ? q.zh : q.en,
      ctx,
      title: t("cf.chat.deepTitle", {
        category: t(`k.deep.cat.${q.category}`),
      }),
      deep: true,
    };
  }

  /** A few deep questions at random: those about a video only with one
   * (a couple of them first), never the ones that wait for the detector */
  function deepChips(withVideo) {
    if (!deepBank) return [];
    const askable = deepBank.filter((q) => q.needs === "knowledge");
    if (!withVideo) return sample(askable, DEEP_CHIPS).map(deepChip);
    const video = deepBank.filter((q) =>
      ["video_moment", "video_range", "hud"].includes(q.needs),
    );
    return [...sample(video, 2), ...sample(askable, DEEP_CHIPS - 1)].map(
      deepChip,
    );
  }

  /** The library bar's chips: questions about one's play, then a few
   * deep ones and a few demo questions */
  const entryChips = () => [
    ...examples().map((text) => ({ text })),
    ...deepChips(false),
    ...demoChips(),
  ];

  /** A review's chips: with a video, the moment and the range first, then
   * the deep questions, the examples and the demo questions */
  function chatChips() {
    const withVideo = Boolean(cf.review?.video);
    const chips = examples().map((text) => ({ text }));
    if (!withVideo) return [...chips, ...deepChips(false), ...demoChips()];
    return [
      { text: t("cf.ask.moment"), ctx: "moment" },
      { text: t("cf.chat.rangeExample"), ctx: "range" },
      ...deepChips(true),
      ...chips,
      ...demoChips(),
    ];
  }

  /** The next example as the placeholder of the empty chat inputs */
  function rotatePlaceholders() {
    const list = examples();
    if (!list.length) return;
    chat.example = (chat.example + 1) % list.length;
    for (const id of ["cf-entry-text", "cf-chat-text"]) {
      $(id).placeholder = list[chat.example];
      // A long example wraps: the empty input grows for it too
      if (!$(id).value) grow($(id));
    }
  }

  function startPlaceholders() {
    clearInterval(chat.placeholderTimer);
    rotatePlaceholders();
    chat.placeholderTimer = setInterval(rotatePlaceholders, PLACEHOLDER_MS);
  }

  /** A chat input grows with its text, up to INPUT_ROWS lines */
  function grow(input) {
    input.style.height = "auto";
    const line = parseFloat(getComputedStyle(input).lineHeight) || 20;
    input.style.height = `${Math.min(input.scrollHeight, line * INPUT_ROWS + 14)}px`;
  }

  /** Enter sends, Shift+Enter breaks the line */
  function sendOnEnter(input, form) {
    input.addEventListener("keydown", (event) => {
      if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
        event.preventDefault();
        form.requestSubmit();
      }
    });
    input.addEventListener("input", () => grow(input));
  }

  /** A source of a message as a list item: its id, title and section; an
   * expert comment of #vod-review as its reviewer and date, the era and the
   * moment it is about; an expert note as its label ("Expert note (user),
   * date"), then the question it answers. The title opens the source
   * popover (source.js: the comment in its conversation, or the chunk
   * cited); the original is a small link after it. `list` and `i` say
   * where the source is in the message (`sources` or `experts`). */
  function sourceItem(s, i, list) {
    const link = (text) =>
      `<button type="button" class="src-link" data-src-list="${list}" data-src-index="${i}">${escapeHtml(text)}</button>${
        s.url
          ? ` <a class="cf-src-out" href="${escapeHtml(s.url)}" target="_blank" rel="noopener" title="${escapeHtml(t("src.original"))}">↗</a>`
          : ""
      }`;
    if (s.source === "expert-note") {
      return `<li><b>${escapeHtml(s.id)}</b> ${link(s.heading || t("k.source.expertNote"))} <span class="cf-kind">${escapeHtml(t("k.source.expertNote"))}</span> › ${escapeHtml(s.title)}</li>`;
    }
    const x = s.expert;
    if (x) {
      const about = s.heading.split(", about ")[1];
      return `<li><b>${escapeHtml(s.id)}</b> ${link(`${x.reviewer}, ${x.date}`)} <span class="panel-note">#vod-review · ${escapeHtml(x.game)}</span>${about ? ` › ${escapeHtml(about)}` : ""}</li>`;
    }
    return `<li><b>${escapeHtml(s.id)}</b> ${link(s.title)}${s.heading ? ` › ${escapeHtml(s.heading)}` : ""}${s.license ? ` <span class="panel-note">${escapeHtml(s.license)}</span>` : ""}</li>`;
  }

  /** A message as HTML: escaped, with [S1] citations linked to their
   * sources and, when a video is attached, times that seek to them */
  function messageHtml(message) {
    let html = escapeHtml(message.text);
    const sources = message.sources ?? [];
    html = html.replace(/\[(S\d+)\]/g, (match, id) => {
      const i = sources.findIndex((s) => s.id === id);
      if (i < 0) return match;
      const source = sources[i];
      const title = escapeHtml(
        source.heading ? `${source.title} › ${source.heading}` : source.title,
      );
      return `<button type="button" class="cf-cite" data-src-list="sources" data-src-index="${i}" title="${title}">${id}</button>`;
    });
    if (!cf.review?.video) return html;
    const seekButton = (label, seconds) =>
      `<button type="button" class="cf-tlink num" data-seek="${seconds}" title="${escapeHtml(t("cf.chat.seek", { time: clock(seconds) }))}">${label}</button>`;
    // 1:23 or 1:23.4, then 83.5 s (or 83.5秒); a bare 5.1 stays as it is
    html = html.replace(
      /(^|[^\d:.])(\d{1,3}):([0-5]\d)(\.\d+)?(?![\d:])/g,
      (match, before, m, s, frac) =>
        before +
        seekButton(
          `${m}:${s}${frac ?? ""}`,
          60 * m + parseFloat(`${s}${frac ?? ""}`),
        ),
    );
    html = html.replace(
      /(^|[^\d.:])(\d+(?:\.\d+)?) ?(s\b|秒)/g,
      (match, before, seconds, unit) =>
        before + seekButton(`${seconds} ${unit}`, parseFloat(seconds)),
    );
    return html;
  }

  /** The thread: every message, the open review's comments it added, and
   * the examples while it is empty */
  function drawChat() {
    const list = $("cf-chat");
    const review = cf.review;
    if (!review) return list.replaceChildren();
    const messages = review.messages;
    $("cf-chat-empty").hidden = messages.length > 0;
    if (!messages.length)
      drawChips($("cf-chat-chips"), $("cf-chat-text"), chatChips());
    const withVideo = Boolean(review.video);
    $("cf-context").hidden = !withVideo;
    markContext();
    list.replaceChildren(
      ...messages.map((message) => {
        const li = document.createElement("li");
        const user = message.role === "user";
        li.className = `cf-msg ${user ? "is-user" : "is-ai"}`;
        li.dataset.id = message.id;
        const when = new Date(message.created_ms).toLocaleTimeString(
          i18nLocale(),
          { hour: "2-digit", minute: "2-digit" },
        );
        let context = "";
        if (user && message.t_s != null && withVideo) {
          const label =
            message.t_end_s != null
              ? `${clock(message.t_s)} – ${clock(message.t_end_s)}`
              : t("cf.chat.at", { time: clock(message.t_s) });
          context = `<button type="button" class="cf-time num" data-seek="${message.t_s}">${escapeHtml(label)}</button>`;
        }
        const sources = (message.sources ?? [])
          .map((s, i) => sourceItem(s, i, "sources"))
          .join("");
        const experts = (message.experts ?? [])
          .map((s, i) => sourceItem(s, i, "experts"))
          .join("");
        const added = (message.comments ?? [])
          .map((id) => review.comments.find((c) => c.id === id))
          .filter(Boolean)
          .sort((a, b) => a.t_s - b.t_s);
        const comments =
          added.length && withVideo
            ? `<div class="cf-msg-comments"><span class="panel-note">${escapeHtml(t("cf.chat.commentsAdded", { n: added.length }))}</span> ${added.map((c) => `<button type="button" class="cf-time num" data-comment="${escapeHtml(c.id)}">${timeText(c)}</button>`).join(" ")}</div>`
            : "";
        // An answer can be corrected into an expert note
        const memo = user
          ? ""
          : `<button type="button" class="mode-toggle cf-mini cf-memo" data-memo title="${escapeHtml(t("cf.chat.memoTitle"))}">${escapeHtml(t("cf.chat.memo"))}</button>`;
        li.innerHTML = `
          <div class="cf-msg-head">
            <span class="cf-author">${escapeHtml(user ? t("cf.author.you") : t("cf.name"))}</span>
            ${context}
            <span class="cf-meta panel-note num">${escapeHtml(when)}</span>
          </div>
          <div class="cf-msg-text">${messageHtml(message)}</div>
          ${sources ? `<details class="cf-sources"><summary>${escapeHtml(t("cf.chat.sources"))} (${message.sources.length})</summary><ol class="k-sources">${sources}</ol></details>` : ""}
          ${experts ? `<details class="cf-sources"><summary>${escapeHtml(t("cf.chat.experts"))} (${message.experts.length})</summary><ol class="k-sources">${experts}</ol></details>` : ""}
          ${comments}
          ${memo ? `<div class="cf-msg-tools">${memo}</div>` : ""}`;
        return li;
      }),
    );
    list.scrollTop = list.scrollHeight;
  }

  /** "Correct / add to memory": the answer, with the question it answered,
   * in the expert note editor (knowledge.js); the note is indexed at once */
  function correctAnswer(message) {
    const messages = cf.review?.messages ?? [];
    const at = messages.indexOf(message);
    const question = messages
      .slice(0, at)
      .reverse()
      .find((m) => m.role === "user");
    window.cuttlefishNotes?.edit({
      question: question?.text ?? "",
      body: message.text,
      from: `chat ${cf.id ?? ""}`.trim(),
      onSaved: (note) => chatNote(t("cf.chat.memoSaved", { id: note.id })),
    });
  }

  $("cf-chat").addEventListener("click", (event) => {
    // A cited source opens in the popover (source.js)
    const cite = event.target.closest("[data-src-list]");
    if (cite) {
      const id = cite.closest(".cf-msg")?.dataset.id;
      const message = cf.review?.messages.find((m) => m.id === id);
      const source = message?.[cite.dataset.srcList]?.[cite.dataset.srcIndex];
      if (source) window.cuttlefishSource.open(cite, source);
      return;
    }
    const memo = event.target.closest("[data-memo]");
    if (memo) {
      const id = memo.closest(".cf-msg")?.dataset.id;
      const message = cf.review?.messages.find((m) => m.id === id);
      if (message) correctAnswer(message);
      return;
    }
    const seekTo = event.target.closest("[data-seek]");
    if (seekTo && cf.review?.video) {
      seek(parseFloat(seekTo.dataset.seek));
      return;
    }
    const comment = event.target.closest("[data-comment]");
    if (comment) openComment(comment.dataset.comment);
  });

  /** The range inputs show for a range; the video context is remembered */
  function markContext() {
    const ctx = $("cf-ctx").value;
    $("cf-chat-range").hidden = ctx !== "range";
    $("cf-chat-review").hidden = ctx === "none";
    markCost();
  }

  $("cf-ctx").value = remembered("context", "moment");
  $("cf-ctx").onchange = () => {
    remember("context", $("cf-ctx").value);
    markContext();
  };
  for (const [id, key] of [
    ["cf-chat-fps", "aiFps"],
    ["cf-chat-height", "aiHeight"],
  ]) {
    const select = $(id);
    select.value = remembered(key, select.value);
    // A stored value no option has falls back to the default
    if (!select.value) select.selectedIndex = 1;
    select.onchange = () => {
      remember(key, select.value);
      markCost();
    };
  }
  for (const id of ["cf-chat-from", "cf-chat-to"])
    $(id).addEventListener("input", markCost);

  /** A frame's size at `height`, never above the video's own */
  function aiSize(height) {
    const { width: w, height: h } = cf.meta ?? {};
    if (!w || !h) return [Math.round((height * 16) / 9 / 2) * 2, height];
    const out = Math.min(height, h);
    return [Math.max(2, Math.round((w * out) / h / 2) * 2), out];
  }

  /** Image tokens of `frames` frames at `height` */
  function aiTokens(frames, height) {
    const [w, h] = aiSize(height);
    return frames * Math.ceil((w * h) / 750);
  }

  function tokenText(tokens) {
    return tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : String(tokens);
  }

  /** The image tokens the next message would send, as text; empty
   * without a video or frames */
  function costText() {
    if (!cf.review?.video) return "";
    const ctx = $("cf-ctx").value;
    if (ctx === "none") return "";
    if (ctx !== "range")
      return t("cf.cost.one", {
        tokens: tokenText(aiTokens(AI.MOMENT_FRAMES, AI.MOMENT_HEIGHT)),
        frames: AI.MOMENT_FRAMES,
      });
    const range = rangeOf();
    if (!range) return "";
    const span = range.to - range.from;
    if (span > AI.MAX_RANGE_S)
      return t("cf.ask.longRange", { max: AI.MAX_RANGE_S });
    const height = parseInt($("cf-chat-height").value, 10);
    if (span > AI.TWO_PASS_S) {
      const scout = Math.min(AI.MAX_FRAMES, Math.ceil(span * AI.SCOUT_FPS));
      return t("cf.cost.two", {
        scout: tokenText(aiTokens(scout, AI.SCOUT_HEIGHT)),
        answer: tokenText(aiTokens(AI.KEY_FRAMES, height)),
      });
    }
    const fps = parseFloat($("cf-chat-fps").value);
    const frames = Math.max(1, Math.min(AI.MAX_FRAMES, Math.ceil(span * fps)));
    return t("cf.cost.one", {
      tokens: tokenText(aiTokens(frames, height)),
      frames,
    });
  }

  /** Show the estimate next to the "With the video" choice */
  function markCost() {
    const cost = $("cf-ctx-cost");
    const text = costText();
    if (cost.textContent !== text) cost.textContent = text;
    cost.hidden = !text;
  }

  function chatNote(text, error = false) {
    const note = $("cf-chat-note");
    note.textContent = text;
    note.classList.toggle("is-error", error);
    note.classList.toggle("is-warning", false);
  }

  /** The range the inputs name, else the open comment's, else the last
   * RANGE_S seconds: {from, to} */
  function rangeOf() {
    const now = Math.round(player.time() * 1000) / 1000;
    const active = activeComment();
    const from =
      parseTime($("cf-chat-from").value) ??
      active?.t_s ??
      Math.max(0, now - RANGE_S);
    const to = parseTime($("cf-chat-to").value) ?? active?.t_end_s ?? now;
    return { from, to };
  }

  /** The moment or range of the video the next message is about, from the
   * context choice: {t_s, t_end_s?}, or null without a video or frames */
  function videoContext() {
    if (!cf.review?.video) return null;
    const ctx = $("cf-ctx").value;
    const now = Math.round(player.time() * 1000) / 1000;
    if (ctx === "none") return null;
    if (ctx !== "range") return { t_s: now };
    const { from, to } = rangeOf();
    if (to <= from) throw new Error(t("cf.ask.badRange"));
    if (to - from > AI.MAX_RANGE_S)
      throw new Error(t("cf.ask.longRange", { max: AI.MAX_RANGE_S }));
    $("cf-chat-from").value = shortTime(from);
    $("cf-chat-to").value = shortTime(to);
    return { t_s: from, t_end_s: to };
  }

  /** Send a message in the open review: the user's turn is saved at once,
   * Cuttlefish's when it comes, even if the review was left meanwhile; his
   * timed comments join the review */
  async function sendChat(text) {
    const review = cf.review;
    text = text.trim();
    if (!review || !text || chat.sending) return;
    let context;
    try {
      context = videoContext();
    } catch (error) {
      return chatNote(error.message, true);
    }
    const id = ensureId();
    const message = {
      id: newId("m"),
      role: "user",
      text,
      ...context,
      created_ms: Date.now(),
    };
    review.messages.push(message);
    $("cf-chat-text").value = "";
    grow($("cf-chat-text"));
    drawChat();
    scheduleSave();
    chat.sending = true;
    $("cf-chat-send").disabled = true;
    chatNote(t(context ? "cf.ask.watching" : "cf.chat.thinking"));
    const body = {
      review: id,
      message: text,
      history: review.messages
        .slice(0, -1)
        .map((m) => ({ role: m.role, text: m.text })),
      video: review.video ?? null,
      ...context,
      ...(context?.t_end_s != null && {
        fps: parseFloat($("cf-chat-fps").value),
        height: parseInt($("cf-chat-height").value, 10),
      }),
    };
    let response;
    let data;
    try {
      response = await fetch("/api/cuttlefish/chat", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      data = await response.json();
    } catch (error) {
      data = { error: error.message, unreachable: true };
    }
    chat.sending = false;
    $("cf-chat-send").disabled = false;
    if (!response?.ok) {
      if (cf.review !== review) return;
      if (data.unreachable)
        return chatNote(t("cf.chat.failed", { error: data.error }), true);
      if (response.status === 501) {
        chat.key = false;
        return markKey();
      }
      return chatNote(t("cf.chat.error", { error: data.error }), true);
    }
    // His comments join the review like the user's
    const added = [];
    for (const c of data.comments ?? []) {
      const comment = {
        id: newId("c"),
        t_s: c.t_s ?? context?.t_s ?? 0,
        ...(c.t_end_s != null && { t_end_s: c.t_end_s }),
        author: "Cuttlefish",
        text: c.text ?? "",
        shapes: c.shapes ?? [],
        created_ms: Date.now(),
      };
      review.comments.push(comment);
      added.push(comment.id);
    }
    review.messages.push({
      id: newId("m"),
      role: "assistant",
      text: data.text ?? "",
      ...(data.sources?.length && { sources: data.sources }),
      ...(data.experts?.length && { experts: data.experts }),
      ...(added.length && { comments: added }),
      ...(data.model && {
        backend: data.backend,
        model: data.model,
        effort: data.effort,
      }),
      created_ms: Date.now(),
    });
    if (cf.review !== review) return putReview(review, id);
    chatNote("");
    drawChat();
    if (added.length) commentsChanged();
    else scheduleSave();
  }

  $("cf-chat-form").onsubmit = (event) => {
    event.preventDefault();
    sendChat($("cf-chat-text").value);
  };
  sendOnEnter($("cf-chat-text"), $("cf-chat-form"));
  $("cf-chat-review").onclick = () => {
    if ($("cf-ctx").value === "none") $("cf-ctx").value = "moment";
    sendChat(t("cf.ask.momentMessage"));
  };

  /** The library's chat bar: the message starts a new review without a
   * video and is sent from there */
  $("cf-entry-form").onsubmit = (event) => {
    event.preventDefault();
    const text = $("cf-entry-text").value.trim();
    if (!text) return;
    $("cf-entry-text").value = "";
    grow($("cf-entry-text"));
    leavePlayer();
    openReview({ comments: [], notes: [], messages: [] }, newReviewId(), 0);
    sendChat(text);
  };
  sendOnEnter($("cf-entry-text"), $("cf-entry-form"));

  // ------------------------------------------------------------- controls

  $("cf-add").onclick = () => addComment();
  $("cf-delete-shape").onclick = () => sketch.removeSelected();
  $("cf-back").onclick = () => {
    navigate("/cuttlefish");
  };
  for (const button of document.querySelectorAll(".cf-tools [data-tool]")) {
    button.onclick = () => setTool(button.dataset.tool);
  }
  function drawSwatches() {
    $("cf-swatches").replaceChildren(
      ...SWATCHES.map((color) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "cf-swatch";
        button.dataset.color = color;
        button.style.background = color;
        button.title = t("cf.swatch", { color });
        button.setAttribute("aria-label", t("cf.swatch", { color }));
        button.onclick = () => setColor(color);
        return button;
      }),
    );
  }
  drawSwatches();
  sketch.setTool(remembered("tool", "arrow"));
  sketch.color = remembered("color", SWATCHES[0]);
  markTools();
  markDanmaku();

  // Keys act only while a review with a video is open; the player has the
  // transport's (Space, arrows, Home, End, G)
  document.addEventListener("keydown", (event) => {
    if (!cf.shown || !cf.review?.video) return;
    const tag = event.target.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") {
      if (event.key === "Escape") event.target.blur();
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key.toLowerCase();
    if (key === "c") addComment();
    else if (key === "d") $("cf-danmaku-toggle").click();
    else if (TOOLS[key]) setTool(TOOLS[key]);
    else if (key === "delete" || key === "backspace") sketch.removeSelected();
    else if (key === "escape") {
      if (sketch.selected >= 0) sketch.select(-1);
      else closeComment();
    } else return;
    event.preventDefault();
  });

  // Everything drawn from JavaScript follows the language
  window.addEventListener("lang-change", () => {
    drawReviews();
    drawSwatches();
    markTools();
    drawChips($("cf-entry-chips"), $("cf-entry-text"), entryChips());
    rotatePlaceholders();
    markKey();
    if (!cf.review) return;
    markSaved(...cf.saved);
    drawInfo();
    drawComments();
    drawNotes();
    drawChat();
    drawMarkers();
  });

  // ---------------------------------------------------------------- routing

  /** The views the tab strip switches, besides the library */
  const VIEWS = ["translate", "knowledge", "pedia"];

  /** Show what the address names: a review, a video, the translate or
   * knowledge view, or the library */
  async function route(state) {
    const view = state.get("view");
    if (VIEWS.includes(view)) {
      // translate.js, knowledge.js and pedia.js show their own views; the
      // Pedia is remembered at its entry
      leavePlayer();
      $("cf-player").hidden = true;
      $("cf-library").hidden = true;
      setChip("");
      rememberView(
        view === "pedia" ? location.pathname : `/cuttlefish/${view}`,
      );
      clearTimeout(cf.pollTimer);
      clearTimeout(cf.listTimer);
      return;
    }
    const at = parseFloat(state.get("t")) || 0;
    const id = state.get("r");
    if (id) {
      if (cf.review && cf.id === id) return;
      leavePlayer();
      const response = await fetch(
        `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
      );
      const review = await response.json();
      if (!response.ok) {
        openError(t("cf.open.cannot", { id, error: review.error }));
        return showLibrary();
      }
      return openReview(review, id, at);
    }
    if (state.get("kind")) {
      const v = videoOf(state);
      if (cf.review?.video && !cf.id && sameVideo(cf.review.video, v)) return;
      leavePlayer();
      return openReview(
        { video: v, comments: [], notes: [], messages: [] },
        null,
        at,
      );
    }
    showLibrary();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    if (cf.shown && app !== "cuttlefish") {
      player.pause();
      flushSave();
      clearTimeout(cf.pollTimer);
      clearTimeout(cf.listTimer);
      clearTimeout(cf.metaTimer);
      clearInterval(chat.placeholderTimer);
    }
    cf.shown = app === "cuttlefish";
    player.enabled = cf.shown;
    markTabs(state);
    if (cf.shown) {
      startPlaceholders();
      route(state);
    }
  });

  /** The tab strip: above the views, not in the player, the current view
   * marked */
  function markTabs(state) {
    const inPlayer = state.has("r") || state.has("kind");
    $("cf-tabs").hidden = !cf.shown || inPlayer;
    const view = state.get("view");
    const current = VIEWS.includes(view) ? view : "reviews";
    for (const tab of $("cf-tabs").querySelectorAll("[data-tab]")) {
      if (tab.dataset.tab === current) tab.setAttribute("aria-current", "page");
      else tab.removeAttribute("aria-current");
    }
  }
  // Unsaved text is written before the page goes away
  window.addEventListener("pagehide", flushSave);

  document.querySelector('.app-nav [data-app="cuttlefish"]').href = storedView(
    "cuttlefish",
    remembered("view"),
  );
})();
