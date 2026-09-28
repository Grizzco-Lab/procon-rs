// Predictor app: run AgentZero's inverse dynamics model (IDM) on a video
// and watch what it predicts, frame by frame, next to the truth when the
// video has a controller recording: the shared player (player.js) shows the
// video with the Full overlay and the neighbours under it; the side column
// holds the run, the predicted controller, the labels table comparing the
// two (as in the Inkspector) and the agreement; the timeline and the stored
// runs follow the video. Runs
// after app.js, player.js and stages.js and uses their helpers ($,
// escapeHtml, stickPercent, appUrl, StageMap). Runs go through /api/predictor (see
// src/predictor.rs); the video plays from /api/cuttlefish/video. State lives
// in the address: /predictor/<video>/<checkpoint>?t=<seconds>.
//
// The online mode (the model switch's "AgentZero online") runs AgentZero's
// policy instead, frame by frame as if live, through /api/predictor/online
// (src/predictor/online.rs), watched at /predictor/online: on a video, the
// player plays it along and its predictions arrive as it goes (kept as a
// run when it ends); on the live capture, the Studio's screen is lent here
// (lendScreen in app.js) with AgentZero's action drawn over it from the
// socket's `agent` messages, the loop's latency beside it, and "Let
// AgentZero play", asked for each time, sends its actions to the Switch,
// mixed with the controller, held to what it may press (the d-pad and the
// special blocked, a press-rate cap a person's measured tapping can set).
// While it plays, Stop bot sits over every app, from the bot's status in
// the socket's `status` messages, and Esc stops it anywhere on the page.
"use strict";

(() => {
  /** How often a running prediction is asked about, in ms */
  const POLL_MS = 1000;
  /** How often AgentZero is asked about while it runs, in ms */
  const ONLINE_POLL_MS = 500;
  /** How often while a person's tapping is measured, in ms */
  const MEASURE_POLL_MS = 250;
  /** GPU memory the policy wants free, in MiB */
  const POLICY_GPU_MIB = 1536;
  /** Frames AgentZero's last action is shown held over frames it skipped */
  const HOLD_FRAMES = 15;
  /** Frames fetched around the playhead for the timeline */
  const CHUNK = 1800;
  /** Button lanes, in controller order */
  const BUTTONS = [
    "zl",
    "l",
    "zr",
    "r",
    "a",
    "b",
    "x",
    "y",
    "up",
    "down",
    "left",
    "right",
    "minus",
    "plus",
    "l_stick",
    "r_stick",
    "home",
    "capture",
  ];
  /** Signal lanes: name, label, value of a label */
  const SIGNALS = [
    ["left_x", "Left stick x", (l) => stick(l.left_stick?.[0])],
    ["left_y", "Left stick y", (l) => stick(l.left_stick?.[1])],
    ["right_x", "Right stick x", (l) => stick(l.right_stick?.[0])],
    ["right_y", "Right stick y", (l) => stick(l.right_stick?.[1])],
    ["gyro_pitch", "Gyro pitch °", (l) => l.gyro_deg?.[1]],
    ["gyro_yaw", "Gyro yaw °", (l) => l.gyro_deg?.[2]],
    ["turn_x", "Camera turn x", (l) => l.camera_turn?.[0]],
    ["turn_y", "Camera turn y", (l) => l.camera_turn?.[1]],
  ];
  const STATES = {
    running: "Predicting",
    done: "Done",
    failed: "Failed",
    cancelled: "Cancelled",
  };

  const pred = {
    /** Whether the app is shown */
    shown: false,
    info: null,
    sessions: null,
    reviews: null,
    /** The current or last run */
    job: null,
    /** Stored runs, newest first */
    runs: [],
    /** The run on screen */
    run: null,
    /** Loaded labels: { start, pred: [], truth: [] | null } */
    chunk: null,
    loading: null,
    /** Seconds of video the timeline shows */
    span: 10,
    pollTimer: null,
    agreeTimer: null,
    agreeAt: 0,
    /** The form's model: "idm" or "policy" (AgentZero online) */
    model: "idm",
    /** Policy checkpoints and what agentzero-play can do */
    onlineInfo: null,
    /** AgentZero's current or last run, and the bot (letting it play),
     * also from every `status` message on the socket */
    online: null,
    bot: null,
    onlineTimer: null,
    /** A Stop bot request under way */
    releasing: false,
    /** Watching AgentZero's run (/predictor/online) */
    watching: false,
    /** The run the player was opened for, and whether it started playing */
    watchedId: null,
    playedId: null,
    /** Frames of the online predictions fetched into the chunk so far */
    patched: null,
    /** The newest action from the socket, drawn at the next frame */
    agent: null,
  };

  const canvas = $("p-timeline");

  function stick(raw) {
    return raw == null ? undefined : stickPercent(raw) / 100;
  }

  function remembered(key, fallback) {
    try {
      return localStorage.getItem(`procon-predictor-${key}`) ?? fallback;
    } catch {
      return fallback;
    }
  }

  function remember(key, value) {
    try {
      localStorage.setItem(`procon-predictor-${key}`, value);
    } catch {
      // Storage may be refused; the choice holds until reload
    }
  }

  async function api(path, body) {
    const response = await fetch(
      `/api/predictor/${path}`,
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          },
    );
    const data = await response.json();
    if (!response.ok) throw new Error(data.error ?? response.statusText);
    return data;
  }

  const clockText = (s) => {
    const m = Math.floor(s / 60);
    return `${m}:${(s - 60 * m).toFixed(1).padStart(4, "0")}`;
  };

  function setChip(text, level = "off") {
    const chip = $("p-chip");
    chip.hidden = !text;
    chip.dataset.level = level;
    chip.querySelector(".chip-text").textContent = text;
  }

  // ------------------------------------------------------------ the form

  /** Folders, checkpoints and what agentzero-predict can do */
  async function loadInfo(refresh = false) {
    if (pred.info && !refresh) return;
    if (pred.model === "idm")
      $("p-caps").textContent = "Checking agentzero-predict…";
    pred.info = await api(`info${refresh ? "?refresh=1" : ""}`);
    fillCheckpoints();
    renderCaps();
  }

  /** Policy checkpoints, what agentzero-play can do and GPU memory */
  async function loadOnlineInfo(refresh = false) {
    if (pred.onlineInfo && !refresh) return;
    if (pred.model === "policy") $("p-caps").textContent = t("po.checking");
    pred.onlineInfo = await api(
      `online/checkpoints${refresh ? "?refresh=1" : ""}`,
    );
    fillCheckpoints();
    renderCaps();
  }

  /** The checkpoints of the form's model, the one chosen before kept */
  function fillCheckpoints() {
    const policy = pred.model === "policy";
    const list = (policy ? pred.onlineInfo : pred.info)?.checkpoints;
    if (!list) return;
    const select = $("p-ckpt");
    const kept =
      (select.dataset.model === pred.model && select.value) ||
      remembered(policy ? "policy" : "ckpt", "");
    select.dataset.model = pred.model;
    select.replaceChildren(
      ...list.map(
        (c) =>
          new Option(
            `${c.name} · ${new Date(c.modified_ms).toLocaleString()}`,
            c.name,
          ),
      ),
    );
    if (!list.length)
      select.add(
        new Option(policy ? t("po.noCheckpoint") : "No checkpoints", ""),
      );
    if (list.some((c) => c.name === kept)) select.value = kept;
  }

  /** GPU memory in use, as the header says it */
  function gpuText(gpu) {
    const used = (gpu.used_mib / 1024).toFixed(1);
    const total = (gpu.total_mib / 1024).toFixed(1);
    return pred.model === "policy"
      ? t("po.gpu", { used, total })
      : `GPU memory ${used} of ${total} GiB in use`;
  }

  /** The note about the command's options, and the inputs they allow */
  function renderCaps() {
    if (pred.model === "policy") return renderOnlineCaps();
    const { info } = pred;
    if (!info) return;
    const caps = info.capabilities;
    const notes = [];
    if (!info.found)
      notes.push(`No AgentZero in ${info.agentzero} ([predictor] agentzero).`);
    if (caps.error) notes.push(caps.error);
    else if (!caps.video)
      notes.push(
        "This agentzero-predict has no --video mode yet: only sessions (and reviews of a session) can run. Recheck once AgentZero has it.",
      );
    if (!caps.error && !caps.range)
      notes.push("No --start-s/--end-s yet: the whole video is predicted.");
    const caps_el = $("p-caps");
    caps_el.textContent = notes.join(" ");
    caps_el.hidden = !notes.length;
    for (const id of ["p-start", "p-end"]) $(id).disabled = !caps.range;
    $("p-cpu-wrap").hidden = !caps.cpu;
    $("p-gpu").textContent = info.gpu ? gpuText(info.gpu) : "";
    checkForm();
  }

  /** The online mode's notes: agentzero-play without --json, no policy
   * checkpoint, too little GPU memory for the policy */
  function renderOnlineCaps() {
    const info = pred.onlineInfo;
    if (!info) return;
    const notes = [];
    const caps = info.capabilities;
    if (caps.error) notes.push(caps.error);
    else if (!caps.json) notes.push(t("po.noJson"));
    else if (!caps.shared && $("p-kind").value === "live")
      notes.push(t("po.noShared"));
    if (!info.checkpoints.length)
      notes.push(t("po.noPolicy", { folder: info.folder }));
    const gpu = info.gpu;
    const free = gpu ? gpu.total_mib - gpu.used_mib : null;
    if (free != null && free < POLICY_GPU_MIB && !$("p-cpu").checked)
      notes.push(t("po.gpuLow", { free: (free / 1024).toFixed(1) }));
    if ($("p-kind").value === "live" && !info.input)
      notes.push(t("po.noInput"));
    const el = $("p-caps");
    el.textContent = notes.join(" ");
    el.hidden = !notes.length;
    for (const id of ["p-start", "p-end"]) $(id).disabled = false;
    $("p-cpu-wrap").hidden = false;
    $("p-gpu").textContent = gpu ? gpuText(gpu) : "";
    checkForm();
  }

  /** List the sessions afresh each time the app or the kind is shown, as
   * sessions get recorded while the page is open; the choice stays */
  async function loadSessions() {
    try {
      const response = await fetch("/api/inspect/sessions");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      pred.sessions = data.sessions;
    } catch (error) {
      pred.sessions = [];
      $("p-session").replaceChildren(
        new Option(`No sessions: ${error.message}`, ""),
      );
      return;
    }
    const kept = $("p-session").value || remembered("session", "");
    $("p-session").replaceChildren(
      ...pred.sessions.map((s) => new Option(s.name, s.name)),
    );
    if (pred.sessions.some((s) => s.name === kept)) $("p-session").value = kept;
    fillSegments();
  }

  function fillSegments() {
    const summary = pred.sessions?.find((s) => s.name === $("p-session").value);
    const select = $("p-segment");
    const kept = select.value;
    select.replaceChildren(
      ...(summary?.segments ?? []).map((s) => new Option(s.file, s.file)),
    );
    if (summary?.segments.some((s) => s.file === kept)) select.value = kept;
    select.hidden = (summary?.segments.length ?? 0) < 2;
  }

  /** A review of a session's recording, which has controller data */
  const sessionReview = (r) => r?.video.kind === "session" && !r.video.file;

  async function loadReviews() {
    if (pred.reviews) return;
    try {
      const response = await fetch("/api/cuttlefish/reviews");
      const data = await response.json();
      if (!response.ok) throw new Error(data.error);
      // A review started from Cuttlefish's chat may have no video yet
      pred.reviews = data.reviews.filter((r) => r.video);
    } catch (error) {
      pred.reviews = [];
      $("p-review").replaceChildren(
        new Option(`No reviews: ${error.message}`, ""),
      );
      return;
    }
    $("p-review").replaceChildren(
      ...pred.reviews.map((r) => {
        const v = r.video;
        const name =
          v.title ??
          (v.kind === "youtube" ? v.ref : v.ref.split("/").pop()) ??
          r.id;
        return new Option(`${r.id} · ${v.kind} · ${name}`, r.id);
      }),
    );
    if (!pred.reviews.length) $("p-review").add(new Option("No reviews", ""));
  }

  /** Show the inputs of the chosen kind of video (or the live capture) */
  function showKind() {
    const kind = $("p-kind").value;
    remember(pred.model === "policy" ? "policyKind" : "kind", kind);
    $("p-session-row").hidden = kind !== "session";
    $("p-review").hidden = kind !== "review";
    $("p-file").hidden = kind !== "file";
    // The live capture has no range
    for (const id of ["p-start", "p-end"]) $(id).hidden = kind === "live";
    if (kind === "session") loadSessions();
    if (kind === "review") loadReviews().then(checkForm);
    if (pred.model === "policy") renderOnlineCaps();
    checkForm();
  }

  /** The source the form names */
  function source() {
    const kind = $("p-kind").value;
    if (kind === "session")
      return { kind, s: $("p-session").value, seg: $("p-segment").value };
    if (kind === "review") return { kind, id: $("p-review").value };
    return { kind, path: $("p-file").value.trim() };
  }

  /** Whether the chosen video can run with this agentzero-predict (or
   * agentzero-play, online) */
  function checkForm() {
    if (pred.model === "policy") {
      const info = pred.onlineInfo;
      const caps = info?.capabilities;
      // The live capture needs the studio's shared frames
      const live = $("p-kind").value === "live";
      const ready = Boolean(
        caps?.json && (caps.shared || !live) && info.checkpoints.length,
      );
      $("p-run").disabled = !ready || onlineRunning();
      $("p-run").title = onlineRunning() ? t("po.running") : "";
      return;
    }
    const caps = pred.info?.capabilities;
    const src = source();
    const needsVideo =
      src.kind === "file" ||
      (src.kind === "review" &&
        !sessionReview(pred.reviews?.find((r) => r.id === src.id)));
    const blocked = Boolean(caps && !caps.video && needsVideo);
    $("p-run").disabled = blocked || running(pred.job);
    $("p-run").title = blocked
      ? "agentzero-predict has no --video mode yet"
      : "";
  }

  $("p-kind").onchange = showKind;
  $("p-session").onchange = () => {
    remember("session", $("p-session").value);
    fillSegments();
  };
  $("p-review").onchange = checkForm;
  $("p-file").oninput = checkForm;
  $("p-cpu").onchange = () => pred.model === "policy" && renderOnlineCaps();
  $("p-recheck").onclick = () =>
    (pred.model === "policy" ? loadOnlineInfo(true) : loadInfo(true)).catch(
      (error) => showRunError(error.message),
    );

  /** Switch the form between the IDM and AgentZero online; with `load`,
   * read the model's checkpoints (the app does when shown) */
  function setModel(model, load = true) {
    pred.model = model === "policy" ? "policy" : "idm";
    remember("model", pred.model);
    const policy = pred.model === "policy";
    for (const button of document.querySelectorAll("[data-model]")) {
      button.setAttribute(
        "aria-pressed",
        String(button.dataset.model === pred.model),
      );
    }
    for (const option of $("p-kind").querySelectorAll("[data-policy-only]")) {
      option.hidden = !policy;
    }
    const kind = policy
      ? remembered("policyKind", "live")
      : remembered("kind", "session");
    $("p-kind").value = !policy && kind === "live" ? "session" : kind;
    $("po-rec-wrap").hidden = !policy;
    $("p-run").textContent = policy ? t("po.start") : "Run the IDM";
    fillCheckpoints();
    renderCaps();
    if (!load) return;
    showKind();
    const loading = policy ? loadOnlineInfo() : loadInfo();
    loading.catch((error) => showRunError(error.message));
  }

  for (const button of document.querySelectorAll("[data-model]")) {
    button.onclick = () => setModel(button.dataset.model);
  }
  setModel(remembered("model", "idm"), false);

  $("p-form").onsubmit = async (event) => {
    event.preventDefault();
    const number = (id) => {
      const text = $(id).value.trim();
      return text === "" || $(id).disabled ? null : Number(text);
    };
    showRunError(null);
    if (pred.model === "policy") return startOnline(number);
    remember("ckpt", $("p-ckpt").value);
    try {
      pred.job = await api("run", {
        source: source(),
        start_s: number("p-start"),
        end_s: number("p-end"),
        checkpoint: $("p-ckpt").value,
        cpu: $("p-cpu").checked,
      });
    } catch (error) {
      return showRunError(error.message);
    }
    renderJob();
    poll();
  };

  $("p-cancel").onclick = async () => {
    $("p-cancel").disabled = true;
    try {
      pred.job = await api("cancel", {});
    } catch (error) {
      showRunError(error.message);
    }
    renderJob();
    poll();
  };

  function showRunError(message) {
    const el = $("p-error");
    el.hidden = !message;
    el.textContent = message ?? "";
  }

  // ------------------------------------------------------------- the run

  const running = (job) => job?.state === "running";

  async function poll() {
    clearTimeout(pred.pollTimer);
    const before = pred.job;
    try {
      pred.job = await api("job");
    } catch {
      return;
    }
    renderJob();
    if (running(pred.job) && pred.shown && !document.hidden) {
      pred.pollTimer = setTimeout(poll, POLL_MS);
    }
    // Just finished: list it and show it
    if (running(before) && pred.job?.state === "done") {
      await loadRuns();
      navigate(runUrl(pred.job.key, pred.job.checkpoint, 0));
    }
  }

  function renderJob() {
    const job = pred.job;
    const busy = running(job);
    $("p-cancel").hidden = !busy;
    // Told to stop: the run ends within seconds (see `cancel` in
    // src/predictor.rs)
    $("p-cancel").disabled = Boolean(job?.stopping);
    checkForm();
    renderChip(job);
    const meter = $("p-progress");
    meter.hidden = !job;
    $("p-log-wrap").hidden = !job;
    if (!job) return;
    meter.dataset.level =
      job.state === "failed"
        ? "critical"
        : job.state === "cancelled"
          ? "warning"
          : "ok";
    $("p-state").textContent =
      busy && job.stopping ? "Cancelling…" : (STATES[job.state] ?? job.state);
    const share = job.total ? job.done / job.total : busy ? 0 : 1;
    $("p-count").textContent = job.total
      ? `${job.done} / ${job.total}`
      : job.seconds != null
        ? `${job.seconds} s`
        : "";
    meter.querySelector(".meter-fill").style.width = `${100 * share}%`;
    const range =
      job.start_s != null || job.end_s != null
        ? ` · ${job.start_s ?? 0}–${job.end_s ?? "end"} s`
        : "";
    $("p-job-note").textContent =
      `${job.title}${range} · ${job.checkpoint}${job.cpu ? " · CPU" : ""}`;
    if (job.error) {
      const el = $("p-error");
      el.hidden = false;
      el.classList.toggle("p-oom", job.out_of_memory);
      el.textContent = job.out_of_memory
        ? `GPU out of memory. ${job.error}.`
        : job.error;
    } else if (busy) showRunError(null);
    $("p-command").textContent = job.command;
    $("p-log").textContent = job.log.join("\n");
  }

  function renderChip(job) {
    if (!job) return setChip("");
    const text = {
      running: job.total
        ? `IDM: ${job.done}/${job.total}`
        : "IDM: loading the model",
      done: `IDM: done in ${job.seconds} s`,
      failed: job.out_of_memory ? "IDM: GPU out of memory" : "IDM: failed",
      cancelled: "IDM: cancelled",
    }[job.state];
    const level = {
      running: "warning",
      done: "good",
      failed: "critical",
      cancelled: "off",
    }[job.state];
    setChip(text, level);
  }

  // ------------------------------------------------------- AgentZero online

  const onlineRunning = () => pred.online?.state === "running";

  async function startOnline(number) {
    const live = $("p-kind").value === "live";
    remember("policy", $("p-ckpt").value);
    try {
      pred.online = await api("online/start", {
        source: live ? null : source(),
        start_s: live ? null : number("p-start"),
        end_s: live ? null : number("p-end"),
        checkpoint: $("p-ckpt").value,
        cpu: $("p-cpu").checked,
        allow_recording: $("po-rec").checked,
      });
    } catch (error) {
      return showRunError(error.message);
    }
    renderOnline();
    navigate(appUrl("predictor", { view: "online" }));
    pollOnline();
  }

  $("po-stop").onclick = async () => {
    $("po-stop").disabled = true;
    try {
      await api("online/stop", {});
    } catch (error) {
      showRunError(error.message);
    }
    pollOnline();
  };

  /** A person's tapping being measured */
  const measuring = () =>
    ["waiting", "counting"].includes(pred.bot?.tapping?.state);

  /** AgentZero's run and the bot, again every ONLINE_POLL_MS while it runs
   * (MEASURE_POLL_MS while a person's tapping is measured) and the app is
   * shown */
  async function pollOnline() {
    clearTimeout(pred.onlineTimer);
    try {
      const data = await api("online/status");
      pred.online = data.run;
      pred.bot = data.bot;
    } catch {
      return;
    }
    renderOnline();
    if (pred.watching) followOnline();
    const active = onlineRunning() || pred.bot?.playing || measuring();
    if (active && pred.shown && !document.hidden) {
      const every = measuring() ? MEASURE_POLL_MS : ONLINE_POLL_MS;
      pred.onlineTimer = setTimeout(pollOnline, every);
    }
  }

  /** The run's state in the form panel, the loop and "Let AgentZero play" */
  function renderOnline() {
    const run = pred.online;
    $("po-run").hidden = !run;
    checkForm();
    if (!run) return;
    const busy = run.state === "running";
    const state = run.loading
      ? "loading"
      : busy && pred.bot?.playing
        ? "playing"
        : busy
          ? run.live
            ? "live"
            : "video"
          : run.state;
    $("po-state").textContent = t(`po.state.${state}`);
    $("po-chip").dataset.level =
      {
        loading: "warning",
        live: "good",
        video: "good",
        playing: "critical",
        done: "off",
        cancelled: "off",
        failed: "critical",
      }[state] ?? "off";
    const where = run.device ?? (run.cpu ? "cpu" : null);
    $("po-note").textContent = [
      run.live ? t("po.kind.live") : run.title,
      run.checkpoint,
      where,
      run.frames ? t("po.actions", { n: run.frames }) : null,
    ]
      .filter(Boolean)
      .join(" · ");
    $("po-stop").hidden = !busy;
    $("po-stop").disabled = Boolean(run.stopping);
    $("po-stop").textContent = run.stopping ? t("po.stopping") : t("po.stop");
    $("po-watch").hidden = !busy || pred.watching;
    const error = $("po-error");
    error.hidden = !run.error;
    error.textContent = run.error ?? "";
    const stored = $("po-stored");
    stored.hidden = !run.stored;
    if (run.stored) {
      const t0 = run.frame_offset / run.fps;
      stored.innerHTML = `${escapeHtml(t("po.stored"))} <a href="${escapeHtml(runUrl(run.key, run.stored, t0))}">${escapeHtml(t("po.openStored"))}</a>`;
    }
    $("po-command").textContent = run.command;
    $("po-log").textContent = run.log.join("\n");
    renderPlay();
    renderLoop();
  }

  /** "Let AgentZero play": off or playing (with the time left), what it may
   * press and the measurement of a person's tapping; only while it runs on
   * the live capture */
  function renderPlay() {
    const run = pred.online;
    const bot = pred.bot;
    const live = run?.live && run.state === "running" && !run.loading;
    $("po-play").hidden = !live && !bot?.playing;
    const playing = Boolean(bot?.playing);
    $("po-play").classList.toggle("is-playing", playing);
    $("po-play-label").textContent = playing
      ? t("po.play.stop")
      : t("po.play.start");
    // Its presses would count as the person's
    $("po-play-btn").disabled = !playing && measuring();
    let note;
    if (playing) {
      note = t("po.play.left", {
        left: clockText(Math.max(0, (bot.until_ms - Date.now()) / 1000)),
        sent: bot.sent,
      });
    } else if (bot?.ended) {
      note = t("po.play.ended", { why: t(`po.ended.${bot.ended}`) });
    } else {
      note = t("po.play.off");
    }
    $("po-play-note").textContent = note;
    $("po-live").dataset.state = playing ? "playing" : "watching";
    $("po-badge").textContent = playing
      ? t("po.badge.playing")
      : t("po.badge.watching");
    renderLimits();
    renderMeasure();
    renderStopBot();
  }

  // ------------------------------------------------ what it may press

  /** The masks and the cap as the studio holds them; a number being typed
   * is left alone */
  function renderLimits() {
    const limits = pred.bot?.limits;
    if (!limits) return;
    $("po-block-dpad").checked = limits.block_dpad;
    $("po-block-special").checked = limits.block_special;
    for (const [id, value] of [
      ["po-cap", limits.max_hz],
      ["po-hold", limits.min_hold_ms],
    ]) {
      if (document.activeElement !== $(id)) $(id).value = value;
    }
  }

  /** Change what it may press; the studio keeps it, and it holds from the
   * next action on, while it plays too */
  async function saveLimits(change) {
    try {
      pred.bot = await api("online/limits", { ...pred.bot?.limits, ...change });
    } catch (error) {
      showRunError(error.message);
    }
    renderPlay();
  }

  $("po-block-dpad").onchange = () =>
    saveLimits({ block_dpad: $("po-block-dpad").checked });
  $("po-block-special").onchange = () =>
    saveLimits({ block_special: $("po-block-special").checked });
  $("po-cap").onchange = () =>
    saveLimits({ max_hz: Number($("po-cap").value) });
  $("po-hold").onchange = () =>
    saveLimits({ min_hold_ms: Number($("po-hold").value) });

  /** What it may not press and how fast it may, for the confirmation */
  function limitsText(limits) {
    if (!limits) return "";
    const blocked = [
      limits.block_dpad && t("po.limits.dpadShort"),
      limits.block_special && t("po.limits.specialShort"),
      t("po.limits.systemShort"),
    ].filter(Boolean);
    return t("po.confirm.limits", {
      blocked: blocked.join(t("po.limits.and")),
      hz: limits.max_hz,
      ms: limits.min_hold_ms,
    });
  }

  // ------------------------------------------ measuring a person's tapping

  /** The measurement: waiting for the first press, counting, the result
   * with the cap it suggests */
  function renderMeasure() {
    const tapping = pred.bot?.tapping;
    const state = tapping?.state;
    const button = $("po-measure-btn");
    button.textContent = measuring()
      ? t("po.measure.cancel")
      : t("po.measure.start");
    button.disabled = Boolean(pred.bot?.playing);
    const result = state === "done" ? tapping.result : null;
    const note = {
      waiting: () => t("po.measure.waiting"),
      counting: () =>
        t("po.measure.counting", {
          n: tapping.presses,
          s: tapping.left_s.toFixed(1),
        }),
      done: () =>
        t("po.measure.result", {
          fastest: result.fastest_hz,
          average: result.average_hz,
          hold: result.shortest_hold_ms ?? "–",
        }),
      none: () => t("po.measure.none"),
    }[state];
    $("po-measure-note").textContent = note ? note() : "";
    const use = $("po-measure-use");
    use.hidden = !result || result.cap_hz === pred.bot.limits.max_hz;
    if (result) use.textContent = t("po.measure.use", { hz: result.cap_hz });
  }

  $("po-measure-btn").onclick = async () => {
    try {
      pred.bot = await api(
        "online/measure",
        measuring() ? { cancel: true } : {},
      );
    } catch (error) {
      showRunError(error.message);
    }
    renderPlay();
    pollOnline();
  };

  // The cap from the person's own fastest
  $("po-measure-use").onclick = () =>
    saveLimits({ max_hz: pred.bot.tapping.result.cap_hz });

  // ------------------------------------------------------- playing, Stop

  $("po-play-btn").onclick = () => {
    if (pred.bot?.playing) return release();
    // Asked every time
    $("po-confirm-limits").textContent = limitsText(pred.bot?.limits);
    $("po-confirm").showModal();
  };

  $("po-confirm").addEventListener("close", async () => {
    if ($("po-confirm").returnValue !== "play") return;
    try {
      pred.bot = await api("online/play", {
        seconds: Number($("po-seconds").value),
      });
    } catch (error) {
      showRunError(error.message);
    }
    renderPlay();
    pollOnline();
  });

  /** Stop bot: the studio lets go at once (one request at a time; another
   * press tries again if it failed) */
  async function release() {
    if (pred.releasing) return;
    pred.releasing = true;
    $("bot-stop").disabled = true;
    try {
      pred.bot = await api("online/release", {});
    } catch (error) {
      showRunError(error.message);
    } finally {
      pred.releasing = false;
      $("bot-stop").disabled = false;
    }
    renderPlay();
  }

  /** Stop bot over every app, while AgentZero plays, with the time left */
  function renderStopBot() {
    const bot = pred.bot;
    const button = $("bot-stop");
    button.hidden = !bot?.playing;
    document.body.classList.toggle("bot-playing", !button.hidden);
    if (button.hidden) return;
    $("bot-stop-left").textContent = t("bot.left", {
      left: clockText(Math.max(0, (bot.until_ms - Date.now()) / 1000)),
    });
  }

  $("bot-stop").onclick = release;

  // The bot's status twice a second from the socket, whichever app is shown
  window.addEventListener("bot", ({ detail }) => {
    if (!detail) return;
    const was = pred.bot?.playing;
    pred.bot = detail;
    renderStopBot();
    if (pred.shown && (was || detail.playing)) renderPlay();
  });

  // Esc stops AgentZero playing, wherever the page is: first, before any
  // other use of the key
  window.addEventListener(
    "keydown",
    (event) => {
      if (event.key === "Escape" && pred.bot?.playing) {
        event.preventDefault();
        release();
      }
    },
    true,
  );

  /** The loop's latency (live) or the model's time (a video), median and
   * 99th percentile over the last seconds */
  function renderLoop() {
    const run = pred.online;
    const panel = $("po-loop-panel");
    panel.hidden = !pred.watching || !run;
    if (panel.hidden) return;
    const timings = run.timings ?? {};
    // The hand-off's parts under it
    const rows = run.live
      ? [
          "handoff",
          "grabber",
          "pipe",
          "shared",
          "wait",
          "upload",
          "model",
          "send",
          "total",
        ]
      : ["model", "age"];
    const ms = (v) => (v == null ? "–" : v.toFixed(1));
    const sending = Boolean(pred.bot?.playing);
    const classes = {
      grabber: "po-part",
      pipe: "po-part",
      shared: "po-part",
      wait: "po-part",
      total: "po-total",
    };
    $("po-loop").innerHTML = rows
      .map((key) => {
        const value = timings[key];
        const label =
          key === "send" && !sending
            ? t("po.loop.sendDry")
            : t(`po.loop.${key}`);
        const cls = classes[key] ? ` class="${classes[key]}"` : "";
        return `<tr${cls} title="${escapeHtml(t(`po.loop.${key}Note`))}"><td>${escapeHtml(label)}</td><td class="num">${ms(value?.median)}</td><td class="num">${ms(value?.p99)}</td></tr>`;
      })
      .join("");
    $("po-loop-note").textContent = [
      timings.rate
        ? t("po.loop.rate", { rate: timings.rate.toFixed(1) })
        : null,
      run.skipped ? t("po.loop.skipped", { n: run.skipped }) : null,
      run.device,
    ]
      .filter(Boolean)
      .join(" · ");
    $("po-loop-foot").textContent = run.live
      ? t("po.loop.footLive")
      : t("po.loop.footVideo");
  }

  // ---------------------------------------------------- watching it run

  /** The viewer follows AgentZero's run: the live capture, or its video */
  function followOnline() {
    const run = pred.online;
    if (!run) {
      hideLive();
      $("p-empty").hidden = false;
      $("p-viewer-note").textContent = t("po.notYet");
      return;
    }
    if (run.live) {
      showLive(run);
      return;
    }
    hideLive();
    if (pred.watchedId !== run.id) openOnlineVideo(run);
    // Play along once AgentZero sees frames (the video's length known)
    if (
      run.state === "running" &&
      run.frames > 0 &&
      pred.playedId !== run.id &&
      player.frames > 0
    ) {
      player.play();
      if (player.playing) pred.playedId = run.id;
    }
    if (run.state === "running" || pred.patched !== null) patchOnline(run);
  }

  /** The thumbnails of a video, for the strip */
  function thumbOf(play) {
    return (k) => {
      const query = new URLSearchParams(play);
      query.set("t_ms", Math.round((k * 1000) / player.fps));
      return `/api/cuttlefish/thumb?${query}`;
    };
  }

  /** Play AgentZero's video in the viewer, its predictions coming in */
  function openOnlineVideo(run) {
    pred.watchedId = run.id;
    pred.run = {
      online: true,
      id: run.id,
      key: run.key,
      checkpoint: `policy-${run.checkpoint}`,
      title: run.title,
      fps: run.fps,
      frame_offset: run.frame_offset,
      play: run.play,
      session: run.session,
    };
    pred.chunk = null;
    pred.patched = null;
    renderRuns();
    $("p-empty").hidden = true;
    $("p-viewer-note").textContent =
      `${run.title} · AgentZero ${run.checkpoint} · ${run.fps} fps`;
    $("p-agree-mode").hidden = true;
    $("p-agree-mode").value = "view";
    drawStage(pred.run);
    player.open(
      {
        video: `/api/cuttlefish/video?${new URLSearchParams(run.play)}`,
        fps: run.fps,
        sound: true,
        thumb: thumbOf(run.play),
        labels,
        title: `${run.title} · AgentZero ${run.checkpoint}`,
      },
      run.frame_offset,
    );
  }

  /** Past the newest frame predicted: the one seen, `lead` ahead, in the
   * video's frames */
  const onlineHead = (run) =>
    run.frame_offset + (run.seen ?? 0) + (run.lead ?? 0) + 1;

  /** New predictions of the running video into the loaded frames */
  async function patchOnline(run) {
    const chunk = pred.chunk;
    if (!chunk || !pred.run?.online || pred.run.id !== run.id) return;
    const end = chunk.start + chunk.pred.length;
    const from = Math.max(chunk.start, pred.patched ?? chunk.start);
    const to = Math.min(end, onlineHead(run));
    if (to <= from) return;
    let data;
    try {
      data = await api(
        `online/labels?${new URLSearchParams({ start: from, stop: to })}`,
      );
    } catch {
      return;
    }
    if (pred.chunk !== chunk) return;
    data.pred.forEach((label, i) => {
      if (label) chunk.pred[from - chunk.start + i] = label;
    });
    // A little overlap: frames skipped then may still come
    pred.patched = run.state === "running" ? Math.max(from, to - 8) : null;
    draw();
    player.refresh();
  }

  /** The live capture: the Studio's screen, lent, with AgentZero's action
   * over it */
  function showLive(run) {
    if (!pred.liveShown) {
      pred.liveShown = true;
      player.close();
      pred.run = null;
      pred.chunk = null;
      pred.watchedId = run.id;
      renderRuns();
      for (const id of ["p-screen", "p-scrubber", "p-strip", "p-stage"]) {
        $(id).hidden = true;
      }
      $("p-player-controls").hidden = true;
      for (const panel of livePanelsHidden()) panel.hidden = true;
      $("po-live").hidden = false;
      $("predictor-layout").classList.add("is-live");
      lendScreen($("po-live"));
    }
    $("p-viewer-note").textContent =
      `${t("po.kind.live")} · ${run.title} · AgentZero ${run.checkpoint}`;
  }

  /** The panels about frames of a video, which the live capture has not */
  const livePanelsHidden = () =>
    [".p-p-frames", ".p-p-timeline", ".p-p-agree"].map((s) =>
      document.querySelector(s),
    );

  function hideLive() {
    if (!pred.liveShown) return;
    pred.liveShown = false;
    lendScreen(null);
    $("po-live").hidden = true;
    $("predictor-layout").classList.remove("is-live");
    for (const id of ["p-screen", "p-scrubber", "p-strip", "p-stage"]) {
      $(id).hidden = false;
    }
    $("p-player-controls").hidden = false;
    for (const panel of livePanelsHidden()) panel.hidden = false;
    drawMini(undefined);
  }

  /** AgentZero's action over the live picture, as the Studio draws the
   * controller's */
  const liveHud = $("input-hud").cloneNode(true);
  liveHud.removeAttribute("id");
  liveHud.classList.add("po-hud");
  liveHud.dataset.keys = "";
  liveHud.removeAttribute("hidden");
  const badge = document.createElement("span");
  badge.className = "po-badge";
  badge.id = "po-badge";
  $("po-live").append(liveHud, badge);

  // Each action from the socket; drawn at the next frame, the newest only
  window.addEventListener("agent", ({ detail }) => {
    if (!pred.liveShown || detail.id !== pred.online?.id) return;
    if (!pred.agent) requestAnimationFrame(drawAgent);
    pred.agent = detail;
  });

  function drawAgent() {
    const action = pred.agent;
    pred.agent = null;
    if (!action || !pred.liveShown) return;
    const send = action.send ?? {};
    drawInputHud(liveHud, {
      left: (send.left_stick ?? [2048, 2048]).map(stickPercent),
      right: (send.right_stick ?? [2048, 2048]).map(stickPercent),
      pressed: new Set(send.buttons ?? []),
      yaw: (send.gyro?.[2] ?? 0) * GYRO_DPS,
      pitch: (send.gyro?.[1] ?? 0) * GYRO_DPS,
    });
    const shown = { ...action, ...send };
    drawMini(shown);
    drawProbs(shown, undefined);
  }

  // ------------------------------------------------------------ the runs

  async function loadRuns() {
    try {
      pred.runs = (await api("runs")).runs;
    } catch (error) {
      $("p-runs").innerHTML =
        `<tr><td colspan="6" class="level-critical">${escapeHtml(error.message)}</td></tr>`;
      return;
    }
    renderRuns();
  }

  /** The state of a run's view at `t` seconds */
  const runState = (key, ckpt, t) => ({ key, ckpt, t: t.toFixed(2) });
  const runUrl = (...view) => appUrl("predictor", runState(...view));

  function renderRuns() {
    const body = $("p-runs");
    body.replaceChildren();
    $("p-runs-note").textContent = pred.runs.length
      ? `${pred.runs.length}`
      : "";
    if (!pred.runs.length) {
      body.innerHTML = `<tr><td colspan="5" class="panel-note">No predictions yet: run one.</td></tr>`;
    }
    for (const run of pred.runs) {
      const tr = document.createElement("tr");
      const range =
        run.start_s != null || run.end_s != null
          ? `${run.start_s ?? 0}–${run.end_s ?? "end"} s`
          : "whole";
      const when = new Date(run.finished_ms ?? run.started_ms);
      tr.innerHTML = `<td>${escapeHtml(run.title)}${run.session ? ' <span class="p-tag">truth</span>' : ""}</td><td>${escapeHtml(run.checkpoint)}</td><td class="num">${range}</td><td class="num">${run.frames}</td><td class="num">${run.seconds ?? "–"} s</td><td class="num">${when.toLocaleString()}</td>`;
      if (pred.run?.key === run.key && pred.run?.checkpoint === run.checkpoint)
        tr.className = "current";
      tr.onclick = () => {
        navigate(runUrl(run.key, run.checkpoint, 0));
      };
      body.append(tr);
    }
  }

  // ------------------------------------------------------------ the viewer

  /** A small controller beside the video, copied from the Studio */
  const mini = $("procon").cloneNode(true);
  for (const el of [mini, ...mini.querySelectorAll("[id]")]) {
    if (!el.closest("defs")) el.removeAttribute("id");
  }
  mini.classList.remove("is-behind", "is-idle");
  mini.classList.add("p-mini");
  mini.style.transform = "";
  mini.setAttribute("aria-label", "Predicted controller");
  $("p-mini").append(mini);
  const miniButtons = [...mini.querySelectorAll("[data-btn]")];
  const miniSticks = {
    l: mini.querySelector('[data-btn="l_stick"]'),
    r: mini.querySelector('[data-btn="r_stick"]'),
  };

  /** The shared player: the run's video with the overlays and labels table */
  const player = new Player({
    screen: $("p-screen"),
    controls: $("p-player-controls"),
    scrubber: $("p-scrubber"),
    strip: $("p-strip"),
    table: $("p-rows"),
    compact: true,
    predNote: $("p-pred-note"),
    remember: "predictor",
    neighbours: { radius: 3 },
    onFrame,
    onError(message) {
      $("p-viewer-note").textContent = message;
    },
  });

  /** Open a stored run */
  function openRun(key, ckpt, t) {
    const run = pred.runs.find((r) => r.key === key && r.checkpoint === ckpt);
    if (!run) {
      $("p-viewer-note").textContent = `No stored run ${key} / ${ckpt}.`;
      return;
    }
    const same = pred.run === run;
    pred.run = run;
    renderRuns();
    $("p-empty").hidden = true;
    $("p-viewer-note").textContent =
      `${run.title} · ${run.checkpoint} · ${run.fps?.toFixed(2) ?? "?"} fps`;
    $("p-agree-mode").hidden = !run.session;
    const n = Math.round(t * (run.fps || 30));
    if (same) {
      if (player.frame !== n) player.go(n);
      return;
    }
    pred.chunk = null;
    drawStage(run);
    player.open(
      {
        video: `/api/cuttlefish/video?${new URLSearchParams(run.play)}`,
        fps: run.fps || 30,
        sound: true,
        thumb: thumbOf(run.play),
        labels,
        title: `${run.title} · ${run.checkpoint}`,
      },
      n,
    );
  }

  /** Links to Gungee's maps of the run's stage, when its review (or a
   * review of the same video) names it, or its title does */
  const stageMap = new StageMap($("p-stage"));
  async function drawStage(run) {
    stageMap.set("");
    const { r, kind, ref } = run.play;
    const stage = await stageOfVideo({ r, kind, ref, title: run.title });
    if (pred.run === run) stageMap.set(stage);
  }

  const fps = () => player.fps;
  const frameCount = () => player.frames;

  /** The labels of frame n, if loaded: [prediction, truth] */
  function labelsAt(n) {
    const chunk = pred.chunk;
    if (!chunk || n < chunk.start || n >= chunk.start + chunk.pred.length)
      return [undefined, undefined];
    const i = n - chunk.start;
    let p = chunk.pred[i];
    // AgentZero skips frames while busy, as live; its last action holds
    // until the next (for as long as the bot's would, STALL in online.rs)
    if (!p && pred.run?.checkpoint?.startsWith("policy-")) {
      for (let k = i - 1; k >= Math.max(0, i - HOLD_FRAMES) && !p; k--) {
        p = chunk.pred[k];
      }
    }
    return [p, chunk.truth?.[i]];
  }

  /** The player's labels: [truth, prediction]; nothing until the chunk is here */
  async function labels(n) {
    const [p, t] = labelsAt(n);
    const loaded =
      pred.chunk && n >= pred.chunk.start && n < pred.chunk.start + CHUNK;
    return [t, loaded ? (p ?? null) : undefined];
  }

  /** Load the frames around n unless they are loaded */
  async function ensureChunk(n) {
    const chunk = pred.chunk;
    const half = Math.ceil((pred.span * fps()) / 2) + 30;
    const low = Math.max(0, n - half);
    const high = Math.min(frameCount() || Infinity, n + half);
    if (chunk && low >= chunk.start && high <= chunk.start + chunk.pred.length)
      return;
    const start = Math.max(0, n - CHUNK / 2);
    const key = `${pred.run.key}/${pred.run.checkpoint}/${start}/${pred.run.id ?? ""}`;
    if (pred.loading === key) return;
    pred.loading = key;
    const run = pred.run;
    // AgentZero's run in progress: its predictions so far
    const range = { start, stop: start + CHUNK };
    const path = run.online
      ? `online/labels?${new URLSearchParams(range)}`
      : `labels?${new URLSearchParams({ key: run.key, ckpt: run.checkpoint, ...range })}`;
    try {
      const data = await api(path);
      if (pred.run !== run) return;
      pred.chunk = { start, pred: data.pred, truth: data.truth };
      // Newer predictions come in by patchOnline
      if (run.online)
        pred.patched = onlineRunning()
          ? Math.max(start, onlineHead(pred.online) - 8)
          : null;
    } catch (error) {
      $("p-viewer-note").textContent = error.message;
    } finally {
      if (pred.loading === key) pred.loading = null;
    }
    draw();
    player.refresh();
  }

  /** The player shows another frame: follow it */
  function onFrame(n) {
    if (!pred.run) return;
    ensureChunk(n);
    draw();
    scheduleAgreement();
    // AgentZero's run keeps its own address
    if (pred.run.online) return;
    const state = runState(pred.run.key, pred.run.checkpoint, n / fps());
    rememberView(replaceRoute("predictor", state));
  }

  $("p-span").value = String(pred.span);
  $("p-span").onchange = (event) => {
    pred.span = Number(event.target.value);
    ensureChunk(player.frame);
    draw();
    scheduleAgreement();
  };

  // ---------------------------------------------------------- drawing

  function draw() {
    if (!pred.run || !pred.shown) return;
    const [p, t] = labelsAt(player.frame);
    drawMini(p);
    drawProbs(p, t);
    drawTimeline();
  }

  /** The small controller: the predicted buttons and sticks */
  function drawMini(p) {
    const pressed = new Set(p?.buttons ?? []);
    for (const el of miniButtons) {
      el.classList.toggle("on", pressed.has(el.dataset.btn));
    }
    for (const [side, key] of [
      ["l", "left_stick"],
      ["r", "right_stick"],
    ]) {
      const [x, y] = (p?.[key] ?? [2048, 2048]).map(stickPercent);
      miniSticks[side]?.setAttribute(
        "transform",
        `translate(${((x / 100) * 24).toFixed(1)} ${((-y / 100) * 24).toFixed(1)})`,
      );
    }
    mini.classList.toggle("is-idle", !p);
  }

  /** Each button's predicted probability, pressed ones marked */
  function drawProbs(p, t) {
    const probs = p?.button_probs ?? {};
    const truth = new Set(t?.buttons ?? []);
    const shown = BUTTONS.filter(
      (b) => (probs[b] ?? 0) >= 0.05 || truth.has(b),
    );
    $("p-probs").innerHTML = shown.length
      ? shown
          .map((b) => {
            const value = probs[b] ?? 0;
            const on = (p?.buttons ?? []).includes(b);
            return `<div class="p-prob${on ? " on" : ""}"><span>${b.toUpperCase()}</span><i style="width:${(100 * value).toFixed(0)}%"></i><b class="num">${value.toFixed(2)}</b>${truth.has(b) ? '<em title="pressed in the recording">✓</em>' : ""}</div>`;
          })
          .join("")
      : `<p class="panel-note">${p ? "No button above 5%." : "No prediction for this frame."}</p>`;
  }

  /** Theme colors for the canvas */
  function colors() {
    const style = getComputedStyle(canvas);
    const get = (name) => style.getPropertyValue(name).trim();
    return {
      truth: get("--s1") || "#3987e5",
      pred: get("--s2") || "#d95926",
      grid: get("--grid") || "#222",
      axis: get("--axis") || "#444",
      text: get("--text-3") || "#888",
      now: get("--text") || "#fff",
    };
  }

  /** Buttons and signals around the playhead, truth filled, prediction as
   * lines */
  function drawTimeline() {
    const chunk = pred.chunk;
    const width = canvas.clientWidth;
    if (!width) return;
    const n0 = player.frame;
    const half = (pred.span * fps()) / 2;
    const first = Math.floor(n0 - half);
    const last = Math.ceil(n0 + half);
    const lanes = [];
    const has = (key, label) => label && label[key] != null;
    const frames = [];
    for (let n = first; n <= last; n++) frames.push([n, ...labelsAt(n)]);
    for (const b of BUTTONS) {
      if (
        frames.some(
          ([, p, t]) => p?.buttons?.includes(b) || t?.buttons?.includes(b),
        )
      )
        lanes.push({ kind: "button", name: b, label: b.toUpperCase() });
    }
    for (const [name, label, pick] of SIGNALS) {
      if (
        frames.some(
          ([, p, t]) => (p && pick(p) != null) || (t && pick(t) != null),
        )
      )
        lanes.push({ kind: "signal", name, label, pick });
    }
    const LABEL = 96;
    const BUTTON_H = 14;
    const SIGNAL_H = 38;
    const TOP = 16;
    const height =
      TOP +
      lanes.reduce(
        (h, l) => h + (l.kind === "button" ? BUTTON_H : SIGNAL_H) + 4,
        0,
      ) +
      4;
    const dpr = window.devicePixelRatio || 1;
    canvas.style.height = `${Math.max(height, 60)}px`;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(Math.max(height, 60) * dpr);
    const g = canvas.getContext("2d");
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    const c = colors();
    const plotW = width - LABEL - 8;
    const x = (n) => LABEL + ((n - first) / (last - first)) * plotW;
    g.font = "11px Inter, system-ui, sans-serif";
    g.textBaseline = "middle";

    // Seconds along the top
    g.fillStyle = c.text;
    g.strokeStyle = c.grid;
    g.lineWidth = 1;
    for (let s = Math.ceil(first / fps()); s <= last / fps(); s++) {
      const px = x(s * fps());
      g.beginPath();
      g.moveTo(px, TOP - 4);
      g.lineTo(px, height);
      g.stroke();
      // Labels at least 44 px apart
      const perSecond = plotW / pred.span;
      const every = [1, 2, 5, 10, 30].find((n) => n * perSecond >= 44) ?? 60;
      if (s % every === 0 && px < width - 30)
        g.fillText(clockText(s).replace(/\.0$/, ""), px + 2, 7);
    }

    if (!chunk) {
      g.fillText("Loading…", LABEL, TOP + 12);
    } else if (!lanes.length) {
      g.fillText("No predictions here", LABEL, TOP + 12);
    }

    let y = TOP;
    for (const lane of lanes) {
      const h = lane.kind === "button" ? BUTTON_H : SIGNAL_H;
      g.fillStyle = c.text;
      g.fillText(lane.label, 4, y + h / 2);
      if (lane.kind === "button") {
        for (const [n, p, t] of frames) {
          const x0 = x(n - 0.5);
          const w = Math.max(1, x(n + 0.5) - x0);
          // Truth fills the lower half, the prediction's probability grows
          // up from the middle and a pressed prediction marks the top
          if (t?.valid !== false && t?.buttons?.includes(lane.name)) {
            g.fillStyle = c.truth;
            g.globalAlpha = 0.7;
            g.fillRect(x0, y + h / 2, w, h / 2);
            g.globalAlpha = 1;
          }
          if (p) {
            const prob = p.button_probs?.[lane.name] ?? 0;
            g.fillStyle = c.pred;
            g.globalAlpha = 0.35;
            g.fillRect(x0, y + (h / 2) * (1 - prob), w, (h / 2) * prob);
            g.globalAlpha = 1;
            if (p.buttons?.includes(lane.name)) {
              g.fillRect(x0, y, w, 2.5);
            }
          }
        }
      } else {
        // Scale: sticks -1..1, others by the largest value in view
        let scale = 1;
        const stickLane = /^(left|right)_/.test(lane.name);
        if (!stickLane) {
          scale = 0;
          for (const [, p, t] of frames) {
            for (const l of [p, t]) {
              const v = l && lane.pick(l);
              if (v != null) scale = Math.max(scale, Math.abs(v));
            }
          }
          scale = scale || 1;
        }
        const mid = y + h / 2;
        const py = (v) => mid - (v / scale) * (h / 2 - 1);
        g.strokeStyle = c.axis;
        g.beginPath();
        g.moveTo(LABEL, mid);
        g.lineTo(LABEL + plotW, mid);
        g.stroke();
        // Truth: filled to zero
        g.fillStyle = c.truth;
        g.globalAlpha = 0.4;
        for (const [n, , t] of frames) {
          const v = t && t.valid !== false ? lane.pick(t) : null;
          if (v == null) continue;
          const x0 = x(n - 0.5);
          const top = py(v);
          g.fillRect(
            x0,
            Math.min(top, mid),
            Math.max(1, x(n + 0.5) - x0),
            Math.abs(top - mid) || 1,
          );
        }
        g.globalAlpha = 1;
        // Prediction: a line
        g.strokeStyle = c.pred;
        g.lineWidth = 1.5;
        g.beginPath();
        let drawing = false;
        for (const [n, p] of frames) {
          const v = p ? lane.pick(p) : null;
          if (v == null) {
            drawing = false;
            continue;
          }
          if (drawing) g.lineTo(x(n), py(v));
          else g.moveTo(x(n), py(v));
          drawing = true;
        }
        g.stroke();
        g.lineWidth = 1;
        g.fillStyle = c.text;
        g.textAlign = "right";
        g.fillText(
          scale === 1 ? "±1" : `±${scale.toPrecision(2)}`,
          LABEL - 4,
          y + 7,
        );
        g.textAlign = "left";
      }
      y += h + 4;
    }

    // The playhead
    g.strokeStyle = c.now;
    g.lineWidth = 1.5;
    g.beginPath();
    g.moveTo(x(n0), TOP - 6);
    g.lineTo(x(n0), height);
    g.stroke();
    canvas.dataset.first = String(first);
    canvas.dataset.last = String(last);
    canvas.dataset.left = String(LABEL);
    canvas.dataset.plot = String(plotW);
  }

  // Click on the timeline to go there
  canvas.addEventListener("click", (event) => {
    if (!pred.run) return;
    const rect = canvas.getBoundingClientRect();
    const first = Number(canvas.dataset.first);
    const last = Number(canvas.dataset.last);
    const left = Number(canvas.dataset.left);
    const plot = Number(canvas.dataset.plot);
    const share = (event.clientX - rect.left - left) / plot;
    if (share < 0 || share > 1) return;
    player.go(Math.round(first + share * (last - first)));
  });

  new ResizeObserver(() => draw()).observe(canvas);

  // -------------------------------------------------------- agreement

  /** Ask for the agreement of the frames in view (or all), at most once a
   * second while playing */
  function scheduleAgreement() {
    if (!pred.run?.session) {
      $("p-agree").innerHTML =
        `<p class="panel-note">${pred.run ? "No controller recording for this video: predictions only." : "Open a prediction."}</p>`;
      $("p-agree-note").textContent = "";
      return;
    }
    clearTimeout(pred.agreeTimer);
    const wait = Math.max(250, 1000 - (performance.now() - pred.agreeAt));
    pred.agreeTimer = setTimeout(loadAgreement, wait);
  }

  async function loadAgreement() {
    const run = pred.run;
    if (!run?.session || !pred.shown) return;
    pred.agreeAt = performance.now();
    const all = $("p-agree-mode").value === "all" && !run.online;
    const half = Math.round((pred.span * fps()) / 2);
    const start = all ? 0 : Math.max(0, player.frame - half);
    const stop = all ? Math.max(1, frameCount()) : player.frame + half;
    let agreement;
    try {
      agreement = run.online
        ? (await api(`online/labels?${new URLSearchParams({ start, stop })}`))
            .agreement
        : await api(
            `agreement?${new URLSearchParams({ key: run.key, ckpt: run.checkpoint, start, stop })}`,
          );
    } catch (error) {
      $("p-agree").innerHTML =
        `<p class="notice">${escapeHtml(error.message)}</p>`;
      return;
    }
    if (pred.run !== run) return;
    renderAgreement(agreement, start, stop);
  }

  $("p-agree-mode").onchange = () => loadAgreement();

  function renderAgreement(a, start, stop) {
    $("p-agree-note").textContent =
      `${a.frames} frames · ${clockText(start / fps())}–${clockText(stop / fps())}`;
    const value = (v, digits = 2) => (v == null ? "–" : v.toFixed(digits));
    const level = (v) =>
      v == null
        ? ""
        : v >= 0.7
          ? "level-good"
          : v >= 0.4
            ? "level-warning"
            : "level-critical";
    const buttons = a.buttons
      .filter((b) => b.truth + b.pred > 0)
      .map(
        (b) =>
          `<tr><td>${b.name.toUpperCase()}</td><td class="num ${level(b.f1)}">${value(b.f1)}</td><td class="num">${b.truth}</td><td class="num">${b.pred}</td></tr>`,
      )
      .join("");
    const names = Object.fromEntries(SIGNALS.map(([n, l]) => [n, l]));
    // A camera turn without pairs: the session has no turn fit (so the
    // truth has no turn), or the model no turn head
    const truthTurn = pred.chunk?.truth?.some((l) => l?.camera_turn) ?? false;
    const missing = (name) =>
      !name.startsWith("turn")
        ? "–"
        : truthTurn
          ? "not predicted"
          : "no turn fit";
    const signals = a.signals
      .map(
        (s) =>
          `<tr><td>${escapeHtml(names[s.name] ?? s.name)}</td><td class="num ${level(s.r)}">${value(s.r)}</td><td class="num" colspan="2">${s.n ? `${s.n} frames` : missing(s.name)}</td></tr>`,
      )
      .join("");
    $("p-agree").innerHTML = `
      <table class="data-table p-agree-table">
        <thead><tr><th>Button</th><th>F1</th><th>Truth</th><th>Predicted</th></tr></thead>
        <tbody>${buttons || '<tr><td colspan="4" class="panel-note">No button pressed here.</td></tr>'}</tbody>
        <thead><tr><th>Signal</th><th>r</th><th colspan="2"></th></tr></thead>
        <tbody>${signals}</tbody>
      </table>`;
  }

  // -------------------------------------------------------------- routing

  /** The last view, for the app link after leaving or a reload */
  function rememberView(url) {
    document.querySelector('.app-nav [data-app="predictor"]').href = url;
    remember("view", url);
  }

  async function route(state) {
    // Reading the command's options takes seconds; the viewer does not wait
    const loading = pred.model === "policy" ? loadOnlineInfo() : loadInfo();
    loading.catch((error) => showRunError(error.message));
    showKind();
    poll();
    const watching = state.get("view") === "online";
    const was = pred.watching;
    pred.watching = watching;
    if (!watching) hideLive();
    await pollOnline();
    if (!pred.runs.length) await loadRuns();
    if (watching) {
      // Opened afresh: the run as it is now
      if (!was) pred.watchedId = null;
      rememberView(appUrl("predictor", { view: "online" }));
      followOnline();
      return;
    }
    const key = state.get("key");
    const ckpt = state.get("ckpt");
    if (key && ckpt) openRun(key, ckpt, Number(state.get("t")) || 0);
    else draw();
  }

  window.addEventListener("app-route", (event) => {
    const { app, state } = event.detail;
    const was = pred.shown;
    pred.shown = app === "predictor";
    player.enabled = pred.shown;
    if (pred.shown) route(state);
    else if (was) {
      clearTimeout(pred.pollTimer);
      clearTimeout(pred.agreeTimer);
      clearTimeout(pred.onlineTimer);
      player.pause();
      // The Studio's screen goes home with it
      hideLive();
      pred.watching = false;
    }
  });

  document.addEventListener("visibilitychange", () => {
    if (!pred.shown) return;
    if (document.hidden) player.pause();
    else {
      poll();
      pollOnline();
    }
  });

  // Words the script writes itself
  window.addEventListener("lang-change", () => {
    if (pred.model === "policy") $("p-run").textContent = t("po.start");
    renderCaps();
    renderOnline();
  });

  document.querySelector('.app-nav [data-app="predictor"]').href = storedView(
    "predictor",
    remembered("view"),
  );
})();
