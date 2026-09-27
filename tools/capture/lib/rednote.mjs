// Xiaohongshu (RedNote, 小红书) as its web page sees it: the JSON answers
// the page receives (its API, snake case: `note_card`, `liked_count`) and
// the page's server state (`window.__INITIAL_STATE__`, camel case:
// `noteCard`, `likedCount`), read into notes, comments, listed notes and
// followed accounts; what a page says when it wants a person (a captcha,
// a slider, a login); and the record `rednote.mjs` writes, one JSON line
// per note in `<inbox>/rednote/<user id>/notes.jsonl`, which
// `cuttlefish ingest inbox` reads (`crates/cuttlefish/src/rednote.rs`).

import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

export const SITE = "https://www.xiaohongshu.com";
/** The site's hosts: xiaohongshu.com, and rednote.com, where it sends
 * visitors from outside China */
const HOST = String.raw`(?:xiaohongshu|rednote)\.com`;

/** The site's origin a page is on (`https://www.rednote.com`), else null */
export function originOf(url) {
  const m = new RegExp(
    String.raw`^https://(?:[\w-]+\.)*${HOST}(?=[/?#]|$)`,
  ).exec(url ?? "");
  return m ? m[0] : null;
}
/** The `tool` of the state file, so the inbox knows whose it is */
export const TOOL = "rncap";
/** The `source` of every record */
export const SOURCE = "rednote";
export const NOTES_FILE = "notes.jsonl";

/** Whether an address is one of the site's JSON answers worth keeping */
export const isSiteApi = (url) =>
  new RegExp(String.raw`${HOST}/api/`).test(url) ||
  new RegExp(String.raw`${HOST}/.*/user/me`).test(url);

/** What the site's pages say when they want a person. Parts of the
 * address (a captcha, a verification, a login, a risk-control page), words
 * of the page text (a security check, a slider, a code to enter, a request
 * to log in or scan a code, an account or traffic anomaly), and codes of
 * JSON answers that mean the session is not accepted. */
export const MARKERS = Object.freeze({
  url: ["captcha", "verify", "login", "risk", "security"],
  text: [
    "安全验证",
    "请完成验证",
    "滑动验证",
    "滑块",
    "验证码",
    "扫码登录",
    "登录后查看",
    "请先登录",
    "帐号异常",
    "账号异常",
    "访问频次异常",
    "访问异常",
    "网络连接异常",
  ],
  codes: [-100, -101, 300011, 300012, 300013, 300015],
});

/** Texts of page elements the flow clicks: the button that unfolds more
 * replies under a comment */
export const PAGE = Object.freeze({
  moreReplies: ["展开", "条回复", "更多回复"],
});

/** Why a page address or text means a challenge, or null */
export function challenge(url, text) {
  const lower = (url ?? "").toLowerCase();
  const inUrl = MARKERS.url.find((m) => lower.includes(m));
  if (inUrl) return `the page address holds "${inUrl}": ${url}`;
  const page = text ?? "";
  const inText = MARKERS.text.find((m) => page.includes(m));
  if (!inText) return null;
  // The words around it, to tell a challenge from a page that mentions one
  const at = page.indexOf(inText);
  const around = page
    .slice(Math.max(0, at - 40), at + inText.length + 40)
    .replace(/\s+/g, " ");
  return `the page says "${inText}" (…${around}…)`;
}

/** Whether a JSON answer's code means the session is not accepted */
export const refused = (code) => MARKERS.codes.includes(code);

/** The first of `names` the object has with a value (snake or camel case) */
export function field(v, names) {
  if (!v || typeof v !== "object") return undefined;
  for (const n of names) if (v[n] != null) return v[n];
  return undefined;
}

const text = (v, names) => {
  const x = field(v, names);
  if (typeof x === "string") return x.trim();
  if (typeof x === "number") return String(x);
  return "";
};

/** A count as the site writes it: a number, or `1234`, `1,234`, `1.2万`,
 * `3亿`, `999+`; anything else is 0. (万 is ten thousand, 亿 a hundred
 * million.) */
export function count(v) {
  if (typeof v === "number") return Math.max(0, Math.round(v));
  if (typeof v !== "string") return 0;
  let s = v.trim().replace(/[,+]/g, "");
  let unit = 1;
  if (s.endsWith("万")) {
    unit = 10_000;
    s = s.slice(0, -1);
  } else if (s.endsWith("亿")) {
    unit = 100_000_000;
    s = s.slice(0, -1);
  }
  const n = Number(s.trim());
  return Number.isFinite(n) ? Math.max(0, Math.round(n * unit)) : 0;
}

/** A time the site writes as milliseconds (or seconds) since the epoch,
 * as a number or its text, as an ISO string; null when it is none */
export function timeOf(v) {
  const n = typeof v === "string" ? Number(v.trim()) : v;
  if (typeof n !== "number" || !Number.isFinite(n) || n <= 0) return null;
  // Seconds before the year 5138, milliseconds after 1973
  const ms = n < 100_000_000_000 ? n * 1000 : n;
  return new Date(ms).toISOString();
}

const authorOf = (v) => ({
  user_id: text(v, ["user_id", "userId", "userid", "id"]),
  nickname: text(v, ["nickname", "nick_name", "nickName", "name"]),
});

/** The note's page, with the token its link carries when known */
export function noteUrl(id, token) {
  const base = `${SITE}/explore/${id}`;
  return token
    ? `${base}?xsec_token=${encodeURIComponent(token)}&xsec_source=pc_user`
    : base;
}

/** A note out of the site's `note_card` (its API) or `note` (its page
 * state): everything but the comments; null without an id */
export function noteFrom(card) {
  const id = text(card, ["note_id", "noteId", "id"]);
  if (!id) return null;
  const interact = field(card, ["interact_info", "interactInfo"]) ?? {};
  const images = [];
  for (const i of field(card, ["image_list", "imageList"]) ?? []) {
    const direct = text(i, ["url_default", "urlDefault", "url"]);
    if (direct) {
      images.push(direct);
      continue;
    }
    const info = (field(i, ["info_list", "infoList"]) ?? []).find((x) =>
      text(x, ["url"]),
    );
    if (info) images.push(text(info, ["url"]));
  }
  let video = null;
  const stream = field(card, ["video"])?.media?.stream ?? {};
  for (const codec of ["h264", "h265", "av1"]) {
    const first = stream[codec]?.[0];
    const url = first ? text(first, ["master_url", "masterUrl"]) : "";
    if (url) {
      video = url;
      break;
    }
  }
  if (!video) {
    const key = text(field(card, ["video"])?.consumer, [
      "origin_video_key",
      "originVideoKey",
    ]);
    if (key) video = `https://sns-video-bd.xhscdn.com/${key}`;
  }
  return {
    id,
    url: noteUrl(id),
    author: authorOf(field(card, ["user", "user_info", "userInfo"])),
    date: timeOf(field(card, ["time", "create_time", "createTime"])),
    updated: timeOf(field(card, ["last_update_time", "lastUpdateTime"])),
    kind: text(card, ["type"]),
    title: text(card, ["title", "display_title", "displayTitle"]),
    text: text(card, ["desc"]),
    tags: (field(card, ["tag_list", "tagList"]) ?? [])
      .map((t) => text(t, ["name"]))
      .filter(Boolean),
    images,
    video,
    likes: count(field(interact, ["liked_count", "likedCount"])),
    collects: count(field(interact, ["collected_count", "collectedCount"])),
    shares: count(field(interact, ["share_count", "shareCount"])),
    comment_count: count(field(interact, ["comment_count", "commentCount"])),
    xsec_token: text(card, ["xsec_token", "xsecToken"]) || null,
    comments: [],
    comments_complete: false,
  };
}

/** A comment (with the replies it carries) out of the site's comment
 * object; null when it has neither id nor text */
export function commentFrom(v) {
  const id = text(v, ["id", "comment_id", "commentId"]);
  const content = text(v, ["content"]);
  if (!id && !content) return null;
  const target = field(v, ["target_comment", "targetComment"]);
  const targetId = text(target, ["id"]);
  const targetAuthor = authorOf(
    field(target, ["user_info", "userInfo", "user"]),
  );
  return {
    id,
    author: authorOf(field(v, ["user_info", "userInfo", "user"])),
    date: timeOf(field(v, ["create_time", "createTime", "time"])),
    text: content,
    likes: count(field(v, ["like_count", "likeCount"])),
    location: text(v, ["ip_location", "ipLocation"]) || null,
    reply_to: targetId || null,
    reply_to_author: targetAuthor.nickname || null,
    replies: (field(v, ["sub_comments", "subComments"]) ?? [])
      .map(commentFrom)
      .filter(Boolean),
    replies_total: count(field(v, ["sub_comment_count", "subCommentCount"])),
  };
}

/** A note as a creator's list shows it (`user_posted`, or the page
 * state's `notes`, which wraps each in `noteCard`): id, title, the token
 * its link needs, and its author's id when given */
export function listedFrom(v) {
  const card = field(v, ["noteCard", "note_card"]) ?? v;
  const id = text(card, ["note_id", "noteId", "id"]);
  if (id.length < 8) return null;
  return {
    id,
    title: text(card, ["display_title", "displayTitle", "title"]),
    xsec_token: text(card, ["xsec_token", "xsecToken"]) || null,
    author_id:
      text(field(card, ["user", "user_info", "userInfo"]), [
        "user_id",
        "userId",
        "userid",
      ]) || null,
  };
}

/** One `name=value` of a URL's query, or null */
function query(url, name) {
  try {
    return new URL(url).searchParams.get(name);
  } catch {
    return null;
  }
}

/** What a JSON answer of the site holds, by its shape (and its address for
 * what the shape does not say):
 * - `{kind: "me", user_id, guest, fields}`: the user (`/user/me`); a
 *   guest has an id too, so only an account's name (nickname or red_id, the handle)
 *   counts as logged in; `fields` names what the answer held
 * - `{kind: "followings", users, has_more}`: a page of followed accounts,
 *   or the accounts the comment box's @ picker offers (`intimacy_list`)
 * - `{kind: "list", notes, has_more}`: a page of a creator's notes
 * - `{kind: "notes", notes}`: note details (`/feed`), without comments
 * - `{kind: "comments", note_id, root, comments, has_more}`: a page of
 *   comments, `root` set for the replies of one comment
 * - `{kind: "error", code, msg}`: the site refused
 * - null for anything else */
export function recognise(url, body) {
  if (!body || typeof body !== "object") return null;
  const data = body.data;
  if (
    body.success === false ||
    (typeof body.code === "number" && body.code !== 0 && body.code !== 200)
  )
    return {
      kind: "error",
      code: body.code ?? 0,
      msg: text(body, ["msg", "message"]),
    };
  if (!data || typeof data !== "object") return null;
  const hasMore = () => field(data, ["has_more", "hasMore"]) === true;
  if (/\/user\/me/.test(url ?? ""))
    return {
      kind: "me",
      user_id: text(data, ["user_id", "userId"]),
      guest:
        field(data, ["guest"]) === true ||
        !(text(data, ["nickname"]) || text(data, ["red_id", "redId"])),
      fields: Object.keys(data).sort(),
    };
  if (/intimacy_list/.test(url ?? "") && Array.isArray(data.items))
    return {
      kind: "followings",
      // Its ids carry a hash after the account's (`<24 hex>_<32 hex>`)
      users: data.items
        .map(authorOf)
        .map((u) => ({ ...u, user_id: u.user_id?.split("_")[0] }))
        .filter((u) => u.user_id),
      has_more: false,
    };
  if (Array.isArray(data.users))
    return {
      kind: "followings",
      users: data.users.map(authorOf).filter((u) => u.user_id),
      has_more: hasMore(),
    };
  if (Array.isArray(data.notes))
    return {
      kind: "list",
      notes: data.notes.map(listedFrom).filter(Boolean),
      has_more: hasMore(),
    };
  if (Array.isArray(data.items)) {
    const notes = data.items
      .map((i) => noteFrom(field(i, ["note_card", "noteCard"])))
      .filter(Boolean);
    return notes.length ? { kind: "notes", notes } : null;
  }
  if (Array.isArray(data.comments))
    return {
      kind: "comments",
      note_id:
        query(url, "note_id") ?? (text(data, ["note_id", "noteId"]) || null),
      root: query(url, "root_comment_id"),
      comments: data.comments.map(commentFrom).filter(Boolean),
      has_more: hasMore(),
    };
  return null;
}

/** The notes of the page's server state (`noteDetailMap`, refs
 * unwrapped by the page script), each with the comments it holds */
export function notesFromState(state) {
  const map = state?.note?.noteDetailMap;
  if (!map || typeof map !== "object") return [];
  const out = [];
  for (const e of Object.values(map)) {
    const note = noteFrom(field(e, ["note", "noteCard", "note_card"]));
    if (!note) continue;
    const comments = field(e, ["comments"]);
    note.comments = (field(comments, ["list"]) ?? [])
      .map(commentFrom)
      .filter(Boolean);
    note.comments_complete = field(comments, ["hasMore", "has_more"]) === false;
    out.push(note);
  }
  return out;
}

/** The notes a creator's page state lists (`user.notes`: one list per
 * tab, the first tab the notes) */
export function listedFromState(state) {
  const notes = state?.user?.notes;
  if (!Array.isArray(notes)) return [];
  const list = notes.length && notes.every(Array.isArray) ? notes[0] : notes;
  return list.map(listedFrom).filter(Boolean);
}

/** The note ids in the links of a page (`/explore/<id>`,
 * `/discovery/item/<id>`, `/user/profile/<user>/<id>`), with their tokens
 * and the link's first line as the title */
export function listedFromLinks(links) {
  const seen = new Map();
  for (const [href, label] of links ?? []) {
    let path;
    let params;
    try {
      const u = new URL(href, SITE);
      path = u.pathname;
      params = u.searchParams;
    } catch {
      continue;
    }
    const m =
      /^\/(?:explore|discovery\/item)\/([0-9a-f]{24})$/.exec(path) ??
      /^\/user\/profile\/([^/]+)\/([0-9a-f]{24})$/.exec(path);
    if (!m) continue;
    const id = m[2] ?? m[1];
    const author = m[2] ? m[1] : null;
    const entry = seen.get(id) ?? {
      id,
      title: "",
      xsec_token: null,
      author_id: author,
    };
    const first = (label ?? "").trim().split("\n")[0].trim();
    if (!entry.title && first) entry.title = first;
    entry.xsec_token ??= params.get("xsec_token");
    seen.set(id, entry);
  }
  return [...seen.values()];
}

/** A creator as given: a 24-character id, or a profile link
 * (`https://www.xiaohongshu.com/user/profile/<id>...`, or rednote.com) */
export function creatorId(given) {
  const t = String(given ?? "").trim();
  const m = /\/user\/profile\/([^/?#]+)/.exec(t);
  const id = m ? m[1] : t;
  if (!/^[0-9a-f]{24}$/.test(id))
    throw new Error(
      `${JSON.stringify(t)} is not a creator: give the profile link (https://www.xiaohongshu.com/user/profile/<id>) or the 24-character id`,
    );
  return id;
}

/** A creator's id as a folder name: refused rather than escaped */
export function folderOf(id) {
  if (!/^[0-9a-f]{24}$/.test(id ?? ""))
    throw new Error(`not a creator id: ${id}`);
  return id;
}

/** One note as a record of the file: the note with its comments, the
 * Salmon Run terms matched, and when it was captured */
export function record(note, matched, capturedAt = new Date()) {
  const comment = (c) => ({
    id: c.id,
    author: c.author,
    date: c.date ?? null,
    text: c.text,
    likes: c.likes ?? 0,
    location: c.location ?? null,
    reply_to: c.reply_to ?? null,
    reply_to_author: c.reply_to_author ?? null,
    replies: (c.replies ?? []).map(comment),
    replies_total: c.replies_total ?? 0,
  });
  return {
    source: SOURCE,
    id: note.id,
    url: noteUrl(note.id),
    author: note.author,
    date: note.date ?? null,
    updated: note.updated ?? null,
    kind: note.kind ?? "",
    title: note.title ?? "",
    text: note.text ?? "",
    tags: note.tags ?? [],
    images: note.images ?? [],
    video: note.video ?? null,
    likes: note.likes ?? 0,
    collects: note.collects ?? 0,
    shares: note.shares ?? 0,
    comment_count: note.comment_count ?? 0,
    comments: (note.comments ?? []).map(comment),
    comments_complete: note.comments_complete === true,
    matched,
    captured_at: capturedAt.toISOString(),
  };
}

/** Appends `records` to the creator's file under `dir` (the `rednote`
 * folder of the inbox); returns the file's path */
export function append(dir, userId, records) {
  const folder = join(dir, folderOf(userId));
  mkdirSync(folder, { recursive: true });
  const path = join(folder, NOTES_FILE);
  appendFileSync(path, records.map((r) => JSON.stringify(r) + "\n").join(""));
  return path;
}

// ------------------------------------------------------- page scripts
// Each begins with a marker comment, so a fake page in tests can tell
// them apart

/** The page's server state with Vue refs unwrapped: its `note` and `user`
 * parts, as an object (null without a state) */
export const JS_STATE = `/* rncap:state */ (() => {
  const s = window.__INITIAL_STATE__;
  if (!s) return null;
  const seen = new WeakSet();
  const un = (v, d) => {
    if (v === null || typeof v !== "object") return v;
    if (d > 14) return null;
    if (v.__v_isRef) return un(v._value !== undefined ? v._value : v._rawValue, d + 1);
    if (Array.isArray(v)) return v.map((x) => un(x, d + 1));
    if (seen.has(v)) return null;
    seen.add(v);
    const o = {};
    for (const k of Object.keys(v)) {
      if (k.startsWith("__v_")) continue;
      o[k] = un(v[k], d + 1);
    }
    return o;
  };
  const note = s.note ? { noteDetailMap: un(s.note.noteDetailMap, 0) } : null;
  const user = s.user
    ? { notes: un(s.user.notes, 0), userInfo: un(s.user.userInfo, 0), loggedIn: un(s.user.loggedIn, 0) }
    : null;
  return { note, user };
})()`;

/** Every link of the page: `[address, text]` */
export const JS_LINKS = `/* rncap:links */ [...document.querySelectorAll("a[href]")].map((a) => [a.href, (a.innerText || "").trim().slice(0, 200)])`;

/** The note the page shows, read off the DOM when no answer gave it: the
 * site's detail page has the title in \`#detail-title\`, the text in
 * \`#detail-desc\`, the author and date near them, the comments as
 * \`.comment-item\` (a reply sits in a \`.reply-container\`) */
export const JS_NOTE_DOM = `/* rncap:note-dom */ (() => {
  const q = (sel, root) => (root || document).querySelector(sel);
  const txt = (el) => (el ? (el.innerText || "").trim() : "");
  const title = txt(q("#detail-title")) || txt(q(".note-content .title"));
  const desc = txt(q("#detail-desc")) || txt(q(".note-content .desc"));
  if (!title && !desc) return null;
  const author = txt(q(".author-wrapper .username")) || txt(q(".author-wrapper .name"));
  const date = txt(q(".bottom-container .date")) || txt(q(".note-content .date"));
  const comments = [...document.querySelectorAll(".comment-item")].slice(0, 400).map((c) => ({
    author: txt(q(".author .name", c)) || txt(q(".name", c)),
    text: txt(q(".content", c)),
    date: txt(q(".date", c)),
    reply: !!c.closest(".reply-container"),
  }));
  return { title, desc, author, date, comments };
})()`;

/** A note out of what `JS_NOTE_DOM` read: the page's title, text, author
 * and date, the comments as roots with the replies under the root before
 * them; null when the page showed no note */
export function noteFromDom(dom, id, creator) {
  if (!dom) return null;
  const s = (k) => (typeof dom[k] === "string" ? dom[k].trim() : "");
  const day = /^\d{4}-\d{2}-\d{2}/.exec(s("date"));
  const note = {
    id,
    url: noteUrl(id),
    author: {
      user_id: creator.user_id,
      nickname: s("author") || creator.nickname,
    },
    date: day ? new Date(`${day[0]}T00:00:00Z`).toISOString() : null,
    updated: null,
    kind: "",
    title: s("title"),
    text: s("desc"),
    tags: [],
    images: [],
    video: null,
    likes: 0,
    collects: 0,
    shares: 0,
    comment_count: 0,
    comments: [],
    comments_complete: true,
  };
  let root = null;
  (dom.comments ?? []).forEach((c, i) => {
    const comment = {
      id: `dom-${i}`,
      author: { user_id: "", nickname: c.author ?? "" },
      date: null,
      text: c.text ?? "",
      likes: 0,
      location: null,
      reply_to: null,
      reply_to_author: null,
      replies: [],
      replies_total: 0,
    };
    if (!comment.text) return;
    if (c.reply && root) root.replies.push(comment);
    else {
      note.comments.push(comment);
      root = comment;
    }
  });
  note.comment_count = note.comments.reduce(
    (n, c) => n + 1 + c.replies.length,
    0,
  );
  return note;
}
