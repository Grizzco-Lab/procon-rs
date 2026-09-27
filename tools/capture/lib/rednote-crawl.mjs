// The visit on Xiaohongshu: the account's following list (found once from
// its own profile, kept in the state), each followed creator's notes list
// (scrolled until the end, or on later visits until it shows only notes
// seen before), and each new note about Salmon Run, opened from its tile,
// its comments scrolled and folded reply threads unfolded, then closed.
// Notes come from the JSON the page loads for itself and from the page's
// server state; the DOM is the fallback. Every navigation, scroll, click
// and key is one paced action (`Pace`); the run stops at a cap, on a
// blocked answer, or as soon as a page wants a person (a captcha, a
// slider, a login prompt), which is never solved. The page is an
// interface (`Page` of cdp.mjs, or a fake in tests).

import { matches } from "./filter.mjs";
import { Stop } from "./pace.mjs";
import * as rn from "./rednote.mjs";
import * as state from "./state.mjs";

export const DEFAULTS = Object.freeze({
  /** Creators to read (user ids) instead of the following list */
  creators: null,
  /** Read the following list again although the state has it */
  refreshFollowing: false,
  /** Browse and print, write nothing */
  dryRun: false,
  /** What decides that a note is about Salmon Run: `title` (only those
   * are opened) or `detail` (every new note is opened; its title, text
   * and tags decide) */
  match: "title",
  /** Notes kept this run, at most */
  maxNotes: 60,
  /** Comments (with replies) loaded per note, at most */
  maxComments: 200,
  /** Reply threads unfolded per note, at most */
  maxReplies: 10,
  /** Scrolls down a creator's list, at most */
  listScrolls: 40,
  /** Scrolls down a note's comments, at most */
  commentScrolls: 15,
  /** The following list is read again after this many days */
  followingTtlDays: 7,
});

/** HTTP statuses the site answers to what it takes for a script */
const BLOCKED = [403, 461, 471];

/** A note tile of the account's own profile, and a note's comment box */
const OWN_NOTE = "section.note-item a.cover";
const COMMENT_BOX = "#content-textarea";

const isFollowings = (r) => /intimacy_list|follow/.test(r.url);
const isList = (r) => /user_posted/.test(r.url);
const isNoteAnswer = (r) => /comment\/page|\/feed/.test(r.url);
const isComments = (r) => /comment/.test(r.url);
const isReplies = (r) => /comment\/sub/.test(r.url);

export class Crawl {
  /**
   * @param {object} page the browser page (cdp.mjs `Page`)
   * @param {object} deps `pace` (a `Pace`), `state` (the loaded state),
   *   `dir` (the inbox's `rednote` folder), `log(line)`, `options`, `now()`
   */
  constructor(page, deps) {
    this.page = page;
    this.pace = deps.pace;
    this.state = deps.state;
    this.dir = deps.dir;
    this.log = deps.log ?? (() => {});
    this.options = { ...DEFAULTS, ...deps.options };
    /** The site's origin the home page landed on */
    this.origin = this.options.site ?? rn.SITE;
    this.now = deps.now ?? (() => new Date());
    this.summary = {
      creators: 0,
      listed: 0,
      fresh: 0,
      kept: 0,
      offTopic: 0,
      failed: 0,
      comments: 0,
      files: new Set(),
    };
  }

  /** One paced page action, then a look at whether the page wants a
   * person */
  async action(f) {
    await this.pace.before(this.state.day);
    const result = await f();
    const why = rn.challenge(
      await this.page.location(),
      await this.page.text(),
    );
    if (why) throw new Stop("challenge", why);
    return result;
  }

  /** Drains the page's answers, recognised; a blocked status or a
   * refused answer stops the run */
  drain() {
    const out = [];
    for (const r of this.page.drain()) {
      if (BLOCKED.includes(r.status))
        throw new Stop(
          "blocked",
          `the site answered HTTP ${r.status} to ${r.url}: it takes this for a script`,
        );
      const p = rn.recognise(r.url, r.json);
      if (!p) continue;
      if (p.kind === "error") {
        if (rn.refused(p.code))
          throw new Stop(
            "login",
            `the site answered code ${p.code} "${p.msg}" to ${r.url}: the session is not accepted; run login again`,
          );
        continue;
      }
      out.push(p);
    }
    return out;
  }

  save() {
    if (!this.options.dryRun) state.save(this.dir, this.state);
  }

  /** Runs the whole visit; the state is saved as it goes. Returns the
   * summary, with `stopped` set when a `Stop` ended it early. */
  async run() {
    try {
      const me = await this.start();
      const creators = await this.following(me);
      for (const id of this.order(creators)) {
        if (this.summary.kept >= this.options.maxNotes) break;
        await this.creator(id);
      }
    } catch (error) {
      if (!(error instanceof Stop)) throw error;
      this.summary.stopped = error;
    } finally {
      this.save();
    }
    return this.summary;
  }

  /** The home page: is the profile logged in, and as whom (the site's
   * `user/me` answer, else the page state); a guest stops the run */
  async start() {
    const payloads = await this.action(async () => {
      await this.page.goto(this.origin);
      await this.page.waitFor((r) => /user\/me/.test(r.url), 8000);
      this.origin = rn.originOf(await this.page.location()) ?? this.origin;
      return this.drain();
    });
    // undefined: nothing told; null: a guest; a string: the account's id
    let known;
    for (const p of payloads)
      if (p.kind === "me") known = p.guest ? null : p.user_id;
    if (known === undefined) {
      const st = await this.page.evaluate(rn.JS_STATE);
      const info = st?.user?.userInfo;
      const id = rn.field(info, ["userId", "user_id"]);
      const name = rn.field(info, ["nickname", "redId", "red_id"]);
      if (typeof name === "string" && name && typeof id === "string")
        known = id;
      else if (typeof st?.user?.loggedIn === "boolean")
        known = st.user.loggedIn ? "" : null;
    }
    if (known === null)
      throw new Stop("login", "the site sees a guest: run `login` first");
    if (known === undefined)
      throw new Stop("login", "could not confirm the login: run `login` first");
    const me = this.options.me ?? (known || this.state.following.me);
    this.log(me ? `logged in as ${me}` : "logged in");
    return me || null;
  }

  /** The creators to visit: `--creators`, else the following list from
   * the state when fresh, else read from the account's own profile */
  async following(me) {
    if (this.options.creators?.length) {
      for (const id of this.options.creators) state.account(this.state, id);
      return this.options.creators;
    }
    const f = this.state.following;
    const ageDays = f.at
      ? (this.now() - new Date(f.at)) / 86_400_000
      : Infinity;
    if (
      f.handles.length &&
      ageDays < this.options.followingTtlDays &&
      !this.options.refreshFollowing
    ) {
      this.log(
        `following list of ${f.handles.length} creators from ${f.at.slice(0, 10)}`,
      );
      return f.handles;
    }
    if (!me)
      throw new Stop(
        "empty",
        "the account's id is not known, so its following list cannot be opened; give the creators with --creators (their profile links)",
      );
    const found = new Map();
    const take = () => {
      for (const p of this.drain()) {
        if (p.kind !== "followings") continue;
        for (const u of p.users)
          if (u.user_id !== me && !found.has(u.user_id))
            found.set(u.user_id, u);
      }
    };
    await this.action(() =>
      this.page.goto(`${this.origin}/user/profile/${me}`),
    );
    take();
    // The web profile does not open the following list; the @ picker of a
    // note's comment box lists the accounts followed. One of the account's
    // own notes is opened, "@" typed and taken back, nothing is sent.
    const opened = await this.action(() => this.page.click(OWN_NOTE));
    if (!opened)
      throw new Stop(
        "empty",
        "no note of yours to open the comment box of; give the creators with --creators (their profile links)",
      );
    await this.action(() => this.page.click(COMMENT_BOX));
    await this.action(async () => {
      await this.page.type("@");
      await this.page.waitFor(isFollowings, 8000);
    });
    take();
    await this.page.key("Backspace", 8);
    await this.page.key("Escape", 27);
    if (!found.size)
      throw new Stop(
        "empty",
        "the following list gave no accounts; give the creators with --creators (their profile links)",
      );
    for (const [id, u] of found)
      if (u.nickname) state.account(this.state, id).nickname = u.nickname;
    this.state.following = {
      me,
      handles: [...found.keys()],
      at: this.now().toISOString(),
    };
    this.save();
    this.log(`following ${found.size} creators`);
    await this.action(() => this.page.key("Escape", 27));
    return this.state.following.handles;
  }

  /** Creators never visited first, then the longest unvisited */
  order(ids) {
    const at = (id) => this.state.accounts[id]?.visited_at ?? "";
    return [...ids].sort((a, b) =>
      at(a) < at(b) ? -1 : at(a) > at(b) ? 1 : 0,
    );
  }

  /** The listed notes of a creator's page as it stands: its `user_posted`
   * answers, its server state and its links (a tile whose link names
   * another author is not the creator's). Returns how many were new. */
  async listedNow(id, payloads, into, hasMore) {
    const found = [];
    for (const p of payloads)
      if (p.kind === "list") {
        hasMore.value = p.has_more;
        found.push(...p.notes);
      }
    found.push(...rn.listedFromState(await this.page.evaluate(rn.JS_STATE)));
    found.push(...rn.listedFromLinks(await this.page.evaluate(rn.JS_LINKS)));
    let added = 0;
    for (const l of found) {
      if (l.author_id && l.author_id !== id) continue;
      const have = into.get(l.id);
      if (have) {
        have.title ||= l.title;
        have.xsec_token ??= l.xsec_token;
      } else {
        into.set(l.id, l);
        added++;
      }
    }
    return added;
  }

  /** One creator: the list, then the new notes about Salmon Run */
  async creator(id) {
    const record = state.account(this.state, id);
    const profile = `${this.origin}/user/profile/${id}`;
    const seen = this.state.seen;
    const listed = new Map();
    const hasMore = { value: null };
    this.log(`creator ${record.nickname ? `${record.nickname} ` : ""}${id}`);
    await this.action(() => this.page.goto(profile));
    await this.page.waitFor(isList, 8000);
    await this.listedNow(id, this.drain(), listed, hasMore);
    let idle = 0;
    let reachedEnd = hasMore.value === false;
    for (let i = 0; i < this.options.listScrolls && !reachedEnd; i++) {
      // Later visits stop once the page shows only notes seen before: new
      // notes are on top
      if (
        record.listed_to_end &&
        listed.size &&
        [...listed.keys()].every((n) => seen[n])
      )
        break;
      await this.action(() => this.page.scroll());
      await this.page.waitFor(isList, 5000);
      const added = await this.listedNow(id, this.drain(), listed, hasMore);
      idle = added === 0 ? idle + 1 : 0;
      reachedEnd = hasMore.value === false || idle >= 2;
    }
    if (reachedEnd) record.listed_to_end = true;
    record.visited_at = this.now().toISOString();
    this.summary.creators++;
    this.summary.listed += listed.size;
    const fresh = [...listed.values()].filter((l) => !seen[l.id]);
    this.summary.fresh += fresh.length;
    this.log(
      `  ${listed.size} notes listed, ${fresh.length} new${reachedEnd ? "" : " (list not finished)"}`,
    );
    for (const l of fresh) {
      const byTitle = matches(l.title);
      if (this.options.match === "title" && !byTitle.length) {
        seen[l.id] = "off-topic";
        record.off_topic++;
        this.summary.offTopic++;
        continue;
      }
      if (this.summary.kept >= this.options.maxNotes) break;
      await this.note(id, record, l, byTitle, profile);
    }
    this.save();
  }

  /** Adds the comments of the answers about note `id` to `roots` (a page
   * of replies to the root it names), each once; `hasMore` follows the
   * last page of root comments. Returns how many were new. */
  merge(payloads, id, roots, ids, hasMore) {
    let added = 0;
    for (const p of payloads) {
      if (p.kind !== "comments") continue;
      if (p.note_id && p.note_id !== id) continue;
      if (p.root) {
        const root = roots.find((r) => r.id === p.root);
        if (!root) continue;
        for (const c of p.comments)
          if (!root.replies.some((x) => x.id === c.id)) {
            root.replies.push(c);
            added++;
          }
      } else {
        hasMore.value = p.has_more;
        for (const c of p.comments)
          if (!ids.has(c.id)) {
            ids.add(c.id);
            roots.push(c);
            added++;
          }
      }
    }
    return added;
  }

  /** One note: opened from its tile on the creator's page (else by its
   * address), read from the page state, the feed answer or the DOM, its
   * comments scrolled and reply threads unfolded within the caps, kept
   * when about Salmon Run, then closed */
  async note(creator, record, l, byTitle, profile) {
    const id = l.id;
    let opened = false;
    if ((await this.page.location()).includes(creator)) {
      opened = await this.action(() => this.page.click(`a[href*="${id}"]`));
      if (opened) opened = (await this.page.location()).includes(id);
    }
    if (!opened)
      await this.action(() => this.page.goto(rn.noteUrl(id, l.xsec_token)));
    await this.page.waitFor(isNoteAnswer, 8000);
    const payloads = this.drain();
    const st = await this.page.evaluate(rn.JS_STATE);
    let note =
      rn.notesFromState(st).find((n) => n.id === id) ??
      payloads
        .flatMap((p) => (p.kind === "notes" ? p.notes : []))
        .find((n) => n.id === id) ??
      rn.noteFromDom(await this.page.evaluate(rn.JS_NOTE_DOM), id, {
        user_id: creator,
        nickname: record.nickname ?? "",
      });
    if (!note) {
      this.state.seen[id] = "failed";
      this.summary.failed++;
      this.log(`  ${id}: the page showed no note; skipped`);
      await this.close(profile);
      return;
    }
    note.author.user_id ||= creator;
    note.author.nickname ||= record.nickname ?? "";
    if (note.author.nickname && !record.nickname)
      record.nickname = note.author.nickname;
    // The comments: what came with the page, then more by scrolling the
    // comments pane, then the folded reply threads
    const roots = note.comments;
    const ids = new Set(roots.map((c) => c.id));
    const hasMore = { value: !note.comments_complete };
    const total = () => roots.reduce((n, c) => n + 1 + c.replies.length, 0);
    this.merge(payloads, id, roots, ids, hasMore);
    let idle = 0;
    for (
      let i = 0;
      i < this.options.commentScrolls &&
      hasMore.value &&
      idle < 2 &&
      total() < this.options.maxComments;
      i++
    ) {
      await this.action(() => this.page.scroll({ selector: ".note-scroller" }));
      await this.page.waitFor(isComments, 5000);
      idle =
        this.merge(this.drain(), id, roots, ids, hasMore) === 0 ? idle + 1 : 0;
    }
    for (
      let i = 0;
      i < this.options.maxReplies && total() < this.options.maxComments;
      i++
    ) {
      const clicked = await this.action(() =>
        this.page.clickText(rn.PAGE.moreReplies),
      );
      if (!clicked) break;
      await this.page.waitFor(isReplies, 5000);
      this.merge(this.drain(), id, roots, ids, hasMore);
    }
    note.comments = roots;
    note.comments_complete =
      !hasMore.value && roots.every((c) => c.replies.length >= c.replies_total);
    const matched = [
      ...new Set([
        ...byTitle,
        ...matches([note.title, note.text, ...note.tags].join("\n")),
      ]),
    ];
    const title = (note.title || note.text).replace(/\s+/g, " ").slice(0, 60);
    if (!matched.length) {
      this.state.seen[id] = "off-topic";
      record.off_topic++;
      this.summary.offTopic++;
      this.log(`  ${id} "${title}": not about Salmon Run`);
      await this.close(profile);
      return;
    }
    const line = rn.record(note, matched, this.now());
    this.state.seen[id] = "kept";
    record.kept++;
    this.summary.kept++;
    this.summary.comments += total();
    this.log(
      `  ${this.options.dryRun ? "would keep" : "kept"} ${rn.noteUrl(id)} (${total()} comments${note.comments_complete ? "" : ", more not loaded"}): ${title}`,
    );
    if (!this.options.dryRun) {
      this.summary.files.add(rn.append(this.dir, creator, [line]));
      this.save();
    }
    await this.close(profile);
  }

  /** Leaves a note: Escape closes one opened over the creator's list; a
   * page of its own is left by opening the list again */
  async close(profile) {
    await this.action(() => this.page.key("Escape", 27));
    const creator = profile.split("/").pop();
    if (!(await this.page.location()).includes(creator))
      await this.action(() => this.page.goto(profile));
  }
}
