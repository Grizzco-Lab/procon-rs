// The visit against a scripted page: which addresses are opened, what is
// kept, what the state remembers, and what stops a run
import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { Crawl, asksToLogIn, trouble } from "../lib/crawl.mjs";
import { Pace } from "../lib/pace.mjs";
import * as state from "../lib/state.mjs";

const NOW = new Date("2026-09-27T12:00:00Z");
const GRAPHQL = "https://x.com/i/api/graphql/abc/";

/** A tweet result in the shape the walker looks for */
function tweet(id, handle, text, extra = {}) {
  const t = {
    __typename: "Tweet",
    rest_id: id,
    core: {
      user_results: {
        result: {
          __typename: "User",
          legacy: { screen_name: handle, name: handle },
        },
      },
    },
    legacy: {
      created_at: extra.created_at ?? "Thu Sep 24 12:00:00 +0000 2026",
      conversation_id_str: extra.conversation ?? id,
      full_text: text,
      lang: extra.lang ?? "ja",
      entities: {},
    },
  };
  if (extra.reply_to)
    Object.assign(t.legacy, {
      in_reply_to_status_id_str: extra.reply_to,
      in_reply_to_screen_name: extra.reply_handle ?? "someone",
    });
  if (extra.retweet)
    t.legacy.retweeted_status_result = {
      result: tweet(`${id}0`, "other", text),
    };
  return t;
}
const user = (handle) => ({
  __typename: "User",
  rest_id: handle,
  legacy: { screen_name: handle, name: handle },
});
const answer = (op, ...results) => ({
  url: `${GRAPHQL}${op}?variables=x`,
  status: 200,
  json: {
    data: {
      entries: results.map((r) => ({
        itemContent: { tweet_results: { result: r } },
      })),
    },
  },
});

/** A page whose answers are scripted per address: what arrives on
 * opening it, then per scroll */
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
  async scroll() {
    this.visited.push("scroll");
    const batches = this.entry().scrolls ?? [];
    this.queue.push(...(batches[this.scrolls++] ?? []));
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
  async signedIn() {
    const entry = this.entry();
    return "signedIn" in entry ? entry.signedIn : "me_me";
  }
  async domPosts() {
    return this.entry().dom ?? [];
  }
}

const quick = () =>
  new Pace(
    { maxActions: null, dailyCap: null },
    { sleep: async () => true, random: () => 0.5 },
  );

function crawl(page, dir, options = {}, st = state.load(dir)) {
  const lines = [];
  const c = new Crawl(page, {
    pace: quick(),
    state: st,
    dir,
    log: (l) => lines.push(l),
    options,
    now: () => NOW,
  });
  c.lines = lines;
  return c;
}

const A = "https://x.com/coach_a";
const B = "https://x.com/coach_b";
const P1 = "https://x.com/coach_a/status/1971000000000000010";
const P2 = "https://x.com/coach_a/status/1971000000000000012";

/** The script of a first visit: two accounts followed, coach_a with two
 * Salmon Run posts (one loaded on a scroll), an off-topic post, a retweet,
 * a reply and an old post; coach_b with nothing about Salmon Run */
function firstVisit() {
  return {
    "https://x.com/home": { responses: [] },
    "https://x.com/me_me/following": {
      responses: [answer("Following", user("coach_a"), user("me_me"))],
      scrolls: [[answer("Following", user("coach_b"))], [], []],
    },
    [A]: {
      responses: [
        answer(
          "UserTweets",
          tweet("1971000000000000010", "coach_a", "バクダンは湧いた瞬間に処理"),
          tweet("1971000000000000011", "coach_a", "今日のランチはカレー"),
          tweet("1971000000000000013", "coach_a", "RT @x: Salmon Run", {
            retweet: true,
          }),
          tweet(
            "1971000000000000014",
            "coach_a",
            "@y そうですね サーモンラン",
            { reply_to: "1", reply_handle: "y" },
          ),
          tweet("1800000000000000000", "coach_a", "old Salmon Run post", {
            created_at: "Sat Jan 06 00:00:00 +0000 2024",
          }),
        ),
      ],
      scrolls: [
        [
          answer(
            "UserTweets",
            tweet(
              "1971000000000000012",
              "coach_a",
              "Eggstra Work this weekend",
              { lang: "en" },
            ),
          ),
        ],
        [],
        [],
      ],
    },
    [P1]: {
      responses: [
        answer(
          "TweetDetail",
          tweet(
            "1971000000000000010",
            "coach_a",
            "バクダンは湧いた瞬間に処理（全文）",
          ),
          tweet("1971000000000000020", "fan", "@coach_a なるほど", {
            conversation: "1971000000000000010",
            reply_to: "1971000000000000010",
            reply_handle: "coach_a",
          }),
          tweet("1971000000000000099", "other", "unrelated", {
            conversation: "5",
          }),
        ),
      ],
      scrolls: [
        [
          answer(
            "TweetDetail",
            tweet("1971000000000000021", "coach_a", "@fan はい", {
              conversation: "1971000000000000010",
              reply_to: "1971000000000000020",
              reply_handle: "fan",
            }),
          ),
        ],
      ],
    },
    [P2]: {
      responses: [
        answer(
          "TweetDetail",
          tweet("1971000000000000012", "coach_a", "Eggstra Work this weekend"),
        ),
      ],
    },
    [B]: {
      responses: [
        answer(
          "UserTweets",
          tweet("1971000000000000050", "coach_b", "new keyboard day"),
        ),
      ],
    },
  };
}

test("a first visit: following, profiles, the Salmon Run threads", async () => {
  const dir = join(mkdtempSync(join(tmpdir(), "xcap-crawl-")), "x");
  const page = new FakePage(firstVisit());
  const c = crawl(page, dir);
  const s = await c.run();
  assert.equal(s.stopped, undefined);
  assert.deepEqual(page.visited, [
    "https://x.com/home",
    "https://x.com/me_me/following",
    "scroll",
    "scroll",
    "scroll",
    A,
    "scroll",
    "scroll",
    "scroll",
    P2,
    "scroll",
    P1,
    "scroll",
    // coach_b showed a new post, so the page is scrolled for more; two
    // empty scrolls end it
    B,
    "scroll",
    "scroll",
  ]);
  assert.equal(s.kept, 2);
  assert.equal(s.replies, 2);
  assert.equal(s.offTopic, 2);
  assert.equal(s.accounts, 2);
  const st = state.load(dir);
  assert.deepEqual(st.following.handles, ["coach_a", "coach_b"]);
  assert.equal(st.following.me, "me_me");
  // The newest of the account's own posts (retweets and replies aside)
  assert.equal(st.accounts.coach_a.newest_id, "1971000000000000012");
  assert.equal(st.accounts.coach_a.kept, 2);
  assert.equal(st.accounts.coach_a.off_topic, 1);
  assert.equal(st.accounts.coach_b.off_topic, 1);
  assert.deepEqual(st.seen, {
    "1971000000000000010": "kept",
    "1971000000000000011": "off-topic",
    "1971000000000000012": "kept",
    "1971000000000000013": "retweet",
    "1971000000000000014": "reply",
    "1971000000000000050": "off-topic",
  });
  assert.equal(st.day.actions, 16);
  const lines = readFileSync(join(dir, "coach_a", "posts.jsonl"), "utf8")
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l));
  assert.deepEqual(
    lines.map((l) => l.id),
    ["1971000000000000012", "1971000000000000010"],
  );
  const thread = lines[1];
  // The post as its own page loaded it, its replies in order, only those
  // of its conversation
  assert.equal(thread.text, "バクダンは湧いた瞬間に処理（全文）");
  assert.deepEqual(
    thread.replies.map((r) => r.id),
    ["1971000000000000020", "1971000000000000021"],
  );
  assert.equal(thread.replies[1].reply_to.handle, "fan");
  assert.deepEqual(thread.matched, ["バクダン"]);
  assert.equal(thread.source, "x");
  assert.equal(existsSync(join(dir, "coach_b")), false);
  assert.ok(
    c.lines.some((l) =>
      l.includes(
        "kept https://x.com/coach_a/status/1971000000000000010 (2 replies)",
      ),
    ),
  );

  // A second run: the following list is fresh, the profiles show nothing
  // new, so nothing is read again
  const again = new FakePage(firstVisit());
  const s2 = await crawl(again, dir).run();
  assert.deepEqual(again.visited, ["https://x.com/home", A, B]);
  assert.equal(s2.kept, 0);
  assert.equal(state.load(dir).day.actions, 19);
  rmSync(dir, { recursive: true });
});

test("a dry run writes only the day's action count", async () => {
  const dir = join(mkdtempSync(join(tmpdir(), "xcap-crawl-")), "x");
  const page = new FakePage(firstVisit());
  const s = await crawl(page, dir, { dryRun: true }).run();
  assert.equal(s.kept, 2);
  assert.equal(s.files.size, 0);
  assert.deepEqual(
    readdirSync(dir).filter((f) => f !== state.STATE_FILE),
    [],
  );
  const st = state.load(dir);
  assert.equal(st.day.actions, 16);
  assert.deepEqual(st.seen, {});
  assert.deepEqual(st.accounts, {});
  rmSync(dir, { recursive: true });
});

test("the per-run thread limit and --accounts", async () => {
  const dir = join(mkdtempSync(join(tmpdir(), "xcap-crawl-")), "x");
  const page = new FakePage(firstVisit());
  const s = await crawl(page, dir, {
    maxPosts: 1,
    accounts: ["coach_a"],
  }).run();
  assert.equal(s.kept, 1);
  assert.ok(!page.visited.includes("https://x.com/me_me/following"));
  assert.ok(!page.visited.includes(B));
  // The thread not read is not marked seen: the next run takes it
  assert.equal(state.load(dir).seen["1971000000000000010"], undefined);
  rmSync(dir, { recursive: true });
});

test("the login page, a rate limit and a lost session stop the run", async () => {
  const dir = join(mkdtempSync(join(tmpdir(), "xcap-crawl-")), "x");
  const login = firstVisit();
  login[A].location =
    "https://x.com/i/flow/login?redirect_after_login=%2Fcoach_a";
  let s = await crawl(new FakePage(login), dir, {
    accounts: ["coach_a"],
  }).run();
  assert.equal(s.stopped.reason, "login");
  assert.equal(state.load(dir).day.actions, 2);

  const limited = firstVisit();
  limited[A].responses.push({
    url: `${GRAPHQL}UserTweets`,
    status: 429,
    json: null,
  });
  s = await crawl(new FakePage(limited), dir, { accounts: ["coach_a"] }).run();
  assert.equal(s.stopped.reason, "rate-limit");
  assert.equal(s.kept, 0);

  const signedOut = firstVisit();
  signedOut["https://x.com/home"].signedIn = null;
  s = await crawl(new FakePage(signedOut), dir).run();
  assert.equal(s.stopped.reason, "login");
  assert.match(s.stopped.message, /run `login` first/);

  assert.ok(asksToLogIn("https://x.com/i/flow/login"));
  assert.ok(asksToLogIn("https://x.com/account/access"));
  assert.ok(!asksToLogIn("https://x.com/coach_a"));
  assert.ok(!asksToLogIn("not a url"));
  assert.equal(trouble([{ url: `${GRAPHQL}Viewer`, status: 403 }]), null);
  assert.equal(
    trouble([{ url: `${GRAPHQL}TweetDetail`, status: 403 }]).reason,
    "forbidden",
  );
  assert.equal(
    trouble([{ url: `${GRAPHQL}TweetDetail`, status: 401 }]).reason,
    "login",
  );
  rmSync(dir, { recursive: true });
});

test("a profile whose answers were missed is read from the page", async () => {
  const dir = join(mkdtempSync(join(tmpdir(), "xcap-crawl-")), "x");
  const script = firstVisit();
  script[A] = {
    responses: [],
    dom: [
      {
        id: "1971000000000000010",
        handle: "coach_a",
        text: "バクダンは湧いた瞬間に処理",
        date: "2026-09-24T12:00:00.000Z",
      },
      {
        id: "1971000000000000011",
        handle: "coach_a",
        text: "lunch",
        date: null,
      },
      {
        id: "1971000000000000060",
        handle: "someone_else",
        text: "サーモンラン",
        date: null,
      },
    ],
  };
  const page = new FakePage(script);
  const s = await crawl(page, dir, { accounts: ["coach_a"] }).run();
  assert.equal(s.kept, 1);
  assert.deepEqual(page.visited, ["https://x.com/home", A, P1, "scroll"]);
  assert.equal(state.load(dir).seen["1971000000000000060"], undefined);
  rmSync(dir, { recursive: true });
});
