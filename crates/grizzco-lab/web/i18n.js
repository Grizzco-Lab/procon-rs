// Page language: a dictionary per language and `t(key, values)` to look a
// string up. English is here; each other language has a file of its own that
// adds its table to I18N (i18n-zh.js: Simplified Chinese). Loaded before the
// apps' scripts. The shell (app names, status chips, the View menu), the
// Cuttlefish app (with its views) and the Pipeline app are translated; other
// apps can adopt it key by key.
//
// Elements carry their key in data-i18n (text), data-i18n-html (markup of
// our own), data-i18n-placeholder, data-i18n-title or data-i18n-aria-label;
// their English text in the page stays as it is until a language is applied.
// A value is a string with {name} placeholders, or a function of the values
// (for plurals). A key missing from a language falls back to English.
//
// The language is chosen in the View menu (buttons with data-pick-lang) or
// switched by the button next to it (data-toggle-lang),
// remembered in localStorage `procon-lang`, and defaults to the browser's.
// Changing it fires a `lang-change` event on window, so scripts redraw what
// they drew themselves.
"use strict";

/** Tables by language code */
const I18N = {
  en: {
    "lang.name": "English",
    "view.language": "Language",
    "view.switchTo": "Switch to {lang}",

    // The shell: the product's name, app names, the status chips and the
    // View menu
    "shell.title": "Grizzco Lab",
    "shell.brand": "Grizzco <b>Lab</b>",
    "shell.apps": "Apps",
    "shell.status": "Status",
    "app.studio": "Studio",
    "app.inspect": "Inkspector",
    "app.cuttlefish": "Cuttlefish",
    "app.vision": "Vision",
    "app.predictor": "Predictor",
    "app.pipeline": "Pipeline",
    // What each app is for: tooltips, the expanded rail and the guide
    "app.studio.sub": "data capture",
    "app.inspect.sub": "frame-by-frame inspection and labeling",
    "app.cuttlefish.sub":
      "VOD review / cross-language slang translation / Overfishing Pedia",
    "app.vision.sub": "vision and 3D reconstruction",
    "app.predictor.sub": "controller action prediction",
    "app.pipeline.sub": "the GPUs and the experiment queue",
    // The guide: how the apps fit together
    "guide.open": "How it fits together",
    "guide.title": "How it fits together",
    "guide.lede":
      "Grizzco Lab is six apps and one pipeline: record your play with the controller's input, check it frame by frame, learn from it, teach models to see the game and to play it, and follow their training as it runs.",
    "guide.close": "Close",
    "guide.out.studio": "recordings",
    "guide.out.inspect": "labels",
    "guide.out.cuttlefish": "reviews and knowledge",
    "guide.out.vision": "detections",
    "guide.out.predictor": "IDM predictions",
    "guide.out.pipeline": "checkpoints",
    "guide.loop":
      "IDM predictions go back to the Inkspector, next to the labels, to check them frame by frame",
    "guide.again": "Open this again with ? or from the View menu.",
    "guide.done": "Got it",
    "chip.connecting": "Connecting…",
    "chip.offline": "Dashboard offline, reconnecting…",
    "chip.proxy": "Proxy",
    "chip.noProxy": "No proxy",
    "chip.proxyTitle":
      "Proxy connected: {address}, host clock {offset} vs proxy",
    "chip.noProxyTitle": "Proxy not connected: {address}",
    "chip.controller": "Controller",
    "chip.noController": "No controller",
    "chip.controllerTitle": "Controller input at {rate} Hz",
    "chip.noControllerTitle": "No controller input",
    "chip.latencyUnknown": "Latency –",
    "chip.latencyUnknownTitle":
      "Proxy latency unknown: no reports forwarded in the last half second",
    "chip.latencyOff": "Proxy latency unknown",
    "chip.latencyTitle":
      "Proxy latency. From the proxy reading a report to the Switch taking it: mean {mean} ms, max {max} ms over the last half second. The controller's own USB polling (up to 8 ms) comes on top, as it would without the proxy.",
    "chip.rec.idle": "Not recording",
    "chip.rec.recording": "REC {time}",
    "chip.rec.paused": "Paused {time}",
    "chip.rec.idleTitle": "Idle: recording is run from the Studio app",
    "chip.rec.recordingTitle":
      "Recording: recording is run from the Studio app",
    "chip.rec.pausedTitle": "Paused: recording is run from the Studio app",
    // The Studio's capture card chip
    "chip.capture.ok": "Capture card",
    "chip.capture.lost": "Card: {n} lost",
    "chip.capture.title": "Capture card {input}, read by the lab itself",
    "chip.capture.since":
      "Since the reader started at {time}: {frames} frames, {corrupted} skipped (corrupted or short), {dropped} dropped by its driver",
    "chip.capture.recording":
      "Recording {file}: {frames} frames, {corrupted} skipped, {dropped} dropped",
    "chip.capture.idle": "Not recording",
    "chip.capture.note":
      "Neither kind reaches a recording: the frame before stands in for each, so the video keeps its timing.",
    "view.name": "View",
    "view.theme": "Theme",
    "view.theme.studio": "Studio",
    "view.theme.joy": "Joy",
    "view.theme.telemetry": "Telemetry",
    "view.theme.salmon": "Salmon Run",
    "view.layout": "Layout",
    "view.layout.auto": "Auto",
    "view.layout.autoTitle":
      "Fit the window: panels side by side on wide screens",
    "view.layout.phone": "Phone",
    "view.layout.phoneTitle": "Single-column phone layout",
    "view.apps": "Apps",
    "view.apps.side": "Side rail",
    "view.apps.sideTitle":
      "Apps in a left rail; drag them, or Alt+arrows on one, to reorder",
    "view.apps.top": "Top bar",
    "view.apps.topTitle":
      "Apps in the top bar; drag them, or Alt+arrows on one, to reorder",
    "view.rail": "Rail",
    "view.rail.compact": "Compact",
    "view.rail.compactTitle":
      "Icons only; names and what each app is for show as tooltips",
    "view.rail.expanded": "Expanded",
    "view.rail.expandedTitle": "Icons with their names and what each is for",
    "view.guide": "Guide",
    "view.rail.expand": "Expand the rail: icons with names",
    "view.rail.collapse": "Collapse the rail: icons only",

    // The shared video player (player.js)
    "player.play": "▶ Play",
    "player.pause": "❚❚ Pause",
    "player.frameBack": "‹ Frame",
    "player.frameNext": "Frame ›",
    "player.speed": "Playback speed",
    "player.sound": "Sound",
    "player.soundTitle": "Play the sound with the frames",
    "player.overlay": "Overlay",
    "player.overlay.full": "Overlay: Full",
    "player.overlay.minimal": "Overlay: Minimal",
    "player.overlay.none": "Overlay: None",
    "player.goto": "Go to…",
    "player.gotoAsk": "Frame number, or time as 12.5s or 1:02.5",
    "player.frame": "frame",
    "player.prediction": "prediction",
    "player.noReports": "No controller report",
    "player.cannotPlay": "This browser cannot play the video",
    "player.strip.frames": "±{n} frames, updated while paused",
    "player.strip.seconds":
      "±{span} s around the playhead, updated while paused",

    // Salmon Run stages and Gungee's community maps (stages.js)
    "stage.pick": "Stage",
    "stage.pickTitle": "The Salmon Run stage, for the links to Gungee's maps",
    "stage.unknown": "Stage…",
    "stage.tide": "Tide",
    "stage.tide.Low": "Low tide",
    "stage.tide.Mid": "Normal",
    "stage.tide.High": "High tide",
    "stage.mapAlt": "{stage} from above, by Gungee",
    "stage.credit":
      "Map by <b>Gungee</b>, from his community tools at {link}. Thank you, Gungee! For reference only: tracks are not placed on it yet.",
    "stage.map": "Map by Gungee:",
    "stage.map2d":
      "Gungee's 2D stage map (salmon-learn-nw.gungee.jp), in a new tab",
    "stage.map3d":
      "Gungee's 3D stage map (salmon-learn-nw.gungee.jp), in a new tab",

    // Vision: the Tracks panel
    "v.tracks": "Tracks",
    "v.tracks.canvas": "Tracks over the frame",
    "v.tracks.track": "Track",
    "v.tracks.class": "Class",
    "v.tracks.frames": "Frames",
    "v.tracks.first": "First",
    "v.tracks.last": "Last",
    "v.tracks.window":
      "Trails ±{s} s ({frames} frames) around the playhead, fading with time; select a track for its whole path",
    "v.tracks.focus":
      "#{id} {label}: {n} boxes, frames {first}–{last}, over frame {middle}; the dashed box is where it is at the playhead",
    "v.tracks.none": "No tracks: run with Track on.",
    "v.tracks.empty": "No results for this segment yet: run a detection.",
    "v.tracks.cropTitle": "Frame {n}: go there",
    "v.tracks.limit":
      "Trails are positions on the screen, and the camera keeps turning, so they are not places on the stage. Map positions need camera localisation, which is planned.",

    // Vision: the Salmon Run detector (AgentZero's service)
    "v.det.model": "Salmon Run detector (AgentZero)",
    "v.det.checking": "Checking the detector…",
    "v.det.ready": "Detector ready",
    "v.det.busy": "Detector busy with another request",
    "v.det.down": "Detector not running",
    "v.det.downNote":
      "Nothing answers at {url}. Start it with <code>{command}</code>{how}.",
    "v.det.downHow": ", or press Start detector",
    "v.det.start": "Start detector",
    "v.det.check": "Check again",
    "v.det.starting": "Starting the detector; it loads its model…",
    "v.det.exited":
      "The detector {state}; see the lab's log. It needs a trained checkpoint: <code>agentzero-detect train</code> writes <code>runs/detect/best</code>.",
    "v.det.timeout": "The detector did not answer within two minutes.",
    "v.det.checkpoint": "Checkpoint",
    "v.det.map50": "mAP50",
    "v.det.map50Value": "{map50} on held-out frames (epoch {epoch})",
    "v.det.trained": "Trained on",
    "v.det.trainedValue": "{train} labeled frames, {val} held out",
    "v.det.saved": "Saved",
    "v.det.device": "Device",
    "v.det.notLoaded": "not loaded yet",
    "v.det.gpuFree": "GPU {gib} GiB free",
    "v.det.classes": "Classes",
    "v.det.weak":
      "This model is weak: {labeled} of about {target} frames are labeled. Expect missed and wrong boxes, and check every one in the Label mode before it counts.",
    "v.det.progress": "Labeling progress: {labeled} / {target} frames",
    "v.det.progressTitle":
      "Frames labeled by people, per class and segment (what agentzero-detect status counts)",
    "v.det.needsService": "Start the detector first.",
    "v.det.reason": "{device} because {reason}",

    // Cuttlefish: the views
    "cf.name": "Cuttlefish",
    "cf.tab.reviews": "Reviews",
    "cf.tab.reviewsNote":
      "Ask Cuttlefish about your play, open a video, browse your reviews",
    "cf.tab.translate": "Translate",
    "cf.tab.translateNote":
      "Jargon and callouts across languages, in the names each community uses; a term shows its glossary entry",
    "cf.tab.knowledge": "Knowledge",
    "cf.tab.knowledgeNote":
      "What Cuttlefish knows: import material, documents, glossary, assets, search",
    "cf.tab.pedia": "Pedia",
    "cf.tab.pediaNote":
      "Overfishing Pedia: Salmon Run terms, techniques and slang, with what the community says about each",
    "cf.loading": "Loading…",

    // Overfishing Pedia (pedia.js)
    "pedia.title": "Overfishing Pedia",
    "pedia.lede": ({ n, comments, vods }) =>
      `${n} Salmon Run terms from the glossary, with slang in every language and what ${comments} #vod-review comments on ${vods} VODs say about them.`,
    "pedia.random": "Surprise me",
    "pedia.search": "Search in any language, slang included…",
    "pedia.searchLabel": "Search the Pedia",
    "pedia.era": "Game",
    "pedia.anyGame": "Any game",
    "pedia.sourceLabel": "Source",
    "pedia.anySource": "All",
    "pedia.orderLabel": "Order",
    "pedia.az": "A–Z",
    "pedia.talk": "Most discussed",
    "pedia.sections": "Sections",
    "pedia.all": "All",
    "pedia.facet.official": "Official",
    "pedia.facet.officialTitle": "Names from the game",
    "pedia.facet.community": "Community",
    "pedia.facet.communityTitle":
      "Slang or a concept the community uses, from its texts",
    "pedia.facet.user": "Yours",
    "pedia.facet.userTitle": "Taught or edited by you",
    "pedia.mentions": ({ n }) => (n === 1 ? "1 mention" : `${n} mentions`),
    "pedia.mentionsTitle": ({ n }) =>
      n === 1
        ? "1 #vod-review comment uses it"
        : `${n} #vod-review comments use it`,
    "pedia.section.movement": "Movement techniques",
    "pedia.section.movementNote":
      "cancels, strafes and rolls: moving like a pro",
    "pedia.section.bosses": "Bosses & Salmonids",
    "pedia.section.bossesNote":
      "Boss and lesser Salmonids, their parts and attacks, and how to kill them",
    "pedia.section.kings": "King Salmonids",
    "pedia.section.kingsNote": "what waits in the Xtrawave",
    "pedia.section.events": "Special events & tides",
    "pedia.section.eventsNote": "known occurrences and the water level",
    "pedia.section.eggs": "Egg flow",
    "pedia.section.eggsNote": "Golden Eggs and how they reach the basket",
    "pedia.section.strategy": "Roles & strategy",
    "pedia.section.strategyNote": "team play, positions and wave plans",
    "pedia.section.weapons": "Weapons",
    "pedia.section.weaponsNote":
      "Grizzco and rental weapons, specials and subs, and their tricks",
    "pedia.section.stages": "Stages",
    "pedia.section.stagesNote": "where the shifts happen",
    "pedia.section.other": "Modes, ranks & mechanics",
    "pedia.section.otherNote": "everything else worth knowing",
    "pedia.kind.boss": "boss",
    "pedia.kind.part": "part",
    "pedia.kind.attack": "attack",
    "pedia.kind.technique": "technique",
    "pedia.kind.mechanic": "mechanic",
    "pedia.kind.event": "event",
    "pedia.kind.tide": "tide",
    "pedia.kind.stage": "stage",
    "pedia.kind.weapon": "weapon",
    "pedia.kind.special": "special",
    "pedia.kind.sub": "sub",
    "pedia.kind.role": "role",
    "pedia.kind.category": "category",
    "pedia.kind.callout": "callout",
    "pedia.kind.mode": "mode",
    "pedia.kind.salmon-run": "Salmon Run",
    "pedia.kind.title": "title",
    "pedia.failed": "The Pedia did not load: {error}",
    "pedia.noMatch": "No entry matches. Try another name, or fewer filters.",
    "pedia.hidden": ({ n }) =>
      n === 1
        ? "1 entry matches, hidden by the section or filters."
        : `${n} entries match, hidden by the section or filters.`,
    "pedia.showAll": "Show them",
    "pedia.matched": "as {name}",
    "pedia.noEntry": "No entry {id}: {error}",
    "pedia.rejected":
      "{name} was flagged as wrong and left the glossary; it will not be suggested again.",
    "pedia.editFailed": "Not saved: {error}",
    "pedia.outOfScope": "outside the Pedia's sections",
    "pedia.noDefinition":
      "No definition yet. Write one with Edit: it helps Cuttlefish too.",
    "pedia.editedByYou": "Edited by you; imports never change it.",
    "pedia.reset": "Back to the glossary's",
    "pedia.ask": "Ask Cuttlefish about this",
    "pedia.askQuestion": ({ name }) =>
      `Explain ${name} in Salmon Run for a new player: what it is, when it matters, and the usual mistakes.`,
    "pedia.edit": "Edit",
    "pedia.addNote": "Add a note",
    "pedia.noteQuestion": ({ name }) =>
      `What should a player know about ${name}?`,
    "pedia.flag": "Flag as wrong",
    "pedia.flagAsk":
      "Flag {name} as wrong? It leaves the glossary and is never suggested again (Undo is offered).",
    "pedia.flagHint":
      "What is wrong? Write it as it should be: your version replaces the glossary's, here and for Cuttlefish.",
    "pedia.definition": "Definition",
    "pedia.kindLabel": "Kind",
    "pedia.wild": "In the wild",
    "pedia.wildNote": ({ n }) =>
      n === 1
        ? "1 #vod-review comment uses it; the best are quoted"
        : `${n} #vod-review comments use it; the best are quoted`,
    "pedia.wildNone": "No #vod-review comment uses it yet.",
    "pedia.more": "Show more",
    "pedia.context": "Conversation",
    "pedia.contextTitle": "The comment in full, with its replies",
    "pedia.openAt": "Open the review at {at}",
    "pedia.ofVideo": "{at} of the video",
    "pedia.notes": "Expert notes",
    "pedia.noteBy": "{author}, {date} ({era})",
    "pedia.facts": "Game data",
    "pedia.factsNote": "Exact numbers from the game's files",
    "pedia.allFacts": ({ n }) => `Show all ${n}`,
    "pedia.fact.weapon": "Salmon Run weapon",
    "pedia.fact.special": "Salmon Run special",
    "pedia.fact.salmonid": "Salmonid",
    "pedia.fact.stage": "Salmon Run stage",
    "pedia.fact.level": "Hazard level {hazard}%",
    "pedia.fact.grizzco": "Grizzco weapon",
    "pedia.fact.version": "v{version}",
    "pedia.fact.events": "Eggstra Work: {list}",
    "pedia.stat.of": "{stat} ({group})",
    "pedia.stat.downTo": "down to",
    "pedia.stat.hpCoef": "{name} HP ×",
    "pedia.hpFrom": "HP from {source}; Lean's data has none.",
    "pedia.group.direct": "direct hit",
    "pedia.stat.damage": "Damage",
    "pedia.stat.damage_radius": "Damage radius",
    "pedia.stat.paint_radius": "Paint radius",
    "pedia.stat.range": "Range",
    "pedia.stat.ink": "Ink per use",
    "pedia.stat.ink_per_frame": "Ink per frame",
    "pedia.stat.fire_interval": "Time between shots",
    "pedia.stat.full_charge": "Full charge",
    "pedia.stat.detonation": "Explodes after",
    "pedia.stat.duration": "Duration",
    "pedia.stat.ink_recovery": "Ink refill delay",
    "pedia.stat.category": "Kind",
    "pedia.stat.hp": "HP",
    "pedia.stat.at_once": "At most at once",
    "pedia.stat.eggs_per_hit": "Power Eggs per hit",
    "pedia.stat.eggs_on_kill": "Power Eggs when defeated",
    "pedia.stat.big_run": "Big Run stage",
    "pedia.group.after_roll": "after a dodge roll",
    "pedia.group.dodge_roll": "dodge roll",
    "pedia.group.vertical_slash": "vertical slash",
    "pedia.group.horizontal_slash": "horizontal slash",
    "pedia.group.vertical_projectile": "vertical slash's projectile",
    "pedia.group.horizontal_projectile": "horizontal slash's projectile",
    "pedia.group.charged_swing": "charged slash",
    "pedia.group.vertical_flick": "vertical flick",
    "pedia.group.horizontal_flick": "horizontal flick",
    "pedia.group.flick": "flick",
    "pedia.group.canopy": "canopy",
    "pedia.group.pellets": "pellets",
    "pedia.group.cannon": "cannon",
    "pedia.group.explosion": "explosion",
    "pedia.group.laser": "laser",
    "pedia.group.jet": "jet",
    "pedia.group.shockwave": "shockwave",
    "pedia.group.turret": "turret",
    "pedia.group.contact": "contact",
    "pedia.part.bomb": "bomb",
    "pedia.cat.Rare": "Boss Salmonid",
    "pedia.cat.Boss": "King Salmonid",
    "pedia.cat.Zako": "Lesser Salmonid",
    "pedia.cat.EventRare": "Appears in known occurrences",
    "pedia.cat.Other": "Other",
    "pedia.yes": "Yes",
    "pedia.no": "No",
    "pedia.unit.percent": "%",
    "pedia.unit.percentPerFrame": "% per frame",
    "pedia.unit.frames": "frames ({s} s)",
    "pedia.unit.units": "game units",
    "pedia.table.falloff": "Damage by distance",
    "pedia.table.upTo": "Up to (game units)",
    "pedia.table.hpCoef": "HP multiplier by hazard level",
    "pedia.table.hazard": "Hazard level",
    "pedia.table.coef": "HP ×",
    "pedia.hit.one": "One hit defeats",
    "pedia.hit.more": "More hits",
    "pedia.hit.hits": "Hits to defeat",
    "pedia.hit.bomb": "{name}'s bomb",
    "pedia.hit.hp": "HP {hp}",
    "pedia.hit.note":
      "At the most damage per hit, against HP from {source}; whether a hit can reach them is not counted.",
    "pedia.raw": ({ n }) => `Raw parameters (${n})`,
    "pedia.raw.key": "Internal key",
    "pedia.raw.versus": "Battle form",
    "pedia.raw.note":
      "As the game files have them: damage ×10, ink as a fraction of the tank, times in frames.",
    "pedia.questions": "Deep questions",
    "pedia.questionsNote": "from the question bank; ask Cuttlefish any of them",
    "pedia.answered": "Answered",
    "pedia.askThis": "Ask",
    "pedia.names": "Official names",
    "pedia.noNames": "No official name: this is player jargon.",
    "pedia.slang": "Slang",
    "pedia.noSlang": "No slang known yet. Add what players call it.",
    "pedia.alias.remove": "Remove this alias",
    "pedia.alias.removeAsk":
      "Remove the alias {text}? A suggested one is rejected, so it is not suggested again.",
    "pedia.related": "Related",
    "pedia.noRelated": "Not linked to other terms yet.",
    "pedia.relSet": "Link to a term",
    "pedia.relChange": "Change the link",
    "pedia.relKind": "How",
    "pedia.relNone": "No link",
    "pedia.rel.part-of": "Part of",
    "pedia.rel.kind-of": "A kind of",
    "pedia.rel.related-to": "Related to",
    "pedia.relIn.part-of": "Parts & attacks",
    "pedia.relIn.kind-of": "Kinds",
    "pedia.relIn.related-to": "Related terms",

    // The source popover (source.js)
    "src.title": "Source",
    "src.close": "Close",
    "src.openAt": "Open the review at {at}",
    "src.ownVod": "on their own VOD of {date}",
    "src.onVod": "on {poster}'s VOD of {date}",
    "src.replyingTo": "Replying to",
    "src.replies": ({ n }) => (n === 1 ? "1 reply" : `${n} replies`),
    "src.discord": "Open in Discord ↗",
    "src.original": "Open the original ↗",
    "src.noText": "The text is not in the knowledge folder here.",
    "src.failed": "Could not load the source: {error}",

    // Library
    "cf.open.title": "Open a video",
    "cf.open.note":
      "each review is a folder: notes, and the video when it lives there",
    "cf.open.session": "Recorded session",
    "cf.open.sessionLabel": "Session",
    "cf.open.segmentLabel": "Segment",
    "cf.open.open": "Open",
    "cf.open.file": "Video file on this machine",
    "cf.open.filePlaceholder": "/path/to/video.mp4",
    "cf.open.from": "From",
    "cf.open.to": "to",
    "cf.open.download": "Download and open",
    "cf.open.noSessions": "No sessions",
    "cf.open.noSessionsBecause": "No sessions: {error}",
    "cf.open.badTime":
      'Cannot read the time "{text}"; write 90, 1:30 or 1:02.5',
    "cf.open.cannot": "Cannot open the review {id}: {error}",
    "cf.download.done": "done",
    "cf.download.failed": "failed",
    "cf.reviews.video": "Video",
    "cf.reviews.comments": "Comments",
    "cf.reviews.changed": "Changed",
    "cf.reviews.count": "{n} in {dir}",
    "cf.reviews.none": "No reviews yet: open a video and comment on it.",
    "cf.reviews.fileIn": "{file} in the review",
    "cf.reviews.deleteAsk": "Delete the review {id}? Its folder is removed.",
    "cf.reviews.deleteWithVideo":
      "Delete the review {id}? Its folder is removed, including the video {file}.",
    "cf.delete": "Delete",
    "cf.kind.session": "Session",
    "cf.kind.file": "File",
    "cf.kind.youtube": "YouTube",
    "cf.kind.chat": "Chat",
    "cf.reviews.noVideo": "no video yet",
    "cf.reviews.noneFiltered": "No reviews of this kind.",
    "cf.reviews.community": "from #vod-review",
    "cf.reviews.eggstra": "probably Eggstra Work #{n}",
    "cf.filter.label": "Which reviews",
    "cf.filter.all": "All",
    "cf.filter.mine": "Mine",
    "cf.filter.community": "Community",
    "cf.era.S2": "Splatoon 2 era",
    "cf.era.S3": "Splatoon 3 era",
    "cf.source.discord": "The message on Discord",
    "cf.reviews.messages": ({ n }) => (n === 1 ? "1 message" : `${n} messages`),
    "cf.range.end": "end",
    "cf.youtube.lookingUp": "looking up the title…",
    "cf.youtube.open": "Open the original on YouTube at this time",
    "cf.youtube.openStart":
      "Open the original on YouTube where the range starts",
    "cf.youtube.range": "range {range} of the original",

    // Player
    "cf.back": "← Reviews",
    "cf.backTitle": "Back to the reviews",
    "cf.review": "Review",
    "cf.copy": "Copy into review",
    "cf.copyTitle":
      "Copy the video file into this review's folder, so the review keeps it",
    "cf.copy.running": "Copying…",
    "cf.copy.failed": "Not copied: {error}",
    "cf.tools": "Drawing",
    "cf.tool.select": "Select",
    "cf.tool.rect": "Rectangle",
    "cf.tool.ellipse": "Ellipse",
    "cf.tool.arrow": "Arrow",
    "cf.tool.freehand": "Freehand",
    "cf.swatch": "Draw in {color}",
    "cf.deleteShape": "Delete shape",
    "cf.deleteShapeTitle": "Delete the selected shape (Del)",
    "cf.addComment": "+ Comment",
    "cf.danmaku": "Danmaku",
    "cf.danmakuTitle": "Show comments over the video while it plays (D)",
    "cf.danmakuStyle": "Danmaku style",
    "cf.danmakuFloat": "Float in the corner",
    "cf.danmakuSlide": "Slide across",
    "cf.strip": "Neighbours",
    "cf.strip.every": "every",

    // Comments
    "cf.comments": "Comments",
    "cf.comments.count": "{n} · click one to go to its time",
    "cf.comments.none": "none yet",
    "cf.author.you": "You",
    "cf.drawings": ({ n }) => (n === 1 ? "1 drawing" : `${n} drawings`),
    "cf.noText": "No text",
    "cf.comment.delete": "Delete comment",
    "cf.comment.deleteAsk": "Delete this comment and its drawings?",
    "cf.comment.placeholder":
      "What happens here? Draw on the frame to point at it.",
    "cf.comment.endHere": "Set end here",
    "cf.comment.endHereTitle": "End the comment's range at the current time",
    "cf.comment.noEnd": "No end",
    "cf.comment.moveHere": "Move here",
    "cf.comment.moveHereTitle": "Move the comment to the current time",
    "cf.comment.goPast": "Go past the comment's time first",
    "cf.done": "Done",
    "cf.cancel": "Cancel",
    "cf.keys":
      "<kbd>Space</kbd> play/pause · <kbd>←</kbd> <kbd>→</kbd> one frame (<kbd>Shift</kbd> one second) · <kbd>C</kbd> comment here · <kbd>V</kbd> <kbd>R</kbd> <kbd>E</kbd> <kbd>A</kbd> <kbd>F</kbd> tools · <kbd>Del</kbd> delete shape · <kbd>D</kbd> danmaku · drawing on a paused frame without a comment open starts one.",

    // Notes
    "cf.notes": "Notes",
    "cf.notes.count": ({ n }) => (n === 1 ? "1 note" : `${n} notes`),
    "cf.notes.none": "on the whole video",
    "cf.notes.new": "New note",
    "cf.notes.placeholder":
      "About the whole video: what went well, what to change, or a rant",
    "cf.notes.hint": "Notes belong to no time or drawing.",
    "cf.notes.add": "Add note",
    "cf.notes.edit": "Edit",
    "cf.notes.save": "Save",
    "cf.notes.edited": "edited",
    "cf.notes.delete": "Delete note",
    "cf.notes.deleteAsk": "Delete this note?",
    "cf.notes.unplaced": "Not placed yet:",
    "cf.notes.unplacedTimer": ({ wave, s }) =>
      `wave ${wave}, ${s} s left on the timer; placed once the video's HUD is read`,
    "cf.notes.unplacedWave": ({ wave }) =>
      `wave ${wave}; placed once the video's HUD is read`,

    // Saving
    "cf.save.new": "not saved yet: comment or ask to start the review",
    "cf.save.dirty": "unsaved changes",
    "cf.save.saving": "saving…",
    "cf.save.saved": "saved as {id}",
    "cf.save.error": "not saved: {error}",

    // Ask Cuttlefish: the chat
    "cf.ask": "Ask Cuttlefish",
    "cf.ask.note": "a veteran's eye on your play",
    "cf.ask.moment": "Comment on this moment",
    "cf.ask.momentMessage":
      "Comment on this moment: what matters most here, and what would you do next time?",
    "cf.ask.from": "from",
    "cf.ask.to": "to",
    "cf.ask.fromLabel": "Range start",
    "cf.ask.toLabel": "Range end",
    "cf.ask.badRange": "The range must end after it starts",
    "cf.ask.longRange": "A range is at most {max} s",
    "cf.ask.fpsLabel": "Frames per second of a range",
    "cf.ask.heightLabel": "Frame height of a range (never above the video's)",
    "cf.ask.watching": "Cuttlefish is watching…",
    "cf.chat.entryNote":
      "a question about your play; each conversation is a review, with or without a video",
    "cf.chat.messageLabel": "Message to Cuttlefish",
    "cf.chat.send": "Send",
    "cf.chat.keys": "Enter sends · Shift+Enter for a new line",
    "cf.chat.thinking": "Cuttlefish is thinking… (up to a minute)",
    "cf.chat.noKey":
      "No model backend where the lab runs, so Cuttlefish cannot answer yet: export ANTHROPIC_API_KEY, or install the Claude Code CLI and log in, before starting the lab. Your messages, comments and drawings are saved as usual.",
    "cf.chat.failed": "Could not reach Cuttlefish: {error}",
    "cf.chat.error": "Cuttlefish could not answer: {error}",
    "cf.chat.empty":
      "Ask anything about Salmon Run or about the moment you are watching. Answers cite the knowledge; times in them seek the video. Jargon and callouts are translated in the Translate view.",
    "cf.chat.try": "Try one:",
    // Questions about one's play: the chips and the rotating placeholder
    "cf.chat.examples": [
      "Where could I have played this wave better?",
      "Why did I go down here?",
      "Which boss should I have taken first here?",
      "Where did the egg flow break?",
      "Was my positioning right?",
      "When should I leave the basket to kill a Stinger?",
    ],
    // With a video, next to "Comment on this moment"
    "cf.chat.rangeExample": "What goes wrong in this range?",
    "cf.chat.with": "With the video:",
    "cf.chat.ctxMoment": "this moment",
    "cf.chat.ctxRange": "a range",
    "cf.chat.ctxNone": "no frames",
    "cf.cost.one": "≈ {tokens} image tokens ({frames} frames)",
    "cf.cost.two": "≈ {scout} + up to {answer} image tokens (two passes)",
    "cf.cost.title":
      "Images the model reads, about width × height / 750 tokens each. A range over 20 s takes two passes: a sparse overview picks the key moments, then sharper frames around them.",
    "cf.chat.at": "at {time}",
    "cf.chat.sources": "Sources",
    "cf.chat.experts": "Expert comments given",
    "cf.chat.deepTitle": "A deep question of the bank: {category}",
    "cf.chat.memo": "Correct / add to memory",
    "cf.chat.memoTitle":
      "Edit this answer into the correct explanation and save it as an expert note; Cuttlefish follows notes over every other source",
    "cf.chat.memoSaved":
      "Saved as the expert note {id}; Cuttlefish uses it from now on",

    // The expert note editor
    "note.title": "Expert note",
    "note.editTitle": "Expert note {id}",
    "note.hint":
      "edit the answer into the correct explanation; Cuttlefish will follow it",
    "note.question": "Question",
    "note.body": "The correct explanation (Markdown)",
    "note.tags": "Tags, comma-separated",
    "note.terms": "Glossary terms, comma-separated ids",
    "note.era": "Era",
    "note.version": "Game version (optional)",
    "note.save": "Save as note",
    "note.saving": "Saving and indexing…",
    "note.failed": "Not saved: {error}",
    "note.from": "From: {from}",
    "cf.chat.commentsAdded": ({ n }) =>
      n === 1 ? "1 comment added" : `${n} comments added`,
    "cf.chat.seek": "Go to {time}",
    "cf.attach.title": "Attach a video",
    "cf.attach.note":
      "optional: Cuttlefish can then look at the moments you ask about",
    "cf.attach.attach": "Attach",
    "cf.attach.download": "Download and attach",
    "cf.attach.failed": "Not attached: {error}",

    // Translate
    "tr.empty":
      "Type a term (Steelhead, コジャケ, 熊刷) for its glossary entry in every language, or a sentence full of jargon to translate with the names the other community uses. Answers are kept here.",
    "tr.count": "{n} kept in {file}",
    "tr.clear": "Clear history",
    "tr.clearAsk": "Clear the translation history? {file} is removed.",
    "tr.placeholder":
      "A term (Steelhead, コジャケ, 熊刷) or a sentence full of jargon…",
    "tr.textLabel": "Text to translate",
    "tr.into": "into",
    "tr.targetLabel": "Target language",
    "tr.exampleInto": "Translate into {lang}",
    "tr.working": "Translating…",
    "tr.failed": "Not translated: {error}",
    "tr.noKey":
      "No model backend where the lab runs, so only the glossary answers: a term's entry and names, the terms a sentence uses. Export ANTHROPIC_API_KEY, or install the Claude Code CLI and log in, before starting the lab to translate sentences.",
    "tr.noKeyTranslation":
      "Translating this needs a model backend where the lab runs (ANTHROPIC_API_KEY or the Claude Code CLI).",
    "tr.copy": "Copy",
    "tr.copied": "Copied",
    "tr.entry": "Glossary entry",
    "tr.terms": ({ n }) =>
      n === 1 ? "1 glossary term used" : `${n} glossary terms used`,
    "tr.noTerms": "No glossary term in it.",
    "tr.noName": "no {lang} name in the glossary",
    "tr.teach": "Teach a word",
    "tr.teachHint":
      "Select a slang word in the text above (or type it) and pick the term it means.",

    // Slang: aliases taught, suggestions to review
    "slang.title": "Slang",
    "slang.toggleTitle":
      "Slang players use: taught by you or suggested from the knowledge base",
    "slang.label": "{lang} slang",
    "slang.note": ({ n, file }) =>
      `${n === 1 ? "1 alias" : `${n} aliases`} of yours in ${file}; imports never change it.`,
    "slang.suggest": "Suggest slang from the knowledge base",
    "slang.planning": "Counting what there is to read…",
    "slang.plan": ({ documents, chars, total, batches, charsRun }) =>
      `${documents} community ${documents === 1 ? "document has" : "documents have"} ${chars} characters not read yet: ${total} ${total === 1 ? "batch" : "batches"}. This run reads ${batches} (${charsRun} characters), one model request each.`,
    "slang.nothing":
      "Every community document has been read; new imports bring more.",
    "slang.batches": "Batches this run",
    "slang.run": "Run",
    "slang.running": "Reading batch {done} of {total}…",
    "slang.done": "Done: {line}",
    "slang.failed": "The run stopped: {error}",
    "slang.noBackend":
      "Suggestions need a model backend where the lab runs (ANTHROPIC_API_KEY or the Claude Code CLI).",
    "slang.pending": "Waiting for review",
    "slang.noPending": "No suggestions waiting.",
    "slang.taught": "Taught and approved",
    "slang.noTaught":
      "Nothing taught yet: add an alias on a term, or teach a word of a sentence.",
    "slang.approve": "Approve",
    "slang.reject": "Reject",
    "slang.edit": "Edit",
    "slang.delete": "Delete",
    "slang.deleteAsk": "Delete the alias {text}?",
    "slang.confidence": "{p}% sure",
    "slang.from": "in {doc}",
    "slang.termGone": "its term is gone from the glossary",
    "slang.source.user": "taught",
    "slang.source.seed": "seed",
    "slang.source.suggested": "suggested",
    "slang.source.imported": "imported",
    "slang.auto": "Apply confident suggestions (≥ {p}%)",
    "slang.autoTitle":
      "Approve at once what the model is sure of; you can undo each below",
    "slang.autoApplied": "auto-applied",
    "slang.undo": "Undo",
    "slang.rejected": "rejected",
    "slang.undoTitle": "Take it back and never suggest it again",
    "slang.all": ({ total, parallel }) =>
      `Read everything not read yet (${total} ${total === 1 ? "batch" : "batches"}, ${parallel} at once)`,
    "slang.newTerms": "New terms",
    "slang.noTerms":
      "No new terms: they come from suggestions, for things the glossary lacks.",
    "slang.newTerm": "new term",
    "slang.termPending": "its new term waits for review",
    "slang.rel.part-of": "part of {name}",
    "slang.rel.kind-of": "a kind of {name}",
    "slang.rel.related-to": "related to {name}",
    "slang.termAliases": "Aliases: {list}",
    "slang.deleteTermAsk": "Delete the term {name} and its aliases?",
    "slang.moves": "Better terms for old aliases",
    "slang.move": "Move",
    "slang.moveAll": "Move all",
    "slang.filter.label": "Which aliases",
    "slang.filter.all": "All",
    "slang.filter.auto": "Auto-applied",
    "slang.noAuto": "Nothing auto-applied.",
    "alias.add": "Add alias",
    "alias.text": "Alias",
    "alias.lang": "Language",
    "alias.note": "Note: its origin or use",
    "alias.term": "Term",
    "alias.termSearch": "Search a term in any language…",
    "alias.pickTerm": "Pick a term from the list.",
    "alias.noMatch": "No term matches.",
    "alias.save": "Save",
    "alias.saveApprove": "Save and approve",
    "alias.cancel": "Cancel",
    "alias.saved": "Saved: {text} → {term}",
    "alias.failed": "Not saved: {error}",

    // Editing a term: a new term in place, a glossary term as an override
    "slang.overrides": "Edited glossary terms",
    "term.edit": "Edit definition / relation",
    "term.editTitle":
      "Correct the definition, or how this term relates to a broader one; kept in glossary-user.toml, so imports never lose it",
    "term.name": "Name",
    "term.kind": "Kind",
    "term.definition": "Definition",
    "term.relation": "Relation",
    "term.relationNone": "none",
    "term.relationTo": "to the term",
    "term.save": "Save",
    "term.saved": "Saved: {name}",
    "term.failed": "Not saved: {error}",
    "term.overrideNote":
      "a glossary term keeps its names; your definition and relation override the glossary's",
    "term.edited": "edited by you",
    "term.unrelated": "relation removed",
    "term.reset": "Restore the glossary's",
    "term.resetAsk":
      "Drop your edits of {name}? The glossary's definition and relation come back.",

    // Knowledge
    "k.loading":
      "Loading the knowledge store… The first time, the embedding model (about 470 MB) is downloaded into the data folder.",
    "k.documents": "Documents",
    "k.chunks": "Chunks",
    "k.glossary": "Glossary",
    "k.digest": "Digest",
    "k.digestNote": "digest.md, sent with every message",
    "k.stats.cannotOpen": "The knowledge store cannot open: {error}",
    "k.stats.nothing": "nothing imported yet",
    "k.stats.ownGlossary": "glossary.toml in the data folder",
    "k.stats.seedGlossary": "the crate's seed glossary",
    "k.yes": "Yes",
    "k.no": "No",
    "k.key.set": "set",
    "k.key.notSet": "not set",
    "k.backend.api": "Model: Anthropic API (ANTHROPIC_API_KEY)",
    "k.backend.claude-cli": "Model: Claude subscription (claude CLI)",
    "k.backend.none": "Model: none",
    "k.backend.note":
      "What answers the chat and the translator: the API billed to ANTHROPIC_API_KEY, or the logged-in Claude Code CLI on your subscription ([cuttlefish] backend)",
    "k.key.discord": "Needed to import through a Discord bot",
    "k.key.discordNote": "(needs DISCORD_BOT_TOKEN where the lab runs)",
    "k.search": "Search",
    "k.searchNote":
      "nearest chunks, any language, no key; what the chat retrieves",
    "k.searchPlaceholder": "Stinger at low tide, バクダンの処理…",
    "k.searchLabel": "Search the knowledge",
    "k.results": "Results",
    "k.search.running": "Searching…",
    "k.search.none":
      "Nothing found: the store is empty. Import something first.",
    "k.licenseUnknown": "license unknown",
    "k.import": "Import",
    "k.importNote": "one at a time; web pages politely",
    "k.kind.web": "Web pages",
    "k.kind.sitemap": "Sitemap",
    "k.kind.wiki": "Wiki / site",
    "k.kind.youtube": "YouTube",
    "k.kind.file": "Files",
    "k.kind.export": "Discord export",
    "k.kind.bot": "Discord bot",
    "k.kind.leanny": "Game data (Lean)",
    "k.leanny.note":
      "Fact cards of exact numbers from Lean's Splatoon 3 datamine: Salmonids, stages, Salmon Run weapons and specials, hazard levels, and every Eggstra Work event with its waves and spawns (dates from Inkipedia). Fetched politely from leanny.github.io into the knowledge folder, files only when they changed; private study only, the data is Nintendo's. Thanks to Lean.",
    "k.leanny.dryRun": "Dry run: list the files first",
    "k.leanny.dryRunTitle":
      "Lists the files with their state and what the copies fetched so far would give; fetches and stores nothing",
    "k.leanny.weapons": "With the weapon parameters",
    "k.leanny.weaponsTitle":
      "The parameter tables of the Salmon Run weapons and specials: about 180 more requests the first time",
    "k.leanny.check": "List the files",
    "k.f.urls":
      "Page addresses, one per line (Google Docs, Sheets and Slides shared by link too)",
    "k.f.sitemap": "Sitemap address",
    "k.f.allTabs": "Every tab of a Google Sheet",
    "k.topic.mediawiki": "MediaWiki topic",
    "k.topic.site": "Whole site",
    "k.f.wikiStart":
      "Start pages or categories, titles or addresses, one per line",
    "k.f.api": "api.php (found from an address when empty)",
    "k.f.exclude": "Categories left out, one per line (optional)",
    "k.f.linkMatch":
      "Also pages a start page links to whose titles contain one of these words, one per line (optional)",
    "k.f.siteStart": "Start address (only its host is crawled)",
    "k.f.skip":
      "Paths skipped besides images and scripts, one per line (optional)",
    "k.f.depth": "Subcategory levels",
    "k.f.dryRun": "Dry run: count the pages first",
    "k.f.dryRunTitle":
      "Lists what is in scope and how long fetching it would take; stores nothing",
    "k.countPages": "Count pages",
    "k.f.atMost": "At most",
    "k.f.pages": "pages",
    "k.f.delay": "one request per site every",
    "k.f.youtube": "Video, playlist or channel (subtitles only)",
    "k.f.videos": "videos",
    "k.f.paths":
      "Files or folders on this machine, full paths, one per line (folders and archives go through the inbox)",
    "k.f.cite": "Address to cite (optional)",
    "k.f.whole": "Each file is one conversation (a thread or forum post)",
    "k.f.channels": "Channel ids, one per line",
    "k.f.threads": "With threads and forum posts",
    "k.f.kindAuto": "Kind: automatic",
    "k.f.license": "License or terms (optional)",
    "k.f.licenseLabel": "License",
    "k.f.refresh": "Again if stored",
    "k.f.refreshTitle":
      "Import again even if stored: replaces the document (a page that came in empty, say)",
    "k.source.web": "Web",
    "k.source.webPage": "Web page",
    "k.source.wiki": "Wiki",
    "k.source.guide": "Guide",
    "k.source.video": "Video",
    "k.source.vodReview": "#vod-review",
    "k.source.discord": "Discord",
    "k.source.file": "File",
    "k.source.expertNote": "Expert note",
    "k.format.note": "Note",
    "k.source.gameData": "Game data",
    "k.format.card": "Fact card",
    "k.source.x": "X (Twitter)",
    "k.format.posts": "Posts",
    "k.source.rednote": "Xiaohongshu",
    "k.ov.credits": "Credits",
    "k.ov.credit": "{n} fact cards from {name}: {what}.",
    "k.job.running": "running",
    "k.job.done": "done",
    "k.job.failed": "failed",
    "k.job.cancelled": "stopped (run again to continue)",
    "k.job.added": "{n} added",
    "k.cancel": "Cancel",
    "k.stop": "Stop (continue later)",
    "k.stopTitle":
      "Stops after the item under way (a page, a file, a batch; the embedding while the store loads too). What was imported is kept; running the same import again continues where it stopped.",
    "k.stopping": "Stopping…",
    "k.stoppingNote":
      "finishing the item under way; run the import again to continue",
    "k.filter": "Filter",
    "k.filterLabel": "Filter documents",
    "k.th.title": "Title",
    "k.th.source": "Source",
    "k.th.license": "License",
    "k.th.fetched": "Fetched",
    "k.docs.shown": "{n} of {total}",
    "k.docs.none": "No documents yet: import some.",
    "k.corpus.go": "Create reviews from #vod-review",
    "k.corpus.note":
      "reads the HUD of the videos on disk, then each reviewed VOD with its video becomes a review with the community's comments, and every comment becomes an expert comment the chat can draw on; re-run any time",
    "k.corpus.failed": "Could not start: {error}",

    // Knowledge: expert notes
    "k.notes.title": "Expert notes",
    "k.notes.note":
      'What you corrected or explained by hand, one Markdown file each in notes/ of the knowledge folder; the most trusted source in every answer. Write one from any answer of Cuttlefish with "Correct / add to memory".',
    "k.notes.count": ({ n }) => (n === 1 ? "1 note" : `${n} notes`),
    "k.notes.none": "no notes yet",
    "k.notes.new": "New note",
    "k.notes.edit": "Edit",
    "k.notes.delete": "Delete",
    "k.notes.deleteAsk": "Delete the note {id}? Its file is removed.",
    "k.notes.by": "{author}, {date}",
    "k.notes.answers": "answers {id}",
    "k.notes.from": "from {from}",

    // Knowledge: the deep questions and their eval
    "k.deep.title": "Deep questions",
    "k.deep.note":
      "Questions a high-level player asks, with answers the community knows. The eval asks the model the ones that need no video; mark each answer good or wrong, and turn a wrong one into an expert note, which Cuttlefish trusts over every other source from then on.",
    "k.deep.count": ({ n, notes }) =>
      `${n} questions${notes ? `, ${notes} answered by a note` : ""}`,
    "k.deep.langLabel": "Language of the questions",
    "k.deep.max": "at most",
    "k.deep.maxLabel": "Questions at most",
    "k.deep.run": "Run the deep eval",
    "k.deep.bank": "The bank: {n} questions in {c} categories",
    "k.deep.needs.knowledge": "knowledge",
    "k.deep.needs.video_moment": "a moment of a video",
    "k.deep.needs.video_range": "a range of a video",
    "k.deep.needs.hud": "the HUD",
    "k.deep.needs.detector": "coming later: needs the detector",
    "k.deep.reference": "note",
    "k.deep.answers": "Answers",
    "k.deep.noFiles":
      "No eval yet: run one here, or `cuttlefish eval deep` in a terminal.",
    "k.deep.fileNote": ({ entries, good, wrong, failed }) =>
      `${entries} answers, ${good} good, ${wrong} wrong${failed ? `, ${failed} failed` : ""}`,
    "k.deep.good": "Good",
    "k.deep.wrong": "Wrong",
    "k.deep.toNote": "Correct → note",
    "k.deep.noteMade": "note {id}",
    "k.deep.failed": "failed: {error}",
    "k.deep.sources": ({ n }) => (n === 1 ? "1 source" : `${n} sources`),
    "k.deep.cat.macro": "Macro and strategy",
    "k.deep.cat.openings": "Wave openings and roles",
    "k.deep.cat.bosses": "Boss mechanics",
    "k.deep.cat.stages": "Stages and tides",
    "k.deep.cat.events": "Known occurrences",
    "k.deep.cat.eggs": "Egg flow",
    "k.deep.cat.weapons": "Weapons and specials",
    "k.deep.cat.moments": "Moments of a video",

    // Knowledge: the inbox, import reports, overview, assets
    "k.kind.inbox": "Inbox",
    "k.inbox.import": "Import inbox",
    "k.inbox.intro":
      "Drop anything here: guides, name tables, whole projects, icons, zip files. Prose becomes searchable documents, names in several languages go into the glossary, images into the assets. Files can also be put straight into",
    "k.inbox.drop": "Drop files or folders here, or",
    "k.inbox.pickFiles": "Choose files",
    "k.inbox.pickFolder": "Choose a folder",
    "k.inbox.into": "Into a folder of the inbox (optional)",
    "k.inbox.status": "The inbox holds {files} files ({size}); {state}.",
    "k.inbox.new": "{n} new or changed since the last import",
    "k.inbox.allImported": "all imported",
    "k.inbox.empty": "The inbox is empty.",
    "k.upload.left": "{n} hidden, dependency or too large files left out",
    "k.upload.done": "Uploaded",
    "k.upload.count": "{done}/{total} files · {sent} of {size}",
    "k.upload.lost": "connection lost",
    "k.upload.failed": "failed {path}: {error}",
    "k.upload.summary": "{n} files uploaded. Import the inbox to digest them.",
    "k.upload.someFailed":
      "{n} files uploaded, {failed} failed. Import the inbox to digest them.",
    "k.report.show": "Show the report",
    "k.report.kind.document": "Documents",
    "k.report.kind.discord": "Discord exports",
    "k.report.kind.rednote": "Xiaohongshu creators",
    "k.report.kind.glossary": "Name tables → glossary",
    "k.report.kind.asset": "Images and icons",
    "k.report.more": "and {n} more",
    "k.report.failed": "Failed",
    "k.report.gone": "Gone from the inbox",
    "k.report.title": "Import of {time}",
    "k.report.stats": "{files} files looked at, {unchanged} unchanged",
    "k.report.statsCancelled":
      "{files} files looked at, {unchanged} unchanged, cancelled",
    "k.report.close": "Close",
    "k.report.reimport": "Import again",
    "k.report.nothing": "Nothing new taken.",
    "k.report.skipped": "Skipped",
    "k.none": "none",
    "k.ov.title": "What the store holds",
    "k.ov.imported": "{n} with imported names",
    "k.ov.tableTerms": "{n} terms",
    "k.ov.noTables": "No name tables imported yet.",
    "k.ov.linked": "{n} linked to a term",
    "k.ov.inboxNote": "{files} files ({size}), {n} new or changed",
    "k.ov.inboxFolder": "(inbox)",
    "k.ov.reports": "Last imports of the inbox",
    "k.ov.noReports": "None yet.",
    "k.ov.bySource": "By source",
    "k.ov.byFormat": "By format",
    "k.format.text": "Text",
    "k.format.googleDoc": "Google Doc",
    "k.format.googleSheet": "Google Sheet",
    "k.format.googleSlides": "Google Slides",
    "k.format.subtitles": "Subtitles",
    "k.format.messages": "Messages",
    "k.format.other": "Other",
    "k.ov.movedAside":
      "Our entries of the old knowledge folder were copied here and moved into {path}. That folder is safe to delete; the files beside it belong to another program and were left alone.",
    "k.assets": "Assets",
    "k.assets.filter": "Name, path or term: Splattershot, バクダン…",
    "k.assets.filterLabel": "Filter assets",
    "k.assets.folder": "Folder",
    "k.assets.allFolders": "All folders",
    "k.assets.firstShown": "{n} of {total}, first {shown} shown",
    "k.assets.none":
      "No images yet: drop icons or image folders into the inbox and import it.",
    "k.delete": "Delete",
    "k.delete.ask": 'Delete "{title}" and its {n} chunks?',
    "k.delete.askInbox":
      'Delete "{title}" and its {n} chunks? Its file stays in the inbox; it comes back only if the file changes or is imported with "Again if stored".',

    // Technique markers: the Studio's Techniques panel (techniques.js; its
    // weapons and specials are named by Lean's data), the Inkspector's
    // markers (inspect.js) and the Pedia's recorded examples
    "tech.title": "Techniques",
    "tech.mode.session": "This session",
    "tech.mode.sessionNote": "Reps marked in this session",
    "tech.mode.all": "Checklist",
    "tech.mode.allNote":
      "Which techniques have examples in any session, as a reminder of what to record",
    "tech.start": "Start span",
    "tech.stop": "Stop span",
    "tech.last": "Mark last",
    "tech.seconds": "Seconds to mark",
    "tech.undo": "Undo",
    "tech.add": "Add",
    "tech.add.label": "Add a technique",
    "tech.add.zh": "Chinese name",
    "tech.add.term": "Pedia term id",
    "tech.add.group": "Its group",
    "tech.keys":
      "<kbd>/</kbd> find · <kbd>1</kbd>–<kbd>9</kbd> pick in the open group · <kbd>M</kbd> start/stop a span · <kbd>B</kbd> mark the last seconds · <kbd>U</kbd> undo the last marker",
    "tech.group.movement": "Movement",
    "tech.group.eggs": "Egg handling",
    "tech.group.weapon": "Weapons",
    "tech.group.sub": "Sub weapon",
    "tech.group.special": "Specials",
    "tech.grizzco": "Grizzco weapons",
    "tech.group.recorded": "recorded {done} / {n}",
    "tech.group.recordedNote": "Items with an example in any session",
    "tech.group.marked": "marked {done} / {n}",
    "tech.group.markedNote": "Items marked in this session",
    "tech.find": "Find a technique, weapon or special  /",
    "tech.findLabel": "Find an item to mark: type, then Enter",
    "tech.find.none": "Nothing matches.",
    "tech.picked": "Picked",
    "tech.data.loading": "Reading Lean's weapons and specials…",
    "tech.data.failed": "Could not read the weapons and specials: {error}",
    "tech.data.none":
      "No game data yet: import “Game data (Lean)” in Cuttlefish's {link}.",
    "tech.data.knowledge": "Knowledge view",
    "tech.credit":
      "Names and pictures from Lean's Splatoon 3 datamine, {link}. Thanks, Lean!",
    "tech.sessionCount": ({ n }) =>
      n === 1
        ? "1 rep marked in this session"
        : `${n} reps marked in this session`,
    "tech.allCount": ({ n }) =>
      n === 1 ? "1 example recorded" : `${n} examples recorded`,
    "tech.none": "No example recorded yet",
    "tech.pedia": "Open in the Pedia",
    "tech.remove": "Remove from the list",
    "tech.removeAsk":
      'Remove "{name}" from the list? Its markers stay in the sessions.',
    "tech.exists": '"{name}" is on the list already',
    "tech.idleNote":
      "Pick what you practise (/ finds it), record, then mark each rep (a span, or the last seconds).",
    "tech.sessionNote": ({ n }) =>
      n === 1 ? "1 marker in this session" : `${n} markers in this session`,
    "tech.allNote": ({ done, n, markers, sessions }) =>
      `${done} of ${n} have examples · ${markers} marker${markers === 1 ? "" : "s"} in ${sessions} session${sessions === 1 ? "" : "s"}`,
    "tech.loading": "Reading the sessions…",
    "tech.marking": "Marking {name} · {time}",
    "mk.title": "Technique markers",
    "mk.note": "Spans of controller input; drawn at this delay",
    "mk.none": "No markers in this session.",
    "mk.elsewhere": ({ n }) =>
      n === 1 ? "1 more in other segments" : `${n} more in other segments`,
    "mk.add": "Add marker here",
    "mk.addNote":
      "Adds a 2 s marker from this frame; then set its start, end and technique",
    "mk.go": "Go",
    "mk.setStart": "Start here",
    "mk.setEnd": "End here",
    "mk.delete": "Delete",
    "mk.deleteAsk": 'Delete the marker "{name}"?',
    "mk.saveError": "Could not save the markers: {error}",
    "mk.technique": "Technique, weapon or special",
    "mk.frames": "Frames",
    "pedia.examples": "Recorded examples",
    "pedia.examplesNote": ({ n }) =>
      n === 1 ? "1 marked in your sessions" : `${n} marked in your sessions`,
    "pedia.examplesNone":
      "None marked yet: pick it in the Studio's Techniques panel while recording.",

    // The Predictor's online mode: AgentZero's policy, as if live
    "po.model": "Model",
    "po.model.idm": "IDM",
    "po.model.idmNote":
      "The inverse dynamics model labels a whole video after the fact, seeing frames before and after each one",
    "po.model.policy": "AgentZero online",
    "po.model.policyNote":
      "AgentZero's policy, online: frame by frame as if live, seeing only the past; on a video, or on the live capture, where it can play the Switch",
    "po.kind.live": "Live capture",
    "po.allowRec": "Allow while recording",
    "po.allowRecNote":
      "Run AgentZero while the Studio records (the recording may want the GPU; AgentZero's own play may be worth recording)",
    "po.start": "Start AgentZero",
    "po.running": "AgentZero is running; stop it first",
    "po.checking": "Checking agentzero-play…",
    "po.noCheckpoint": "No policy checkpoints",
    "po.noPolicy": "No policy checkpoints in {folder} (runs/policy/*/best.pt).",
    "po.noJson":
      "This agentzero-play has no --json yet: update AgentZero, then Recheck.",
    "po.noShared":
      "This agentzero-play cannot take the live capture's frames yet (--shared-frames): update AgentZero, then Recheck. Videos run.",
    "po.noInput":
      "The Studio has no video input: choose the capture card there first.",
    "po.gpu": "GPU {used} / {total} GiB",
    "po.gpuLow":
      "Low GPU memory: {free} GiB free, and the policy wants about 1.5 GiB. Tick CPU, or free the GPU first.",
    "po.state.loading": "Loading the model…",
    "po.state.live": "Watching the live capture",
    "po.state.video": "Playing along the video",
    "po.state.playing": "Playing the Switch",
    "po.state.done": "Done",
    "po.state.cancelled": "Stopped",
    "po.state.failed": "Failed",
    "po.actions": ({ n }) => (n === 1 ? "1 action" : `${n} actions`),
    "po.watch": "Watch",
    "po.stop": "Stop",
    "po.stopping": "Stopping…",
    "po.stored": "Kept as a run of the Predictor.",
    "po.openStored": "Open it",
    "po.notYet": "AgentZero has not run since the lab started.",
    "po.log": "Command and output",
    "po.play.start": "Let AgentZero play…",
    "po.play.stop": "Stop bot (Esc)",
    "po.play.off":
      "Off: AgentZero only watches, and nothing reaches the Switch.",
    "po.play.left":
      "Playing the Switch, mixed with your controller: {left} left, {sent} actions sent, {takeovers} takeovers by you.",
    "po.play.ended":
      "Stopped playing after {played} ({sent} actions sent): {why}.",
    "po.record": "Record bot runs",
    "po.recordNote":
      "Each time AgentZero plays, a session is recorded as the Studio's Record does, bot-<time> beside your own: the video with sound, what reached the Switch in controller.bin, the policy's own actions in agentzero.jsonl, and in session.json the checkpoint, the limits and every moment you took over. A session being recorded takes the run instead.",
    "po.record.recording": "Recording this run in {session}.",
    "po.record.recorded": "Recorded in {session}.",
    "po.record.open": "Open in the Inkspector",
    "po.limits.title": "What AgentZero may press",
    "po.limits.dpad": "Block the d-pad (signals)",
    "po.limits.dpadNote":
      'The d-pad sends signals ("This way!", "Booyah!") that disturb teammates. Your own presses still reach the Switch.',
    "po.limits.special": "Block the special (R-stick click)",
    "po.limits.specialNote": "Your own presses still reach the Switch.",
    "po.limits.cap": "At most",
    "po.limits.capUnit": "presses a second per button,",
    "po.limits.hold": "each held",
    "po.limits.holdUnit": "ms or more",
    "po.limits.capNote":
      "No button faster than a person could press it, so its play never looks like a turbo or a macro. The default, 7.7, is 1.1 times the 7 a second a person keeps up (ordinary people tapping hard reach 6 to 7); measure your own to set it from yours.",
    "po.limits.note":
      "Never Home or Capture. Your own presses always reach the Switch.",
    "po.limits.dpadShort": "the d-pad",
    "po.limits.specialShort": "the special",
    "po.limits.systemShort": "Home or Capture",
    "po.limits.and": ", ",
    "po.measure.start": "Measure my max…",
    "po.measure.startNote":
      "Tap ZR as fast as you can for 10 s; the lab counts your presses from the controller's reports",
    "po.measure.cancel": "Cancel measuring",
    "po.measure.waiting":
      "Tap ZR as fast as you can: 10 s from your first press.",
    "po.measure.counting": "{n} presses, {s} s left…",
    "po.measure.result":
      "Your fastest: {fastest} presses a second (six in a row), {average} over the 10 s; shortest press {hold} ms.",
    "po.measure.none": "No presses of ZR to measure.",
    "po.measure.use": "Use {hz} a second (1.1×)",
    "po.ended.time": "its time was up",
    "po.ended.you": "you stopped it",
    "po.ended.page": "no dashboard page was open",
    "po.ended.replay": "the Replay panel took the replay port",
    "po.ended.link": "the proxy's frames stopped reaching the lab",
    "po.ended.stall": "no action came from the policy for half a second",
    "po.ended.stopped": "AgentZero stopped",
    "po.ended.proxy": "the proxy closed the replay connection",
    "po.ended.recording": "its recording could not start",
    "po.badge.watching": "AgentZero watching",
    "po.badge.playing": "AgentZero playing",
    "po.loop.title": "Loop",
    "po.loop.median": "median ms",
    "po.loop.handoff": "Frame hand-off",
    "po.loop.handoffNote":
      "From the capture card's timestamp of a frame until AgentZero took it: the grabber, the pipe into the lab, shared memory and any wait",
    "po.loop.grabber": "capture card and grabber",
    "po.loop.grabberNote":
      "From the capture card's timestamp until the grabber wrote the frame out: the card's USB transfer, then ffmpeg's decoding, fitting and scaling to 640 x 360",
    "po.loop.pipe": "pipe to the lab",
    "po.loop.pipeNote": "The frame through the grabber's pipe into the lab",
    "po.loop.shared": "into shared memory",
    "po.loop.sharedNote":
      "The frame written into shared memory and announced to AgentZero",
    "po.loop.wait": "waiting for the model",
    "po.loop.waitNote":
      "Until AgentZero took the frame: the model busy with the frame before (it takes only the newest), or waking up",
    "po.loop.upload": "To the model's input",
    "po.loop.uploadNote":
      "The capture card's YUYV frame onto the model's device (the GPU, through pinned memory) and scaled there to 640 x 360 RGB",
    "po.loop.model": "Model",
    "po.loop.modelNote": "The policy's time for one frame",
    "po.loop.send": "Send",
    "po.loop.sendDry": "Back to the lab",
    "po.loop.sendNote":
      "From the action being ready until the lab wrote it to the proxy's replay port (while AgentZero only watches: until the lab had it)",
    "po.loop.total": "Capture to replay port",
    "po.loop.totalNote":
      "From the capture card's timestamp of a frame to its action written to the proxy's replay port; the network to the Pi and the next report come after",
    "po.loop.age": "Frame to action",
    "po.loop.ageNote":
      "From a frame's time in the paced video to its action being ready",
    "po.loop.rate": "{rate} actions/s",
    "po.loop.skipped": "{n} frames skipped",
    "po.loop.direct": "capture card read directly",
    "po.loop.footLive":
      "Over the last 10 seconds, on this machine's monotonic clock.",
    "po.loop.footVideo":
      "Over the last 10 seconds; the video is paced at 30 fps, as if live.",
    "po.confirm.title": "Let AgentZero play the Switch?",
    "po.confirm.what":
      "AgentZero's actions go to the Switch through the proxy's replay port, mixed with your controller.",
    "po.confirm.mix":
      "Your controller stays live and AgentZero never pauses for it: your buttons add to its own, and a stick you push past a small deadzone, or a turn faster than 10°/s, replaces its own while you do. Stop bot (always on screen) or Esc ends it.",
    "po.confirm.limits":
      "It never presses {blocked}, and presses a button at most {hz} times a second, each held {ms} ms or more.",
    "po.confirm.where":
      "Only in the practice area or a private job, with you at the console. Never in public jobs.",
    "po.confirm.for": "Play for",
    "po.confirm.cancel": "Cancel",
    "po.confirm.play": "Let it play",
    "bot.stop": "Stop bot",
    "bot.stopNote": "AgentZero is playing the Switch: stop it (Esc)",
    "bot.left": "{left} left",
    // The Pipeline app: the GPU and the experiment queue agents keep
    "pl.timeline.title": "GPU timeline",
    "pl.timeline.range": "Hours shown",
    "pl.range.60": "1 h",
    "pl.range.180": "3 h",
    "pl.range.360": "6 h",
    "pl.range.720": "12 h",
    "pl.queue.title": "Queue",
    "pl.results.title": "Results",
    "pl.history.title": "History",
    "pl.history.ended": "Ended",
    "pl.history.status": "Status",
    "pl.history.entry": "Entry",
    "pl.history.took": "Took",
    "pl.history.owner": "Owner",
    "pl.history.count": ({ n }) => (n === 1 ? "1 entry" : `${n} entries`),
    "pl.history.empty": "Nothing has finished yet.",
    "pl.history.more": "Show all {n}",
    "pl.offline": "Cannot reach the lab: {error}",
    "pl.group": "Experiment {group}",
    "pl.state.running": "Running",
    "pl.state.detected": "Running, not marked",
    "pl.state.detectedNote":
      "Its processes run, but the queue still says {status}.",
    "pl.state.gone": "No process",
    "pl.state.goneNote":
      "The queue says it runs, but none of its processes is left: did it end without the queue being told?",
    "pl.state.finished": "Ran to the end, not marked",
    "pl.state.stopped": "Ended early, not marked",
    "pl.state.queued": "Queued",
    "pl.state.paused": "Paused",
    "pl.state.done": "Done",
    "pl.state.failed": "Failed",
    "pl.idleBusy":
      "The GPU is {util}% busy with work the queue does not list (see its processes).",
    "pl.startedAt": "Started {clock}",
    "pl.device.gpu": "either GPU",
    "pl.device.cpu": "CPU",
    "pl.device.gpu:linux": "Linux GPU",
    "pl.device.gpu:win11": "win11 GPU",
    "pl.device.host": "{host} GPU",
    "pl.where.local": "Linux · {gpu}",
    "pl.where.remote": "{host} · {gpu}",
    "pl.where.device": "Queued for {device}",
    "pl.progress.steps": "step {step} of {total}",
    "pl.progress.step": "step {step}",
    "pl.progress.items": "{done} of {total}",
    "pl.progress.rate": "{ms} ms/step",
    "pl.progress.rateS": "{s} s/step",
    "pl.progress.perMin": "{n} a minute",
    "pl.progress.eta": "ETA {clock}",
    "pl.progress.left": "{left} left",
    "pl.progress.written": "last written {ago}",
    "pl.proc.gpu": "GPU memory",
    "pl.proc.cpu": "CPU",
    "pl.proc.ram": "RAM",
    "pl.proc.procs": "processes",
    "pl.proc.found": "Found by {how}",
    "pl.found.pgid": "its process group",
    "pl.found.pid": "its process and children",
    "pl.found.match": "its command line",
    "pl.curve.train": "train",
    "pl.curve.val": "validation",
    "pl.curve.split": "held-out {name}",
    "pl.curve.step": "step",
    "pl.curve.aria": "Loss by step: {series}",
    "pl.curve.loading": "Reading its run folder…",
    "pl.curve.none": "No metrics in its run folder yet",
    "pl.score.button_f1": "button F1",
    "pl.score.onset_f1": "onset F1",
    "pl.score.turn_corr_x": "turn r, x",
    "pl.score.turn_corr_y": "turn r, y",
    "pl.score.gyro_corr": "gyro r",
    "pl.score.stick_bin_accuracy": "stick bins",
    "pl.score.best": "best {value} at {step}",
    "pl.log": "Log",
    "pl.log.last": "Latest line",
    "pl.log.loading": "Reading…",
    "pl.recipe": "Recipe (args.json)",
    "pl.files": "Files",
    "pl.files.run": "run folder",
    "pl.files.log": "log",
    "pl.files.match": "command line has",
    "pl.notesCount": ({ n }) =>
      n === 1 ? "1 note, files" : `${n} notes, files`,
    "pl.tile.gpuBusy": "GPU busy",
    "pl.tile.gpuMem": "GPU memory",
    "pl.tile.temp": "Temperature",
    "pl.tile.power": "Power",
    "pl.tile.cpu": "CPU",
    "pl.tile.ram": "Memory",
    "pl.tile.last": "last {minutes} min",
    "pl.tile.fan": "fan {fan}%",
    "pl.tile.clock": "SM clock {clock} MHz",
    "pl.tile.swap": "swap {used} used",
    "pl.tile.swapFull": "swap full ({used})",
    "pl.noGpu": "No GPU readings: {error}",
    "pl.procs.title": "On the GPU",
    "pl.procs.other": "graphics and the rest",
    "pl.procs.otherNote":
      "Memory nvidia-smi lists under no compute process: the desktop, browsers, video",
    "pl.procs.none": "No compute process on the GPU",
    "pl.timeline.busy": "busy",
    "pl.timeline.jobs": "queue's jobs",
    "pl.timeline.other": "other memory",
    "pl.timeline.noLanes": "Nothing from the queue ran in this window",
    "pl.timeline.since":
      "Sampled every {s} s since the lab started, {clock}; earlier lanes come from the queue's own times.",
    "pl.timeline.empty": "No samples yet.",
    "pl.timeline.aria":
      "{gpu} over the last {hours} h: busy {util}% now, its machine's CPU, its memory, and what ran on it when",
    "pl.queue.count": ({ n }) => (n === 1 ? "1 waiting" : `${n} waiting`),
    "pl.queue.updated": "updated {ago}",
    "pl.queue.empty":
      "Nothing waits. Agents queue work with agentzero-queue add.",
    "pl.queue.hint":
      "Drag an entry by its handle, or focus the handle and press ↑ ↓, to change what runs next. Agents take the top one (agentzero-queue next).",
    "pl.queue.drag": "Drag to change the order",
    "pl.queue.move": "Move {id}, place {n} of {count}",
    "pl.queue.next": "Next",
    "pl.queue.saving": "Saving the order…",
    "pl.queue.saved": "Order saved",
    "pl.queue.failed": "Could not save the order: {error}",
    "pl.queue.missing":
      "No queue file yet at {path}. Agents create it with agentzero-queue add.",
    "pl.queue.problem": "Part of the queue file did not read: {problems}",
    "pl.results.count": ({ n }) => `${n} finished`,
    "pl.results.empty": "Nothing has finished yet.",
    "pl.results.next": "Next:",
    "pl.results.took": "took {time}",
    "pl.results.unmarked":
      "Its run folder reached step {step} of {total}; no result written yet.",
    "pl.results.unmarkedLog":
      "Its log reached {step} of {total}; no result written yet.",
    "pl.results.noSummary": "No result written yet.",
    "pl.chip.gpu": "Linux {util}%",
    "pl.chip.running": ({ n }) => `${n} running`,
    "pl.chip.idle": "idle",
    "pl.dur.s": "{s} s",
    "pl.dur.m": "{m} min",
    "pl.dur.h": "{h} h {m} min",
    "pl.state.early": "Stopped early, not marked",
    "pl.state.unknown": "No word",
    "pl.state.goneRemote":
      "The queue says it runs on {host}, but the {host} runner runs nothing.",
    "pl.state.goneRemoteJob":
      "The queue says it runs on {host}, but the {host} runner runs {job}.",
    "pl.state.unknownSince":
      "No word from the {host} runner since {clock} ({ago}): whether it still runs is unknown.",
    "pl.state.unknownNever":
      "The {host} runner has written no GPU file: whether it runs is unknown.",
    "pl.state.runnerStopped": "Its runner is not running.",
    "pl.state.unknownStarting":
      "The {host} runner has taken it and has not written since: it copies the code and data over before the job starts.",
    "pl.phase.trained": "its training ran all {total} steps",
    "pl.phase.trainedEarly":
      "its training stopped early at step {step} of {total}",
    "pl.phase.trainedAt": "its training ended at step {step}",
    "pl.phase.ended": "Ran all {total} steps",
    "pl.phase.endedEarly": "Stopped early at step {step} of {total}",
    "pl.phase.endedAt": "Ended at step {step}",
    "pl.phase.best": "best at step {step}",
    "pl.phase.stalled": "no new step for {time}",
    "pl.proc.gpuBusy": "GPU busy",
    "pl.proc.wholeGpu":
      "The whole {host} GPU: Windows does not tell a process's share",
    "pl.proc.read": "last read",
    "pl.log.lastAgo": "Latest line, {ago}",
    "pl.files.hostPid": "process on {host}",
    "pl.files.command": "command",
    "pl.copycat": "Copycat check",
    "pl.copycat.sub": "beyond repeating the present",
    "pl.copycat.note":
      "Actions persist, so a policy can score well by repeating what the frame seen shows, and still never act on its own. These scores count what it gets right beyond the present.",
    "pl.score.keyframe_button_acc": "keyframe buttons",
    "pl.score.keyframe_onset_recall": "presses caught",
    "pl.score.keyframe_release_recall": "releases caught",
    "pl.score.anticipation_left_x": "anticipation, left x",
    "pl.score.anticipation_left_y": "left y",
    "pl.score.anticipation_turn_x": "turn x",
    "pl.score.anticipation_turn_y": "turn y",
    "pl.score.turn_corr_x_500ms": "turn r 0.5 s, x",
    "pl.score.turn_corr_y_500ms": "y",
    "pl.score.press_f1": "press F1",
    "pl.score.frame_f1_tolerant": "frame F1 ±2",
    "pl.score.onset_f1_wide": "onset F1 ±4",
    "pl.score.hold_iou": "hold IoU",
    "pl.score.tip.keyframe_button_acc":
      "Of the moments the target changes a button (ZR, ZL, B, A, R, Y) from the frame seen, the share it gets right. Repeating the present scores 0.",
    "pl.score.tip.anticipation_left_x":
      "What its left stick (x) says of the target beyond the frame seen: the partial correlation given the present. A copy of the present scores 0.",
    "pl.score.tip.turn_corr_x_500ms":
      "The camera turn summed over half a second, correlated with the player's (x): does the aim go the right way over a moment, not frame by frame.",
    "pl.score.tip.press_f1":
      "F1 of presses, a burst counted as one press, averaged over the buttons pressed: does it press when the player does, and as often.",
    "pl.tile.cpuJobs": "queue {cores} cores",
    "pl.tile.noReading": "no reading",
    "pl.tile.unreachable": "unreachable",
    "pl.tile.runnerStopped": "runner not running",
    "pl.tile.hold": "on hold",
    "pl.procs.remoteNote":
      "Its runner lists the Python processes on the {host} GPU; Windows does not tell their memory",
    "pl.procs.noReading": "The {host} runner last read its GPU {ago}",
    "pl.timeline.cpu": "CPU",
    "pl.timeline.cpuLoad": "CPU · load {load}",
    "pl.timeline.used": "memory used",
    "pl.timeline.jobCpu": "job's CPU",
    "pl.timeline.noReading": "no reading",
    "pl.timeline.cores": "{cores} cores",
    "pl.timeline.strip":
      "Inside each bar, its job's CPU: full height is {cores} cores.",
    "pl.queue.after": "after {ids}",
    "pl.queue.afterNote":
      "Waits for these entries: no runner takes it until they are done",
    "pl.queue.nextOn": "Next · {on}",
    "pl.queue.nextNote":
      "The entry this runner takes next: the first queued one that fits it and waits for nothing",
    "pl.chip.remote": "{host} {util}%",
    "pl.results.early":
      "Stopped early at step {step} of {total} (no better validation); no result written yet.",
    // Storage: the Proxmox pool every VM's disk lives on, the top bar's
    // chip and the Pipeline's banner while it runs low
    "pl.storage.chip.low": "{pool} low: {free} free",
    "pl.storage.chip.critical": "{pool} almost full: {free} free",
    "pl.storage.chip.unknown": "{pool}: no reading",
    "pl.storage.pool":
      "{pool} on {host}: {free} free of {size} ({cap}% used, {frag}% fragmented)",
    "pl.storage.noPool": "{pool} on {host}: no fresh reading",
    "pl.storage.read": "Read at {time}, once a minute",
    "pl.storage.error": "The last reading failed: {error}",
    "pl.storage.noAnswer": "no answer yet",
    "pl.storage.disk": "{name}: {free} free of {size}",
    "pl.storage.root": "This host's /",
    "pl.storage.remote": "win11 C:",
    "pl.storage.limits":
      "Low under {low} free or at {cap}% used; critical under {critical}.",
    "pl.storage.banner.low":
      "{pool} on {host} is running low: {free} free of {size} ({cap}% used).",
    "pl.storage.banner.critical":
      "{pool} on {host} is almost full: {free} free of {size} ({cap}% used).",
    "pl.storage.banner.unknown":
      "No fresh reading of {pool} on {host}: {error}.",
    "pl.storage.banner.why":
      "Every VM's disk is a thin volume on it: when it fills up, the host hangs and both VMs with it. Free space before big jobs.",
    "pl.m.running": ({ n }) => `${n} running`,
    "pl.m.gone": "{n} with no process",
    "pl.m.unknown": "{n} with no word",
    "pl.m.stalled": "{n} stalled",
    "pl.m.idle": "Idle",
    "pl.m.busyOther": "{util}% busy, not with the queue's work",
    "pl.m.never": "No word from its runner yet",
    "pl.m.quiet": "Last word {ago}",
    "pl.m.quietNote":
      "No word from its runner since {clock} ({ago}): what runs there is unknown.",
    "pl.m.runnerDownNote":
      "Its runner (agentzero-win11 run) is not running: nothing new starts on this GPU.",
    "pl.m.holdNote":
      "Its runner holds new entries back (runs/win11/HOLD); what runs goes on.",
    "pl.m.next": "Next",
    "pl.m.nextNote":
      "What this GPU's runner takes next: the first queued entry that fits it and waits for nothing",
    "pl.m.nextWaitsNote":
      "The first queued entry this GPU can take, once the entries it waits for are done",
    "pl.m.idleText": "Nothing of the queue runs on this GPU.",
    "pl.m.nextHere": "Next here",
    "pl.m.nextWaits": "Next here, after {ids}",
    "pl.m.lastHere": "Last here · {clock} · took {took}",
    "pl.tile.notRead": "its runner does not read it yet",
    "pl.tile.gpuMemNote": "the queue's jobs {jobs} · the rest {other}",
    "pl.tile.of": "of {total}",
    "pl.tile.powerOf": "of {limit} W",
    "pl.tile.threads": "{cores} threads",
    "pl.tile.load": "load {load}",
    "pl.tile.available": "{available} available",
    "pl.tile.disk": "Disk",
    "pl.tile.diskNote": "free of {total} · {path}",
    "pl.work.running": "running {time}",
    "pl.work.plain": "No step counter · running {time}",
    "pl.work.plainNow": "No step counter",
    "pl.work.note":
      "What its log says it runs: the step its job script started last (== start <name>), else its main process",
    "pl.proc.main": "main process: {name}",
  },
};

/** The language shown: `procon-lang`, else the browser's */
let i18nCurrent = (() => {
  let saved = null;
  try {
    saved = localStorage.getItem("procon-lang");
  } catch {
    // Storage may be refused; the browser's language decides
  }
  if (saved === "en" || saved === "zh") return saved;
  const browser = navigator.languages?.[0] ?? navigator.language ?? "en";
  return browser.toLowerCase().startsWith("zh") ? "zh" : "en";
})();

/** The language shown: "en" or "zh" */
const i18nLang = () => i18nCurrent;

/** The locale for dates and numbers */
const i18nLocale = () => (i18nCurrent === "zh" ? "zh-CN" : "en");

/** The string of `key` in the language shown, with {name} placeholders
 * filled from `values`; English when the language lacks it, the key when
 * neither has it. An entry that is a list (example messages) comes back
 * as a list of strings. */
function t(key, values = {}) {
  const entry = I18N[i18nCurrent]?.[key] ?? I18N.en[key];
  if (entry == null) return key;
  const text = typeof entry === "function" ? entry(values) : entry;
  const fill = (s) =>
    s.replace(/\{(\w+)\}/g, (match, name) =>
      values[name] != null ? String(values[name]) : match,
    );
  return Array.isArray(text) ? text.map(fill) : fill(text);
}

/** Translate the marked elements under `root` */
function applyI18n(root = document) {
  document.documentElement.lang = i18nLocale();
  for (const el of root.querySelectorAll("[data-i18n]")) {
    el.textContent = t(el.dataset.i18n);
  }
  for (const el of root.querySelectorAll("[data-i18n-html]")) {
    el.innerHTML = t(el.dataset.i18nHtml);
  }
  for (const [attribute, data] of [
    ["placeholder", "i18nPlaceholder"],
    ["title", "i18nTitle"],
    ["aria-label", "i18nAriaLabel"],
  ]) {
    for (const el of root.querySelectorAll(
      `[data-${attribute === "aria-label" ? "i18n-aria-label" : `i18n-${attribute}`}]`,
    )) {
      el.setAttribute(attribute, t(el.dataset[data]));
    }
  }
  // Each language is offered in its own words
  for (const button of document.querySelectorAll("[data-pick-lang]")) {
    const lang = button.dataset.pickLang;
    button.textContent = I18N[lang]?.["lang.name"] ?? lang;
    button.setAttribute("aria-pressed", String(lang === i18nCurrent));
  }
  // The quick switch names the language it goes to
  for (const button of document.querySelectorAll("[data-toggle-lang]")) {
    button.querySelector(".tool-name").textContent = t("view.switchTo", {
      lang: I18N[otherLang()]["lang.name"],
    });
  }
}

/** The language the quick switch goes to */
const otherLang = () => (i18nCurrent === "zh" ? "en" : "zh");

/** Show the page in `lang` and remember it */
function setLang(lang) {
  if (!I18N[lang]) return;
  i18nCurrent = lang;
  try {
    localStorage.setItem("procon-lang", lang);
  } catch {
    // The choice holds until reload
  }
  applyI18n();
  window.dispatchEvent(new CustomEvent("lang-change", { detail: { lang } }));
}

document.addEventListener("DOMContentLoaded", () => {
  for (const button of document.querySelectorAll("[data-pick-lang]")) {
    button.addEventListener("click", () => setLang(button.dataset.pickLang));
  }
  for (const button of document.querySelectorAll("[data-toggle-lang]")) {
    button.addEventListener("click", () => setLang(otherLang()));
  }
  applyI18n();
});
