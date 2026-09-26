// Page language: a dictionary per language and `t(key, values)` to look a
// string up. English is here; each other language has a file of its own that
// adds its table to I18N (i18n-zh.js: Simplified Chinese). Loaded before the
// apps' scripts. The Cuttlefish app (with its Knowledge view) is translated;
// other apps can adopt it key by key.
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

    // Cuttlefish: the views
    "cf.name": "Cuttlefish",
    "cf.tab.reviews": "Reviews",
    "cf.tab.reviewsNote": "Ask Cuttlefish, open a video, browse your reviews",
    "cf.tab.knowledge": "Knowledge",
    "cf.tab.knowledgeNote":
      "What Cuttlefish knows: import material, documents, glossary, assets, search",
    "cf.loading": "Loading…",

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
    "cf.ask.watching": "Cuttlefish is watching…",
    "cf.chat.entryNote":
      "a question, a callout to translate, jargon to explain; each conversation is a review, with or without a video",
    "cf.chat.messageLabel": "Message to Cuttlefish",
    "cf.chat.send": "Send",
    "cf.chat.keys": "Enter sends · Shift+Enter for a new line",
    "cf.chat.thinking": "Cuttlefish is thinking… (up to a minute)",
    "cf.chat.noKey":
      "ANTHROPIC_API_KEY is not set where the studio runs, so Cuttlefish cannot answer yet. Export it before starting the studio; your messages, comments and drawings are saved as usual.",
    "cf.chat.failed": "Could not reach Cuttlefish: {error}",
    "cf.chat.error": "Cuttlefish could not answer: {error}",
    "cf.chat.empty":
      "Ask anything about Salmon Run, paste a callout to translate for a teammate, or ask about the moment you are watching. Answers cite the knowledge; times in them seek the video.",
    "cf.chat.try": "Try one:",
    // The player's own jargon, shown in every language (data, like the
    // glossary), and two requests in English
    "cf.chat.examples": [
      "惯性取消搬蛋快",
      "我刚拿的熊刷，不应该上柱子拍的",
      "小枪可以优先出差回收一些外围蛋，但不要待太久卡新一波怪",
      "我还剩一个镭射",
      "Translate for an English-speaking teammate: 小枪可以优先出差回收一些外围蛋",
      "When should I leave the basket to kill a Stinger?",
    ],
    "cf.chat.with": "With the video:",
    "cf.chat.ctxMoment": "this moment",
    "cf.chat.ctxRange": "a range",
    "cf.chat.ctxNone": "no frames",
    "cf.chat.at": "at {time}",
    "cf.chat.sources": "Sources",
    "cf.chat.commentsAdded": ({ n }) =>
      n === 1 ? "1 comment added" : `${n} comments added`,
    "cf.chat.seek": "Go to {time}",
    "cf.attach.title": "Attach a video",
    "cf.attach.note":
      "optional: Cuttlefish can then look at the moments you ask about",
    "cf.attach.attach": "Attach",
    "cf.attach.download": "Download and attach",
    "cf.attach.failed": "Not attached: {error}",

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
    "k.key.anthropic": "Needed by the chat",
    "k.key.discord": "Needed to import through a Discord bot",
    "k.key.discordNote": "(needs DISCORD_BOT_TOKEN where the studio runs)",
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
    "k.kind.wiki": "Wiki category",
    "k.kind.youtube": "YouTube",
    "k.kind.file": "Files",
    "k.kind.export": "Discord export",
    "k.kind.bot": "Discord bot",
    "k.f.urls": "Page addresses, one per line",
    "k.f.sitemap": "Sitemap address",
    "k.f.categories": "Categories, one per line",
    "k.f.atMost": "At most",
    "k.f.pages": "pages",
    "k.f.delay": "one request per site every",
    "k.f.youtube": "Video, playlist or channel (subtitles only)",
    "k.f.videos": "videos",
    "k.f.paths": "Files on this machine, full paths, one per line",
    "k.f.cite": "Address to cite (optional)",
    "k.f.whole": "Each file is one conversation (a thread or forum post)",
    "k.f.channels": "Channel ids, one per line",
    "k.f.threads": "With threads and forum posts",
    "k.f.kindAuto": "Kind: automatic",
    "k.f.license": "License or terms (optional)",
    "k.f.licenseLabel": "License",
    "k.f.refresh": "Again if stored",
    "k.source.web": "Web",
    "k.source.webPage": "Web page",
    "k.source.wiki": "Wiki",
    "k.source.guide": "Guide",
    "k.source.video": "Video",
    "k.source.vodReview": "#vod-review",
    "k.source.discord": "Discord",
    "k.source.file": "File",
    "k.job.running": "running",
    "k.job.done": "done",
    "k.job.failed": "failed",
    "k.job.cancelled": "cancelled",
    "k.job.added": "{n} added",
    "k.cancel": "Cancel",
    "k.filter": "Filter",
    "k.filterLabel": "Filter documents",
    "k.th.title": "Title",
    "k.th.source": "Source",
    "k.th.license": "License",
    "k.th.fetched": "Fetched",
    "k.docs.shown": "{n} of {total}",
    "k.docs.none": "No documents yet: import some.",
    "k.glossary.size": "{n} terms",
    "k.glossary.none": "No glossary term found.",
    "k.termPlaceholder": "Steelhead, コジャケ, or a sentence",
    "k.termLabel": "Term or text",
    "k.lookUp": "Look up",

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
