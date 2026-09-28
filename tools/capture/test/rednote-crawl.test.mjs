// The visit against a scripted page: which addresses are opened, clicked
// and scrolled, what is kept, what the state remembers, and what stops a
// run
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Pace } from "../lib/pace.mjs";
import { choose, Crawl, shuffle, unread } from "../lib/rednote-crawl.mjs";
import * as rn from "../lib/rednote.mjs";
import * as state from "../lib/state.mjs";
import { commentsFixture, feedFixture, stateFixture } from "./rednote.test.mjs";

const NOW = new Date("2026-09-27T12:00:00Z");
const API = "https://edith.xiaohongshu.com/api/sns/web";
const SITE = rn.SITE;
const A = "5f0000000000000000000001";
const B = "5f0000000000000000000002";
const SR = "66aa00000000000000000001";
const SR2 = "66aa00000000000000000002";
const CAT = "66aa00000000000000000003";
const PROFILE_A = `${SITE}/user/profile/${A}`;
const PROFILE_B = `${SITE}/user/profile/${B}`;
const NOTE = (id) => `${SITE}/user/profile/${A}/${id}?xsec_token=t`;

const answer = (url, json, status = 200) => ({ url, status, json });
const me = (guest) =>
  answer(`${API}/v2/user/me`, {
    code: 0,
    success: true,
    data: guest
      ? { user_id: "me1", guest }
      : { user_id: "me1", guest, nickname: "Me" },
  });
const list = (notes, hasMore) =>
  answer(`${API}/v1/user_posted?num=30&cursor=&user_id=${A}`, {
    code: 0,
    success: true,
    data: {
      has_more: hasMore,
      cursor: "c",
      notes: notes.map(([id, title]) => ({
        note_id: id,
        type: "normal",
        display_title: title,
        xsec_token: `tok-${id}`,
        user: { user_id: A, nickname: "Grizzco Coach" },
      })),
    },
  });
/** The @ picker's accounts, as `intimacy_list` answers */
const followings = (users) =>
  answer(`${API}/v1/intimacy/intimacy_list`, {
    code: 0,
    data: {
      items: users.map(([id, n]) => ({
        userid: `${id}_3e369b8256d5996e6507223766aaadd1`,
        nickname: n,
        rid: "1",
      })),
    },
  });

/** A page whose answers are scripted per address: what arrives on
 * opening it, per scroll, per click on a tile (which moves the page), per
 * click on a text (the following count, "more replies"), and what its
 * scripts return */
class FakePage {
  constructor(script) {
    this.script = script;
    this.queue = [];
    this.visited = [];
    this.here = null;
    this.scrolls = 0;
  }
  entry() {
    return this.script[this.here] ?? {};
  }
  async goto(url) {
    this.here = url;
    this.visited.push(url);
    this.scrolls = 0;
    this.queue.push(...(this.entry().responses ?? []));
  }
  async scroll({ selector, up = false } = {}) {
    this.visited.push(
      selector ? `scroll ${selector}` : up ? "scroll up" : "scroll",
    );
    const batches = this.entry().scrolls ?? [];
    this.queue.push(...(batches[this.scrolls++] ?? []));
    // The links the page draws after scrolling up, when the script says
    if (up && this.entry().linksAfterScrollUp)
      this.entry().links = this.entry().linksAfterScrollUp;
  }
  async click(selector) {
    this.visited.push(`click ${selector}`);
    const c = this.entry().clicks?.[selector];
    if (!c) return false;
    this.here = c.location;
    this.scrolls = 0;
    this.queue.push(...(c.responses ?? []));
    return true;
  }
  async clickText(words, { digit = false } = {}) {
    this.visited.push(digit ? "click count" : "click more");
    const batches = digit ? this.entry().count : this.entry().more;
    if (!batches?.length) return false;
    this.queue.push(...batches.shift());
    return true;
  }
  async type(text) {
    this.visited.push(`type ${text}`);
    this.queue.push(...(this.entry().typed ?? []));
  }
  async key(key) {
    this.visited.push(`key ${key}`);
    if (this.entry().escapeTo) this.here = this.entry().escapeTo;
  }
  drain() {
    return this.queue.splice(0);
  }
  async waitFor(test) {
    return this.queue.some(test);
  }
  async location() {
    return this.entry().location ?? this.here;
  }
  async text() {
    return this.entry().text ?? "a page";
  }
  async evaluate(js) {
    if (js.includes("rncap:state")) return this.entry().state ?? null;
    if (js.includes("rncap:links")) return this.entry().links ?? [];
    if (js.includes("rncap:note-dom")) return this.entry().dom ?? null;
    throw new Error(`unexpected script: ${js.slice(0, 40)}`);
  }
}

const quick = () =>
  new Pace(
    { maxActions: null, dailyCap: null },
    { sleep: async () => true, random: () => 0.5 },
  );

/** A crawl on `page`; its random source just under 1 draws the largest
 * share of the unread notes and shuffles nothing (the list's order) */
function crawl(
  page,
  dir,
  options = {},
  st = state.load(dir, rn.TOOL),
  random = () => 0.999,
) {
  const lines = [];
  const c = new Crawl(page, {
    pace: quick(),
    state: st,
    dir,
    log: (l) => lines.push(l),
    options,
    now: () => NOW,
    random,
  });
  c.lines = lines;
  return c;
}

const scratch = () =>
  join(mkdtempSync(join(tmpdir(), "rncap-crawl-")), "rednote");

/** A first visit of creator A: two Salmon Run notes (one listed after a
 * scroll), a cat; the first note's comments over the page's answer, a
 * scroll and an unfolded reply thread, the second note read off the DOM */
function firstVisit() {
  const comments2 = answer(`${API}/v2/comment/page?note_id=${SR}&cursor=c2`, {
    code: 0,
    success: true,
    data: {
      has_more: false,
      comments: [
        {
          id: "c3",
          content: "third",
          create_time: 1725040000000,
          like_count: 0,
          user_info: { user_id: "u5", nickname: "eve" },
        },
      ],
    },
  });
  const replies = answer(
    `${API}/v2/comment/sub/page?note_id=${SR}&root_comment_id=c1&num=10&cursor=s1`,
    {
      code: 0,
      success: true,
      data: {
        has_more: false,
        comments: [
          {
            id: "c1-2",
            content: "second reply",
            create_time: 1725050000000,
            like_count: 0,
            user_info: { user_id: "u6", nickname: "fay" },
            target_comment: { id: "c1", user_info: { nickname: "alice" } },
          },
          {
            id: "c1-1",
            content: "dup of the first",
            user_info: { nickname: "bob" },
          },
        ],
      },
    },
  );
  return {
    [SITE]: { responses: [me(false)] },
    [PROFILE_A]: {
      responses: [
        list(
          [
            [SR, "打工400分教学"],
            [CAT, "My cat"],
          ],
          true,
        ),
      ],
      scrolls: [[list([[SR2, "Salmon Run W3 plan"]], false)]],
      clicks: {
        [`a[href*="${SR}"]`]: {
          location: NOTE(SR),
          responses: [
            answer(`${API}/v1/feed`, feedFixture()),
            answer(
              `${API}/v2/comment/page?note_id=${SR}&cursor=`,
              commentsFixture(true),
            ),
          ],
        },
        [`a[href*="${SR2}"]`]: { location: NOTE(SR2), responses: [] },
        [`a[href*="${CAT}"]`]: {
          location: NOTE(CAT),
          responses: [
            answer(`${API}/v1/feed`, {
              code: 0,
              success: true,
              data: {
                items: [
                  {
                    note_card: {
                      note_id: CAT,
                      title: "My cat",
                      desc: "purrs",
                      user: { user_id: A },
                    },
                  },
                ],
              },
            }),
          ],
        },
      },
    },
    [NOTE(SR)]: {
      scrolls: [[comments2]],
      more: [[replies]],
      escapeTo: PROFILE_A,
    },
    [NOTE(SR2)]: {
      dom: {
        title: "Salmon Run W3 plan",
        desc: "Left side first.",
        author: "Grizzco Coach",
        date: "2025-01-02",
        comments: [
          { author: "gil", text: "yes", date: "", reply: false },
          { author: "hal", text: "no", date: "", reply: true },
        ],
      },
      escapeTo: PROFILE_A,
    },
    [NOTE(CAT)]: { escapeTo: PROFILE_A },
  };
}

test("a first visit: the list, every note with its comments, matched or not", async () => {
  const dir = scratch();
  const page = new FakePage(firstVisit());
  const c = crawl(page, dir, { creators: [A] });
  const s = await c.run();
  assert.equal(s.stopped, undefined, c.lines.join("\n"));
  assert.deepEqual(
    [s.creators, s.listed, s.fresh, s.kept, s.offTopic, s.failed],
    [1, 3, 3, 3, 1, 0],
  );
  assert.deepEqual(page.visited, [
    SITE,
    PROFILE_A,
    "scroll",
    // The first note: its tile, the comments pane scrolled once (the
    // second page says it is the last), a reply thread unfolded, then
    // none left, closed
    `click a[href*="${SR}"]`,
    "scroll .note-scroller",
    "click more",
    "click more",
    "key Escape",
    // The cat, read like the others (list order: nothing is shuffled
    // here): comments scrolled up to the cap of 3 scrolls, then twice
    // without news
    `click a[href*="${CAT}"]`,
    "scroll .note-scroller",
    "scroll .note-scroller",
    "click more",
    "key Escape",
    // The last note: read off the page, nothing to scroll
    `click a[href*="${SR2}"]`,
    "click more",
    "key Escape",
  ]);
  const lines = readFileSync(join(dir, A, "notes.jsonl"), "utf8")
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l));
  assert.deepEqual(
    lines.map((l) => l.id),
    [SR, CAT, SR2],
  );
  const sr = lines[0];
  // Comments of the page's answer, the scrolled page and the unfolded
  // replies, each once
  assert.deepEqual(
    sr.comments.map((x) => x.id),
    ["c1", "c2", "c3"],
  );
  assert.deepEqual(
    sr.comments[0].replies.map((r) => r.author.nickname),
    ["bob", "fay"],
  );
  assert.equal(sr.comments_complete, true);
  assert.equal(sr.author.nickname, "Grizzco Coach");
  assert.deepEqual(sr.matched, ["打工", "炸弹鱼"]);
  assert.equal(sr.on_topic, true);
  // Saved although the glossary finds no term in it
  assert.deepEqual(lines[1].matched, []);
  assert.equal(lines[1].on_topic, false);
  const sr2 = lines[2];
  assert.equal(sr2.text, "Left side first.");
  assert.equal(sr2.author.user_id, A);
  assert.equal(sr2.date, "2025-01-02T00:00:00.000Z");
  assert.equal(sr2.comments.length, 1);
  assert.equal(sr2.comments[0].replies.length, 1);
  assert.deepEqual(sr2.matched, ["salmon run"]);
  const st = state.load(dir, rn.TOOL);
  assert.equal(st.tool, "rncap");
  assert.deepEqual(st.seen, {
    [SR]: "kept",
    [SR2]: "kept",
    [CAT]: "kept",
  });
  assert.deepEqual(st.accounts[A].notes, [SR, CAT, SR2]);
  assert.deepEqual(st.accounts[A].tokens, {});
  assert.equal(st.accounts[A].kept, 3);
  assert.equal(st.accounts[A].off_topic, 1);
  assert.equal(st.accounts[A].listed_to_end, true);
  assert.equal(st.accounts[A].nickname, "Grizzco Coach");
  assert.equal(st.day.actions, 16);
  assert.ok(
    c.lines.some((l) => l.includes(`kept ${rn.noteUrl(SR)} (5 comments)`)),
    c.lines.join("\n"),
  );

  // A second run: the list shows only notes known, all read: nothing is
  // opened
  const again = new FakePage(firstVisit());
  const s2 = await crawl(again, dir, { creators: [A] }).run();
  assert.deepEqual(again.visited, [SITE, PROFILE_A]);
  assert.deepEqual([s2.kept, s2.fresh], [0, 0]);
  assert.equal(state.load(dir, rn.TOOL).day.actions, 18);
  rmSync(dir, { recursive: true });
});

test("a dry run browses but writes only the day's action count", async () => {
  const dir = scratch();
  const page = new FakePage(firstVisit());
  const c = crawl(page, dir, { creators: [A], dryRun: true });
  const s = await c.run();
  assert.equal(s.kept, 3);
  assert.ok(page.visited.includes(`click a[href*="${SR}"]`));
  assert.ok(c.lines.some((l) => l.includes("would keep")));
  assert.equal(existsSync(join(dir, A)), false);
  assert.equal(s.files.size, 0);
  // Its page actions count against the daily cap; nothing else is kept
  const st = state.load(dir, rn.TOOL);
  assert.equal(st.day.actions, 16);
  assert.deepEqual(st.seen, {});
  assert.deepEqual(st.accounts, {});
  // A real run after it carries the count on
  await crawl(new FakePage(firstVisit()), dir, { creators: [A] }).run();
  assert.equal(state.load(dir, rn.TOOL).day.actions, 32);
  rmSync(dir, { recursive: true });
});

test("a tile the list no longer draws is scrolled toward, else the note is opened by its address", async () => {
  const dir = scratch();
  const script = firstVisit();
  const tile = (id) => [`${SITE}/user/profile/${A}/${id}?xsec_token=t`, ""];
  // After the list's scroll, only the last notes' tiles are drawn: the
  // first note is above them, so the page is scrolled up until its tile
  // shows; the second is drawn already
  script[PROFILE_A].links = [tile(SR2), tile(CAT)];
  script[PROFILE_A].linksAfterScrollUp = [tile(SR), tile(SR2), tile(CAT)];
  const page = new FakePage(script);
  const c = crawl(page, dir, { creators: [A] });
  const s = await c.run();
  assert.equal(s.stopped, undefined, c.lines.join("\n"));
  assert.equal(s.kept, 3);
  assert.deepEqual(page.visited.slice(2, 6), [
    "scroll",
    "scroll up",
    `click a[href*="${SR}"]`,
    "scroll .note-scroller",
  ]);
  assert.ok(page.visited.includes(`click a[href*="${SR2}"]`));
  assert.ok(!c.lines.some((l) => l.includes("did not open")));

  // Never drawn within the scrolls: opened by its address, on the origin
  const stuck = firstVisit();
  stuck[PROFILE_A].links = [tile(SR2), tile(CAT)];
  stuck[NOTE(SR)].location = `${SITE}/explore/${SR}`;
  stuck[rn.noteUrl(SR, `tok-${SR}`)] = stuck[NOTE(SR)];
  const page2 = new FakePage(stuck);
  const dir2 = scratch();
  const c2 = crawl(page2, dir2, { creators: [A], tileScrolls: 2 });
  await c2.run();
  assert.deepEqual(page2.visited.slice(2, 6), [
    "scroll",
    "scroll up",
    "scroll up",
    rn.noteUrl(SR, `tok-${SR}`),
  ]);
  assert.ok(c2.lines.some((l) => l.includes("did not open")));
  rmSync(dir, { recursive: true });
  rmSync(dir2, { recursive: true });
});

/** Creator A with `ids` listed on one page, each note's tile opening a
 * note read off the page */
function manyNotes(ids) {
  const script = {
    [SITE]: { responses: [me(false)] },
    [PROFILE_A]: {
      responses: [
        list(
          ids.map((id) => [id, "Salmon Run"]),
          false,
        ),
      ],
      clicks: {},
    },
  };
  for (const id of ids) {
    script[PROFILE_A].clicks[`a[href*="${id}"]`] = { location: NOTE(id) };
    script[NOTE(id)] = {
      dom: {
        title: `note ${id}`,
        desc: "",
        author: "",
        date: "",
        comments: [],
      },
      escapeTo: PROFILE_A,
    };
  }
  return script;
}

const ids = (n) =>
  Array.from({ length: n }, (_, i) => `66bb0000000000000000000${i}`);
const opened = (page) =>
  page.visited
    .filter((v) => v.startsWith("click a[href"))
    .map((v) => v.slice(15, -2));

test("a visit reads a random share of the unread notes, at most --per-creator; later visits finish the rest", async () => {
  const dir = scratch();
  const all = ids(5);
  // A random source of 0: the smallest share, 60% of 5 unread is 3
  const zero = () => 0;
  const page = new FakePage(manyNotes(all));
  const s = await crawl(page, dir, { creators: [A] }, undefined, zero).run();
  assert.equal(s.kept, 3);
  assert.equal(s.left, 2);
  const first = opened(page);
  assert.equal(new Set(first).size, 3);
  let st = state.load(dir, rn.TOOL);
  assert.deepEqual(st.accounts[A].notes, all);
  const left = all.filter((id) => !first.includes(id));
  assert.deepEqual(unread(st.accounts[A], st.seen), left);
  // The notes left keep their tokens, for their address
  assert.deepEqual(Object.keys(st.accounts[A].tokens).sort(), left);

  // The next visit: the list shows nothing new, so it is not scrolled;
  // the cap of one note a visit leaves one for later
  const page2 = new FakePage(manyNotes(all));
  const s2 = await crawl(page2, dir, { creators: [A], perCreator: 1 }).run();
  assert.equal(s2.kept, 1);
  assert.deepEqual(page2.visited.slice(0, 2), [SITE, PROFILE_A]);
  assert.ok(left.includes(opened(page2)[0]));
  st = state.load(dir, rn.TOOL);
  assert.equal(unread(st.accounts[A], st.seen).length, 1);

  // A note that appeared since is read before the ones left over
  const more = [`66bb00000000000000000009`, ...all];
  const page3 = new FakePage(manyNotes(more));
  const s3 = await crawl(page3, dir, { creators: [A], perCreator: 1 }).run();
  assert.deepEqual([s3.fresh, s3.kept, s3.left], [1, 1, 1]);
  assert.deepEqual(opened(page3), [more[0]]);
  assert.deepEqual(state.load(dir, rn.TOOL).accounts[A].notes, more);
  rmSync(dir, { recursive: true });
});

test("the notes chosen: the share, the cap, new ones first, a random order", () => {
  const u = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"];
  const opts = { share: [0.6, 0.95], cap: 12 };
  assert.equal(choose(u, [], opts, () => 0).length, 6);
  assert.equal(choose(u, [], opts, () => 0.999).length, 9);
  assert.equal(choose(u, [], { ...opts, cap: 4 }, () => 0.999).length, 4);
  assert.deepEqual(
    choose(["a"], [], opts, () => 0),
    ["a"],
  );
  assert.deepEqual(
    choose([], [], opts, () => 0),
    [],
  );
  // The new ones fill the visit first; the order is shuffled
  const picked = choose(u, ["i", "j"], { ...opts, cap: 3 }, () => 0);
  assert.equal(picked.length, 3);
  assert.ok(picked.includes("i") && picked.includes("j"));
  assert.deepEqual(
    shuffle([1, 2, 3, 4], () => 0),
    [2, 3, 4, 1],
  );
  assert.deepEqual(
    shuffle([1, 2, 3, 4], () => 0.999),
    [1, 2, 3, 4],
  );
});

test("creators never visited first, then those with notes left, the oldest visit first", () => {
  const dir = scratch();
  const st = state.fresh(rn.TOOL);
  const visit = (id, at, notes) =>
    Object.assign(state.account(st, id), { visited_at: at, notes });
  st.seen = { n1: "kept", n2: "kept", n3: "off-topic" };
  visit("done-old", "2026-09-01T00:00:00Z", ["n1"]);
  visit("left-new", "2026-09-20T00:00:00Z", ["n2", "n3"]);
  visit("left-old", "2026-09-10T00:00:00Z", ["n4"]);
  const c = crawl(new FakePage({}), dir, {}, st);
  assert.deepEqual(c.order(["done-old", "left-new", "never", "left-old"]), [
    "never",
    "left-old",
    "left-new",
    "done-old",
  ]);
});

test("a challenge stops at once; nothing is written", async () => {
  const dir = scratch();
  const script = firstVisit();
  script[NOTE(SR)].text = "请完成安全验证";
  const s = await crawl(new FakePage(script), dir, { creators: [A] }).run();
  assert.equal(s.stopped.reason, "challenge");
  assert.match(s.stopped.message, /the page says/);
  assert.equal(s.kept, 0);
  assert.equal(existsSync(join(dir, A)), false);
  // The note is not marked, so the next run takes it; the count is kept
  const st = state.load(dir, rn.TOOL);
  assert.equal(st.seen[SR], undefined);
  assert.equal(st.day.actions, 4);
  rmSync(dir, { recursive: true });
});

test("a refused answer, a blocked status and a guest stop the run", async () => {
  const dir = scratch();
  const refused = firstVisit();
  refused[PROFILE_A].clicks[`a[href*="${SR}"]`].responses = [
    answer(`${API}/v1/feed`, {
      code: 300012,
      success: false,
      msg: "please log in",
    }),
  ];
  let s = await crawl(new FakePage(refused), dir, { creators: [A] }).run();
  assert.equal(s.stopped.reason, "login");
  assert.match(s.stopped.message, /300012/);

  const blocked = firstVisit();
  blocked[PROFILE_A].responses = [
    answer(`${API}/v1/user_posted?user_id=${A}`, null, 461),
  ];
  s = await crawl(new FakePage(blocked), dir, { creators: [A] }).run();
  assert.equal(s.stopped.reason, "blocked");
  assert.match(s.stopped.message, /HTTP 461/);

  const guest = firstVisit();
  guest[SITE].responses = [me(true)];
  const page = new FakePage(guest);
  s = await crawl(page, dir, { creators: [A] }).run();
  assert.equal(s.stopped.reason, "login");
  assert.match(s.stopped.message, /run `login` first/);
  assert.deepEqual(page.visited, [SITE]);

  // Nothing tells: a guest may look like that, so the run stops too
  const silent = firstVisit();
  silent[SITE].responses = [];
  s = await crawl(new FakePage(silent), dir, { creators: [A] }).run();
  assert.equal(s.stopped.reason, "login");
  assert.match(s.stopped.message, /could not confirm the login/);
  rmSync(dir, { recursive: true });
});

test("the per-run notes limit leaves the rest for the next run", async () => {
  const dir = scratch();
  const s = await crawl(new FakePage(firstVisit()), dir, {
    creators: [A],
    maxNotes: 1,
  }).run();
  assert.equal(s.kept, 1);
  const st = state.load(dir, rn.TOOL);
  assert.equal(st.seen[SR], "kept");
  assert.equal(st.seen[SR2], undefined);
  rmSync(dir, { recursive: true });
});

test("the following list is read from the account's own profile", async () => {
  const dir = scratch();
  const script = firstVisit();
  const myNote = `${SITE}/explore/mine`;
  script[`${SITE}/user/profile/me1`] = {
    clicks: { "section.note-item a.cover": { location: myNote } },
  };
  script[myNote] = {
    clicks: { "#content-textarea": { location: myNote } },
    typed: [
      followings([
        [A, "Grizzco Coach"],
        ["me1", "Me"],
        [B, "Other"],
      ]),
    ],
  };
  script[PROFILE_B] = { responses: [list([], false)] };
  const page = new FakePage(script);
  const c = crawl(page, dir);
  const s = await c.run();
  assert.equal(s.stopped, undefined, c.lines.join("\n"));
  assert.equal(s.creators, 2);
  // "@" typed into a note's comment box and taken back; nothing sent
  assert.deepEqual(page.visited.slice(0, 8), [
    SITE,
    `${SITE}/user/profile/me1`,
    "click section.note-item a.cover",
    "click #content-textarea",
    "type @",
    "key Backspace",
    "key Escape",
    "key Escape",
  ]);
  const st = state.load(dir, rn.TOOL);
  assert.deepEqual(st.following, {
    me: "me1",
    handles: [A, B],
    at: NOW.toISOString(),
  });
  assert.equal(st.accounts[B].nickname, "Other");
  assert.ok(c.lines.includes("following 2 creators"));

  // Known and fresh: not read again
  const again = new FakePage(firstVisit());
  await crawl(again, dir).run();
  assert.ok(!again.visited.includes("type @"));

  // Without a note of the account's own, the run says what to do
  const bare = firstVisit();
  bare[`${SITE}/user/profile/me1`] = {};
  const s3 = await crawl(new FakePage(bare), scratch(), {
    refreshFollowing: true,
  }).run();
  assert.equal(s3.stopped.reason, "empty");
  assert.match(s3.stopped.message, /--creators/);
  rmSync(dir, { recursive: true });
});

test("the page state lists notes and holds a note's detail", async () => {
  const dir = scratch();
  const id = "66aa00000000000000000002";
  const script = {
    [SITE]: { responses: [me(false)] },
    [PROFILE_A]: {
      state: stateFixture(),
      clicks: { [`a[href*="${id}"]`]: { location: NOTE(id) } },
    },
    [NOTE(id)]: { state: stateFixture(), escapeTo: PROFILE_A },
  };
  const s = await crawl(new FakePage(script), dir, { creators: [A] }).run();
  // The cat's tile opens nothing and its address shows no note
  assert.deepEqual([s.kept, s.failed], [1, 1]);
  const line = JSON.parse(
    readFileSync(join(dir, A, "notes.jsonl"), "utf8").trim(),
  );
  assert.equal(line.title, "サーモンラン tips");
  assert.equal(line.comments[0].author.nickname, "dan");
  assert.equal(line.comments_complete, true);
  rmSync(dir, { recursive: true });
});
