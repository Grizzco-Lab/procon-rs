// Cuttlefish app: review a video with comments at its times and drawings on
// its paused frames, and ask Cuttlefish (the AI) for his. Runs after app.js,
// sketch.js and inspect.js and uses their helpers ($, Sketch, clock,
// escapeHtml). Its state lives in the hash: #cuttlefish (the library),
// #cuttlefish/r=<review>&t=<s> (a saved review) or
// #cuttlefish/kind=<kind>&ref=<ref>&start_s=&end_s= (a video not reviewed yet)
// or #cuttlefish/view=knowledge (the knowledge view, see knowledge.js).
// Reviews are saved as JSON through /api/cuttlefish/reviews/<id>; the format
// is in src/cuttlefish.rs.
"use strict";

(() => {
  /** A comment without an end shows its drawings this long, in seconds */
  const HOLD_S = 2;
  /** Range reviewed by default: this long up to the current time */
  const RANGE_S = 15;
  /** Frame rate assumed until the video's is known */
  const DEFAULT_FPS = 30;
  /** Drawing colors */
  const SWATCHES = ["#ff5c8a", "#ffd23f", "#4fb3ff", "#8bd450", "#ffffff"];
  const TOOLS = {
    v: "select",
    r: "rect",
    e: "ellipse",
    a: "arrow",
    f: "freehand",
  };

  const video = $("cf-video");
  const screen = $("cf-screen");

  const cf = {
    /** Whether the Cuttlefish app is shown */
    shown: false,
    /** Session summaries from the Inkspector's API */
    sessions: null,
    /** The open review: {video, comments}, or null in the library */
    review: null,
    /** Its file name without .json once saved */
    id: null,
    fps: DEFAULT_FPS,
    /** Id of the comment being edited, whose drawings can change */
    active: null,
    saveTimer: null,
    /** Saves run one after another */
    saving: Promise.resolve(),
    /** Download to open once it is done */
    waiting: null,
    pollTimer: null,
  };

  const sketch = new Sketch(screen, {
    onChange: shapesChanged,
    onSelect: markTools,
    onDraw: startDrawing,
  });

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

  /** The query naming a video, for the video and meta endpoints and the hash */
  function videoQuery(v) {
    const query = new URLSearchParams({ kind: v.kind, ref: v.ref });
    if (v.start_s != null) query.set("start_s", v.start_s);
    if (v.end_s != null) query.set("end_s", v.end_s);
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

  /** What a video is called in lists */
  function videoName(v) {
    if (v.kind === "file") return v.ref.split("/").pop();
    if (v.kind === "youtube") {
      const range =
        v.start_s != null || v.end_s != null
          ? ` · ${shortTime(v.start_s ?? 0)}–${v.end_s != null ? shortTime(v.end_s) : "end"}`
          : "";
      return `${v.ref.replace(/^https?:\/\/(www\.)?/, "")}${range}`;
    }
    return v.ref;
  }

  const KINDS = { session: "Session", file: "File", youtube: "YouTube" };

  /** Author as shown */
  const authorName = (author) => (author === "user" ? "You" : author);

  /** A new id: a comment's, or a review's file name from the local time */
  const newCommentId = () =>
    `c${Date.now().toString(36)}${Math.random().toString(36).slice(2, 6)}`;

  function newReviewId() {
    const d = new Date();
    const pad = (n) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}_${pad(d.getHours())}-${pad(d.getMinutes())}-${pad(d.getSeconds())}`;
  }

  /** The last view, for the app link after leaving or a reload */
  function rememberView(hash) {
    document.querySelector('.app-nav [data-app="cuttlefish"]').href = hash;
    remember("view", hash);
  }

  function setChip(text, level = "off") {
    const chip = $("cf-chip");
    chip.hidden = !text;
    chip.dataset.level = level;
    chip.querySelector(".chip-text").textContent = text;
  }

  // ------------------------------------------------------------- library

  async function showLibrary() {
    leavePlayer();
    $("cf-player").hidden = true;
    $("cf-library").hidden = false;
    setChip("");
    rememberView("#cuttlefish");
    loadSessions();
    loadReviews();
    pollDownloads();
  }

  async function loadSessions() {
    if (cf.sessions) return;
    const select = $("cf-session");
    try {
      const response = await fetch("/api/inspect/sessions");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      cf.sessions = data.sessions;
    } catch (error) {
      cf.sessions = [];
      select.replaceChildren(new Option(`No sessions: ${error.message}`, ""));
      return;
    }
    select.replaceChildren(
      ...cf.sessions.map((s) => new Option(s.name, s.name)),
    );
    if (!cf.sessions.length) select.add(new Option("No sessions", ""));
    fillSegments();
  }

  function fillSegments() {
    const summary = cf.sessions?.find((s) => s.name === $("cf-session").value);
    const select = $("cf-segment");
    select.replaceChildren(
      ...(summary?.segments ?? []).map(
        (s) => new Option(s.file + (s.sound ? " ♪" : ""), s.file),
      ),
    );
    select.hidden = (summary?.segments.length ?? 0) < 2;
  }

  async function loadReviews() {
    const note = $("cf-reviews-note");
    const body = $("cf-reviews");
    let data;
    try {
      const response = await fetch("/api/cuttlefish/reviews");
      data = await response.json();
      if (!response.ok) throw new Error(data.error);
    } catch (error) {
      note.textContent = error.message;
      return;
    }
    note.textContent = `${data.reviews.length} in ${data.dir}`;
    body.replaceChildren();
    if (!data.reviews.length) {
      body.innerHTML = `<tr><td colspan="4" class="panel-note">No reviews yet: open a video and comment on it.</td></tr>`;
    }
    for (const review of data.reviews) {
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td><span class="cf-kind">${KINDS[review.video.kind] ?? review.video.kind}</span> ${escapeHtml(videoName(review.video))}<br><span class="panel-note">${escapeHtml(review.id)}</span></td>
        <td class="num">${review.comments}</td>
        <td>${escapeHtml(new Date(review.modified_ms).toLocaleString())}</td>
        <td><button type="button" class="mode-toggle" data-delete>Delete</button></td>`;
      tr.onclick = (event) => {
        if (event.target.closest("[data-delete]")) {
          deleteReview(review.id);
          return;
        }
        location.hash = `#cuttlefish/${new URLSearchParams({ r: review.id })}`;
      };
      body.append(tr);
    }
  }

  async function deleteReview(id) {
    if (!confirm(`Delete the review ${id}? Its file is removed.`)) return;
    const response = await fetch(
      `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
      { method: "DELETE" },
    );
    if (!response.ok) alert((await response.json()).error);
    loadReviews();
  }

  /** Open a video not reviewed yet */
  function openVideoHash(v) {
    location.hash = `#cuttlefish/${videoQuery(v)}`;
  }

  function openError(message) {
    const note = $("cf-open-error");
    note.hidden = !message;
    note.textContent = message ?? "";
  }

  $("cf-session").onchange = fillSegments;
  $("cf-form-session").onsubmit = (event) => {
    event.preventDefault();
    const session = $("cf-session").value;
    if (!session) return;
    const segment = $("cf-segment").value;
    openVideoHash({
      kind: "session",
      ref: segment ? `${session}/${segment}` : session,
    });
  };
  $("cf-form-file").onsubmit = (event) => {
    event.preventDefault();
    openVideoHash({ kind: "file", ref: $("cf-file").value.trim() });
  };
  $("cf-form-youtube").onsubmit = async (event) => {
    event.preventDefault();
    openError(null);
    const body = { url: $("cf-url").value.trim() };
    for (const [key, id] of [
      ["start_s", "cf-from"],
      ["end_s", "cf-to"],
    ]) {
      const text = $(id).value;
      const seconds = parseTime(text);
      if (text.trim() && seconds == null)
        return openError(
          `Cannot read the time "${text}"; write 90, 1:30 or 1:02.5`,
        );
      if (seconds != null) body[key] = seconds;
    }
    const response = await fetch("/api/cuttlefish/download", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const download = await response.json();
    if (!response.ok) return openError(download.error);
    const v = { kind: "youtube", ref: download.url };
    if (download.start_s != null) v.start_s = download.start_s;
    if (download.end_s != null) v.end_s = download.end_s;
    if (download.state === "done") return openVideoHash(v);
    cf.waiting = download.id;
    pollDownloads();
  };

  /** Show the downloads, and keep asking while one runs */
  async function pollDownloads() {
    clearTimeout(cf.pollTimer);
    let data;
    try {
      data = await (await fetch("/api/cuttlefish/downloads")).json();
    } catch {
      return;
    }
    const list = $("cf-downloads");
    list.replaceChildren(
      ...data.downloads.map((d) => {
        const li = document.createElement("li");
        li.className = "cf-download meter";
        li.dataset.state = d.state;
        const range =
          d.start_s != null || d.end_s != null
            ? ` · ${shortTime(d.start_s ?? 0)}–${d.end_s != null ? shortTime(d.end_s) : "end"}`
            : "";
        const percent = d.percent ?? 0;
        li.innerHTML = `
          <div class="cf-download-head"><span class="cf-download-url">${escapeHtml(d.url)}${range}</span><span class="num">${d.state === "running" ? `${percent.toFixed(0)}%` : d.state}</span></div>
          <div class="meter-track"><div class="meter-fill" style="width:${d.state === "done" ? 100 : percent}%"></div></div>
          <span class="panel-note ${d.state === "failed" ? "level-critical" : ""}">${escapeHtml(d.message)}</span>`;
        if (d.state === "done") {
          li.onclick = () =>
            openVideoHash({
              kind: "youtube",
              ref: d.url,
              ...(d.start_s != null && { start_s: d.start_s }),
              ...(d.end_s != null && { end_s: d.end_s }),
            });
        }
        return li;
      }),
    );
    const waiting = data.downloads.find((d) => d.id === cf.waiting);
    if (waiting?.state === "failed") {
      cf.waiting = null;
      openError(waiting.message);
    } else if (waiting?.state === "done") {
      cf.waiting = null;
      return openVideoHash({
        kind: "youtube",
        ref: waiting.url,
        ...(waiting.start_s != null && { start_s: waiting.start_s }),
        ...(waiting.end_s != null && { end_s: waiting.end_s }),
      });
    }
    if (cf.shown && data.downloads.some((d) => d.state === "running")) {
      cf.pollTimer = setTimeout(pollDownloads, 1000);
    }
  }

  // -------------------------------------------------------------- player

  /** Show a review (or a new one of `v`) at time `t` */
  async function openReview(review, id, t) {
    clearTimeout(cf.pollTimer);
    cf.review = review;
    cf.id = id;
    cf.active = null;
    $("cf-library").hidden = true;
    $("cf-player").hidden = false;
    $("cf-ai-note").hidden = true;
    const note = $("cf-video-note");
    note.hidden = true;
    screen.style.aspectRatio = "";
    cf.fps = DEFAULT_FPS;
    video.src = `/api/cuttlefish/video?${videoQuery(review.video)}`;
    video.playbackRate = parseFloat($("cf-speed").value);
    markSaved(id ? "saved" : "new");
    // A video opens paused, ready to draw on
    sketch.setEditable(true);
    drawComments();
    drawMarkers();
    writeHash(t);
    seek(t);
    try {
      const response = await fetch(
        `/api/cuttlefish/meta?${videoQuery(review.video)}`,
      );
      const meta = await response.json();
      if (!response.ok) throw new Error(meta.error);
      if (cf.review !== review) return;
      cf.fps = meta.fps || DEFAULT_FPS;
      // The layer covers the picture exactly, so its fractions are the frame's
      if (meta.width && meta.height)
        screen.style.aspectRatio = `${meta.width} / ${meta.height}`;
    } catch (error) {
      note.hidden = false;
      note.textContent = error.message;
    }
  }

  /** Stop the video when leaving it */
  function leavePlayer() {
    flushSave();
    video.pause();
    if (cf.review) {
      video.removeAttribute("src");
      video.load();
    }
    cf.review = null;
    cf.id = null;
    cf.active = null;
    sketch.set([]);
  }

  function writeHash(t = video.currentTime) {
    if (!cf.review) return;
    const params = cf.id
      ? new URLSearchParams({ r: cf.id })
      : videoQuery(cf.review.video);
    if (t > 0) params.set("t", t.toFixed(3));
    const hash = `#cuttlefish/${params}`;
    history.replaceState(null, "", hash);
    rememberView(hash);
  }

  function seek(t) {
    const duration = video.duration || Infinity;
    video.currentTime = Math.max(0, Math.min(duration, t));
    drawTime();
  }

  const playing = () => !video.paused && !video.ended;

  function toggle() {
    if (playing()) video.pause();
    else {
      cf.active = null;
      drawComments();
      video.play().catch(() => {});
    }
  }

  function step(seconds) {
    video.pause();
    seek(video.currentTime + seconds);
  }

  /** Time, scrubber and drawings of the current time */
  function drawTime() {
    const t = video.currentTime;
    const duration = video.duration || 0;
    $("cf-time").textContent = `${clock(t)} / ${clock(duration)}`;
    const percent = duration ? (100 * t) / duration : 0;
    $("cf-fill").style.width = `${percent}%`;
    $("cf-thumb").style.left = `${percent}%`;
    drawShapes();
    markLive(t);
  }

  // While playing, follow every painted frame
  function follow() {
    if (!playing()) return;
    drawTime();
    requestAnimationFrame(follow);
  }

  video.addEventListener("play", () => {
    $("cf-play").textContent = "❚❚ Pause";
    sketch.setEditable(false);
    requestAnimationFrame(follow);
  });
  video.addEventListener("pause", () => {
    $("cf-play").textContent = "▶ Play";
    sketch.setEditable(true);
    drawTime();
    writeHash();
  });
  video.addEventListener("seeked", () => {
    drawTime();
    if (!playing()) writeHash();
  });
  video.addEventListener("loadedmetadata", () => {
    drawTime();
    drawMarkers();
  });
  video.addEventListener("error", () => {
    if (!cf.review) return;
    const note = $("cf-video-note");
    note.hidden = false;
    note.textContent ||= "This browser cannot play the video";
  });

  // ----------------------------------------------------------- drawings

  const activeComment = () =>
    cf.review?.comments.find((c) => c.id === cf.active) ?? null;

  /** Comments whose drawings show at time t */
  function visibleAt(t, comment) {
    const end = comment.t_end_s ?? comment.t_s + HOLD_S;
    return t >= comment.t_s - 1e-3 && t <= end;
  }

  /** The drawings shown now: the open comment's (editable while paused) and
   * those of the comments playback is passing */
  function drawShapes() {
    if (!cf.review || sketch.drag) return;
    const t = video.currentTime;
    const active = activeComment();
    const shapes = [];
    for (const comment of cf.review.comments) {
      if (comment === active || !visibleAt(t, comment)) continue;
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

  // ------------------------------------------------------------ comments

  /** A comment by the user at the current time, opened for editing */
  function addComment(focus = true) {
    if (!cf.review) return;
    video.pause();
    const comment = {
      id: newCommentId(),
      t_s: Math.round(video.currentTime * 1000) / 1000,
      author: "user",
      text: "",
      shapes: [],
      created_ms: Date.now(),
    };
    cf.review.comments.push(comment);
    cf.active = comment.id;
    drawComments();
    drawMarkers();
    drawShapes();
    scheduleSave();
    if (focus) $("cf-comments").querySelector("textarea")?.focus();
  }

  /** Open a comment: pause at its time and make its drawings editable */
  function openComment(id) {
    const comment = cf.review.comments.find((c) => c.id === id);
    if (!comment) return;
    cf.active = id;
    video.pause();
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
      !confirm("Delete this comment and its drawings?")
    )
      return;
    cf.review.comments = cf.review.comments.filter((c) => c.id !== id);
    if (cf.active === id) cf.active = null;
    drawComments();
    drawMarkers();
    drawShapes();
    scheduleSave();
  }

  const timeText = (c) =>
    c.t_end_s != null ? `${clock(c.t_s)} – ${clock(c.t_end_s)}` : clock(c.t_s);

  /** The list of comments by time; the open one as an editor */
  function drawComments() {
    const list = $("cf-comments");
    if (!cf.review) return list.replaceChildren();
    const comments = [...cf.review.comments].sort((a, b) => a.t_s - b.t_s);
    $("cf-count").textContent = comments.length
      ? `${comments.length} · click one to go to its time`
      : "none yet";
    // Keep the caret where it is when the open editor stays
    const editing = document.activeElement?.closest?.(".cf-comment.is-active");
    if (editing && editing.dataset.id === cf.active) {
      editing.querySelector(".cf-meta").textContent = metaText(activeComment());
      editing.querySelector(".cf-time").textContent = timeText(activeComment());
      return;
    }
    list.replaceChildren(
      ...comments.map((comment) => {
        const li = document.createElement("li");
        li.className = "cf-comment";
        li.dataset.id = comment.id;
        const ai = comment.author !== "user";
        if (ai) li.classList.add("is-ai");
        const active = comment.id === cf.active;
        li.classList.toggle("is-active", active);
        li.innerHTML = `
          <div class="cf-comment-head">
            <button type="button" class="cf-time num" data-act="open">${timeText(comment)}</button>
            <span class="cf-author">${escapeHtml(authorName(comment.author))}</span>
            <span class="cf-meta panel-note">${metaText(comment)}</span>
            <button type="button" class="cf-x" data-act="delete" title="Delete comment" aria-label="Delete comment">✕</button>
          </div>
          ${
            active
              ? `<textarea class="select cf-edit" rows="3" placeholder="What happens here? Draw on the frame to point at it.">${escapeHtml(comment.text)}</textarea>
                 <div class="cf-row">
                   <button type="button" class="mode-toggle" data-act="end" title="End the comment's range at the current time">Set end here</button>
                   ${comment.t_end_s != null ? `<button type="button" class="mode-toggle" data-act="no-end">No end</button>` : ""}
                   <button type="button" class="mode-toggle" data-act="start" title="Move the comment to the current time">Move here</button>
                   <button type="button" class="btn cf-done" data-act="close">Done</button>
                 </div>`
              : `<p class="cf-text">${comment.text ? escapeHtml(comment.text) : '<span class="panel-note">No text</span>'}</p>`
          }`;
        return li;
      }),
    );
    markLive(video.currentTime);
  }

  function metaText(comment) {
    const n = comment.shapes.length;
    return n ? `${n} drawing${n > 1 ? "s" : ""}` : "";
  }

  /** Mark the comments playback is passing */
  function markLive(t) {
    for (const li of $("cf-comments").children) {
      const comment = cf.review?.comments.find((c) => c.id === li.dataset.id);
      li.classList.toggle("is-live", !!comment && visibleAt(t, comment));
    }
  }

  $("cf-comments").addEventListener("click", (event) => {
    const li = event.target.closest(".cf-comment");
    if (!li) return;
    const id = li.dataset.id;
    const comment = cf.review.comments.find((c) => c.id === id);
    const act = event.target.closest("[data-act]")?.dataset.act;
    const t = Math.round(video.currentTime * 1000) / 1000;
    if (act === "delete") return deleteComment(id);
    if (act === "close") return closeComment();
    if (act === "end") {
      if (t <= comment.t_s) return alert("Go past the comment's time first");
      comment.t_end_s = t;
    } else if (act === "no-end") delete comment.t_end_s;
    else if (act === "start") {
      comment.t_s = t;
      if (comment.t_end_s != null && comment.t_end_s <= t)
        delete comment.t_end_s;
    } else {
      if (event.target.closest("textarea")) return;
      if (id !== cf.active) return openComment(id);
      return;
    }
    drawComments();
    drawMarkers();
    scheduleSave();
  });
  $("cf-comments").addEventListener("input", (event) => {
    const comment = activeComment();
    if (!comment || !event.target.matches("textarea")) return;
    comment.text = event.target.value;
    scheduleSave();
  });

  /** Comment dots and ranges on the timeline */
  function drawMarkers() {
    const box = $("cf-markers");
    const duration = video.duration;
    if (!cf.review || !duration) return box.replaceChildren();
    box.replaceChildren(
      ...cf.review.comments.map((comment) => {
        const marker = document.createElement("button");
        marker.type = "button";
        marker.className = "cf-marker";
        if (comment.author !== "user") marker.classList.add("is-ai");
        const left = (100 * comment.t_s) / duration;
        marker.style.left = `${left}%`;
        if (comment.t_end_s != null) {
          marker.classList.add("is-range");
          marker.style.width = `${(100 * (comment.t_end_s - comment.t_s)) / duration}%`;
        }
        marker.title = `${timeText(comment)} ${comment.text}`;
        marker.addEventListener("pointerdown", (event) =>
          event.stopPropagation(),
        );
        marker.onclick = () => openComment(comment.id);
        return marker;
      }),
    );
  }

  // --------------------------------------------------------------- saving

  function markSaved(state, error) {
    const name = cf.review ? videoName(cf.review.video) : "";
    const text = {
      new: "not saved yet: comment to start the review",
      dirty: "unsaved changes",
      saving: "saving…",
      saved: `saved as ${cf.id}`,
      error: `not saved: ${error}`,
    }[state];
    const level = { saved: "good", error: "critical", dirty: "warning" }[state];
    setChip(`${name} · ${text}`, level ?? "off");
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

  /** Write the review; the first save names it and puts it in the hash */
  function save() {
    cf.saveTimer = null;
    const review = cf.review;
    if (!review) return;
    if (!cf.id) {
      cf.id = newReviewId();
      writeHash();
    }
    const id = cf.id;
    const body = JSON.stringify(review);
    markSaved("saving");
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
        if (!response.ok) throw new Error((await response.json()).error);
        if (cf.review === review && !cf.saveTimer) markSaved("saved");
      } catch (error) {
        if (cf.review === review) markSaved("error", error.message);
      }
    });
  }

  // ------------------------------------------------------ ask Cuttlefish

  $("cf-ask").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!cf.review) return;
    const range = event.submitter?.value === "range";
    const t = video.currentTime;
    const active = activeComment();
    let from = t;
    let to = null;
    if (range) {
      from =
        parseTime($("cf-ask-from").value) ??
        active?.t_s ??
        Math.max(0, t - RANGE_S);
      to = parseTime($("cf-ask-to").value) ?? active?.t_end_s ?? t;
      if (to <= from) return aiNote("The range must end after it starts", true);
      $("cf-ask-from").value = shortTime(from);
      $("cf-ask-to").value = shortTime(to);
    }
    const body = {
      video: cf.review.video,
      t_s: from,
      ...(to != null && { t_end_s: to }),
      question: $("cf-question").value.trim(),
      review: cf.id,
    };
    aiNote("Cuttlefish is watching…");
    let response;
    let data;
    try {
      response = await fetch("/api/cuttlefish/ai", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      data = await response.json();
    } catch (error) {
      return aiNote(`Could not ask Cuttlefish: ${error.message}`, true);
    }
    if (!response.ok) {
      const pending = response.status === 501;
      return aiNote(
        pending
          ? `Cuttlefish can't answer yet: ${data.error}. Your own comments and drawings are saved as usual.`
          : `Cuttlefish could not answer: ${data.error}`,
        !pending,
      );
    }
    // His comments join the review like the user's
    const comments = data.comments ?? (data.text ? [data] : []);
    for (const comment of comments) {
      cf.review.comments.push({
        id: newCommentId(),
        t_s: comment.t_s ?? from,
        ...((comment.t_end_s ?? to) != null && {
          t_end_s: comment.t_end_s ?? to,
        }),
        author: "Cuttlefish",
        text: comment.text ?? "",
        shapes: comment.shapes ?? [],
        created_ms: Date.now(),
      });
    }
    aiNote(
      comments.length
        ? `Cuttlefish added ${comments.length} comment(s)`
        : "Cuttlefish had nothing to add",
    );
    drawComments();
    drawMarkers();
    drawShapes();
    if (comments.length) scheduleSave();
  });

  function aiNote(text, error = false) {
    const note = $("cf-ai-note");
    note.hidden = false;
    note.textContent = text;
    note.classList.toggle("is-info", !error);
  }

  // ------------------------------------------------------------- controls

  $("cf-play").onclick = toggle;
  $("cf-back-frame").onclick = () => step(-1 / cf.fps);
  $("cf-next-frame").onclick = () => step(1 / cf.fps);
  $("cf-speed").onchange = () => {
    video.playbackRate = parseFloat($("cf-speed").value);
  };
  $("cf-add").onclick = () => addComment();
  $("cf-delete-shape").onclick = () => sketch.removeSelected();
  $("cf-back").onclick = () => {
    location.hash = "#cuttlefish";
  };
  for (const button of document.querySelectorAll(".cf-tools [data-tool]")) {
    button.onclick = () => setTool(button.dataset.tool);
  }
  $("cf-swatches").replaceChildren(
    ...SWATCHES.map((color) => {
      const button = document.createElement("button");
      button.type = "button";
      button.className = "cf-swatch";
      button.dataset.color = color;
      button.style.background = color;
      button.title = `Draw in ${color}`;
      button.setAttribute("aria-label", `Color ${color}`);
      button.onclick = () => setColor(color);
      return button;
    }),
  );
  sketch.setTool(remembered("tool", "arrow"));
  sketch.color = remembered("color", SWATCHES[0]);
  markTools();

  // Scrubbing the timeline pauses and seeks
  (() => {
    const scrubber = $("cf-scrubber");
    const bubble = $("cf-bubble");
    let dragging = false;
    const timeAt = (event) => {
      const box = scrubber.getBoundingClientRect();
      const x = Math.max(
        0,
        Math.min(1, (event.clientX - box.left) / box.width),
      );
      return x * (video.duration || 0);
    };
    const preview = (event) => {
      const t = timeAt(event);
      bubble.hidden = false;
      bubble.style.left = `${(100 * t) / (video.duration || 1)}%`;
      bubble.textContent = clock(t);
      seek(t);
    };
    scrubber.addEventListener("pointerdown", (event) => {
      if (!cf.review || !video.duration) return;
      dragging = true;
      scrubber.setPointerCapture(event.pointerId);
      video.pause();
      preview(event);
    });
    scrubber.addEventListener("pointermove", (event) => {
      if (dragging) preview(event);
    });
    scrubber.addEventListener("pointerup", () => {
      dragging = false;
      bubble.hidden = true;
    });
  })();

  // Keys act only while a review is open
  document.addEventListener("keydown", (event) => {
    if (!cf.shown || !cf.review) return;
    const tag = event.target.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") {
      if (event.key === "Escape") event.target.blur();
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const key = event.key.toLowerCase();
    if (event.key === " ") toggle();
    else if (event.key === "ArrowLeft") step(event.shiftKey ? -1 : -1 / cf.fps);
    else if (event.key === "ArrowRight") step(event.shiftKey ? 1 : 1 / cf.fps);
    else if (key === "c") addComment();
    else if (TOOLS[key]) setTool(TOOLS[key]);
    else if (key === "delete" || key === "backspace") sketch.removeSelected();
    else if (key === "escape") {
      if (sketch.selected >= 0) sketch.select(-1);
      else closeComment();
    } else return;
    event.preventDefault();
  });

  // ---------------------------------------------------------------- routing

  /** Show what the hash names: a review, a video, the knowledge view or the
   * library */
  async function route(state) {
    if (state.get("view") === "knowledge") {
      // knowledge.js shows its own view
      leavePlayer();
      $("cf-player").hidden = true;
      $("cf-library").hidden = true;
      setChip("");
      rememberView("#cuttlefish/view=knowledge");
      clearTimeout(cf.pollTimer);
      return;
    }
    const t = parseFloat(state.get("t")) || 0;
    const id = state.get("r");
    if (id) {
      if (cf.review && cf.id === id) return;
      leavePlayer();
      const response = await fetch(
        `/api/cuttlefish/reviews/${encodeURIComponent(id)}`,
      );
      const review = await response.json();
      if (!response.ok) {
        openError(`Cannot open the review ${id}: ${review.error}`);
        return showLibrary();
      }
      return openReview(review, id, t);
    }
    if (state.get("kind")) {
      const v = videoOf(state);
      if (cf.review && !cf.id && sameVideo(cf.review.video, v)) return;
      leavePlayer();
      return openReview({ video: v, comments: [] }, null, t);
    }
    showLibrary();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    if (cf.shown && app !== "cuttlefish") {
      video.pause();
      flushSave();
      clearTimeout(cf.pollTimer);
    }
    cf.shown = app === "cuttlefish";
    if (cf.shown) route(state);
  });
  // Unsaved text is written before the page goes away
  window.addEventListener("pagehide", flushSave);

  document.querySelector('.app-nav [data-app="cuttlefish"]').href = remembered(
    "view",
    "#cuttlefish",
  );
})();
