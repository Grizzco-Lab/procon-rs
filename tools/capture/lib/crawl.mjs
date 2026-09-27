// The visit: the account's following list, each followed account's
// profile (scrolling down to what the last run saw), and the page of each
// Salmon Run post for its replies. Posts come from the GraphQL answers the
// page loads for itself; the DOM is the fallback for a profile whose
// answers were missed. Every navigation and scroll is one paced action
// (`Pace`); the run stops at a cap, on a rate limit, or when the app asks
// to log in. The page is an interface (`Page` of cdp.mjs, or a fake in
// tests).

import { isSalmonRun, matches } from "./filter.mjs";
import {
  compareIds,
  operationOf,
  postsIn,
  snowflakeDate,
  usersIn,
} from "./graphql.mjs";
import { Stop } from "./pace.mjs";
import * as output from "./output.mjs";
import * as state from "./state.mjs";

export const HOME = "https://x.com/home";

export const DEFAULTS = Object.freeze({
  /** Only posts newer than this many days are looked at */
  sinceDays: 90,
  /** Threads captured this run, at most */
  maxPosts: 60,
  /** Scrolls down a profile, at most */
  profileScrolls: 8,
  /** Scrolls down a post's page for more replies */
  replyScrolls: 1,
  /** Scrolls down the following list, at most */
  followingScrolls: 60,
  /** The following list is read again after this many days */
  followingTtlDays: 7,
  /** Browse, print, write nothing */
  dryRun: false,
});

/** The app's addresses that mean the session is gone or held */
export function asksToLogIn(url) {
  let path;
  try {
    path = new URL(url).pathname;
  } catch {
    return false;
  }
  return (
    path.startsWith("/i/flow/login") ||
    path === "/login" ||
    path.startsWith("/account/access") ||
    path.startsWith("/i/flow/consent")
  );
}

/** What GraphQL answers say about the session: a `Stop` to throw, or null */
export function trouble(responses) {
  for (const r of responses) {
    if (r.status === 429)
      return new Stop("rate-limit", "X answered 429 (rate limited)");
    if (r.status === 401)
      return new Stop("login", "X answered 401: log in again");
    if (
      r.status === 403 &&
      /UserTweets|TweetDetail|Following/.test(operationOf(r.url) ?? "")
    )
      return new Stop("forbidden", `X answered 403 to ${operationOf(r.url)}`);
  }
  return null;
}

export class Crawl {
  /**
   * @param {object} page the browser page (cdp.mjs `Page`)
   * @param {object} deps `pace` (a `Pace`), `state` (the loaded state),
   *   `dir` (the inbox's `x` folder), `log(line)`, `options`, `now()`
   */
  constructor(page, deps) {
    this.page = page;
    this.pace = deps.pace;
    this.state = deps.state;
    this.dir = deps.dir;
    this.log = deps.log ?? (() => {});
    this.options = { ...DEFAULTS, ...deps.options };
    this.now = deps.now ?? (() => new Date());
    this.summary = {
      accounts: 0,
      kept: 0,
      offTopic: 0,
      replies: 0,
      files: new Set(),
    };
  }

  /** One paced page action, then a look at where the page is */
  async action(f) {
    await this.pace.before(this.state.day);
    const result = await f();
    const here = await this.page.location();
    if (asksToLogIn(here))
      throw new Stop("login", `the app asks to log in (${here})`);
    return result;
  }

  /** Drains the page's GraphQL answers; stops on a rate limit or a lost
   * session */
  drain() {
    const responses = this.page.drain();
    const stop = trouble(responses);
    if (stop) throw stop;
    return responses.filter((r) => r.json);
  }

  /** Saves the state; a dry run saves only the day's action count, so its
   * page actions count against the daily cap too */
  save() {
    if (this.options.dryRun)
      state.saveDay(this.dir, state.TOOL, this.state.day);
    else state.save(this.dir, this.state);
  }

  /** Runs the whole visit; the state is saved as it goes. Returns the
   * summary, with `stopped` set when a `Stop` ended it early. */
  async run() {
    try {
      const me = await this.start();
      const handles = await this.following(me);
      for (const handle of this.order(handles)) {
        if (this.summary.kept >= this.options.maxPosts) break;
        await this.account(handle);
      }
    } catch (error) {
      if (!(error instanceof Stop)) throw error;
      this.summary.stopped = error;
    } finally {
      this.save();
    }
    return this.summary;
  }

  /** Home: the session check, and whose account this is */
  async start() {
    const me = await this.action(async () => {
      await this.page.goto(HOME);
      return this.page.signedIn();
    });
    this.drain();
    const handle = this.options.me ?? me;
    if (!handle)
      throw new Stop(
        "login",
        "no signed-in account on the page: run `login` first",
      );
    this.log(`signed in as @${handle}`);
    return handle;
  }

  /** The accounts to visit: `--accounts`, else the following list from
   * the state when fresh, else read from the page */
  async following(me) {
    if (this.options.accounts?.length) return this.options.accounts;
    const f = this.state.following;
    const ageDays = f.at
      ? (this.now() - new Date(f.at)) / 86_400_000
      : Infinity;
    if (
      f.me === me &&
      f.handles.length &&
      ageDays < this.options.followingTtlDays
    ) {
      this.log(
        `following list of ${f.handles.length} accounts from ${f.at.slice(0, 10)}`,
      );
      return f.handles;
    }
    const handles = new Set();
    const take = () => {
      let added = 0;
      for (const r of this.drain()) {
        if (operationOf(r.url) !== "Following") continue;
        for (const u of usersIn(r.json)) {
          if (
            u.handle.toLowerCase() === me.toLowerCase() ||
            handles.has(u.handle)
          )
            continue;
          handles.add(u.handle);
          added++;
        }
      }
      return added;
    };
    await this.action(() => this.page.goto(`https://x.com/${me}/following`));
    await this.page.waitFor((r) => operationOf(r.url) === "Following");
    take();
    let idle = 0;
    for (let i = 0; i < this.options.followingScrolls && idle < 2; i++) {
      await this.action(() => this.page.scroll());
      if (take() === 0) idle++;
      else idle = 0;
    }
    if (!handles.size)
      throw new Stop("empty", "no following list was received");
    this.state.following = {
      me,
      handles: [...handles],
      at: this.now().toISOString(),
    };
    this.save();
    this.log(`following ${handles.size} accounts`);
    return this.state.following.handles;
  }

  /** Accounts never visited first, then the longest unvisited */
  order(handles) {
    const at = (h) => this.state.accounts[h]?.visited_at ?? "";
    return [...handles].sort((a, b) =>
      at(a) < at(b) ? -1 : at(a) > at(b) ? 1 : 0,
    );
  }

  /** The posts of a profile's answers that are the account's own, with
   * the retweets and replies noted in `seen` */
  ownPosts(handle, responses) {
    const posts = [];
    for (const r of responses) {
      if (!/UserTweets/.test(operationOf(r.url) ?? "")) continue;
      for (const p of postsIn(r.json)) {
        if ((p.author.handle ?? "").toLowerCase() !== handle.toLowerCase())
          continue;
        if (p.retweet) this.state.seen[p.id] ??= "retweet";
        else if (p.reply_to) this.state.seen[p.id] ??= "reply";
        else posts.push(p);
      }
    }
    return posts;
  }

  /** A profile: scroll down until nothing new appears, then the threads
   * of its Salmon Run posts */
  async account(handle) {
    const record = state.account(this.state, handle);
    const since = this.now() - this.options.sinceDays * 86_400_000;
    const seen = this.state.seen;
    const candidates = new Map();
    let newest = record.newest_id;
    let usedDom = false;
    const look = (posts) => {
      let fresh = 0;
      for (const p of posts) {
        if (!newest || compareIds(p.id, newest) > 0) newest = p.id;
        if (seen[p.id]) continue;
        const date = p.date ? new Date(p.date) : snowflakeDate(p.id);
        if (date && date < since) continue;
        fresh++;
        if (isSalmonRun(p)) candidates.set(p.id, p);
        else {
          seen[p.id] = "off-topic";
          record.off_topic++;
          this.summary.offTopic++;
        }
      }
      return fresh;
    };
    await this.action(() => this.page.goto(`https://x.com/${handle}`));
    await this.page.waitFor(
      (r) => /UserTweets/.test(operationOf(r.url) ?? ""),
      15_000,
    );
    let responses = this.drain();
    let posts = this.ownPosts(handle, responses);
    if (!responses.length && !posts.length) {
      // No answer seen: what the page shows
      posts = (await this.page.domPosts())
        .filter((p) => p.handle.toLowerCase() === handle.toLowerCase())
        .map((p) => ({
          id: p.id,
          url: `https://x.com/${p.handle}/status/${p.id}`,
          author: { handle: p.handle, name: null },
          date: p.date,
          text: p.text,
          lang: null,
          urls: [],
          media: [],
          quoted: null,
          reply_to: null,
        }));
      usedDom = true;
    }
    let fresh = look(posts);
    // Scroll on while new posts keep coming; a scroll that loads nothing
    // is given one more chance (the page may not have reached its end)
    let idle = 0;
    for (
      let i = 0;
      i < this.options.profileScrolls && fresh > 0 && !usedDom && idle < 2;
      i++
    ) {
      await this.action(() => this.page.scroll());
      responses = this.drain();
      if (!responses.length) {
        idle++;
        continue;
      }
      idle = 0;
      fresh = look(this.ownPosts(handle, responses));
    }
    record.newest_id = newest;
    record.visited_at = this.now().toISOString();
    this.summary.accounts++;
    this.log(
      `@${handle}: ${candidates.size} Salmon Run posts to read${usedDom ? " (from the page)" : ""}`,
    );
    const ordered = [...candidates.values()].sort((a, b) =>
      compareIds(b.id, a.id),
    );
    for (const post of ordered) {
      if (this.summary.kept >= this.options.maxPosts) break;
      await this.thread(handle, record, post);
    }
    this.save();
  }

  /** A post's page: the post as loaded there and its replies */
  async thread(handle, record, post) {
    const replies = new Map();
    let root = post;
    const take = () => {
      let added = 0;
      for (const r of this.drain()) {
        if (operationOf(r.url) !== "TweetDetail") continue;
        for (const p of postsIn(r.json)) {
          if (p.id === post.id) root = p;
          else if (p.conversation_id === post.id && !replies.has(p.id)) {
            replies.set(p.id, p);
            added++;
          }
        }
      }
      return added;
    };
    await this.action(() => this.page.goto(post.url));
    await this.page.waitFor(
      (r) => operationOf(r.url) === "TweetDetail",
      15_000,
    );
    take();
    for (let i = 0; i < this.options.replyScrolls; i++) {
      await this.action(() => this.page.scroll());
      if (take() === 0) break;
    }
    const list = [...replies.values()].sort((a, b) => compareIds(a.id, b.id));
    const matched = matches(
      [root.text, root.quoted?.text].filter(Boolean).join("\n"),
    );
    const line = output.record(root, list, matched, this.now());
    this.state.seen[post.id] = "kept";
    record.kept++;
    this.summary.kept++;
    this.summary.replies += list.length;
    const head = root.text.replace(/\s+/g, " ").slice(0, 60);
    this.log(
      `  ${this.options.dryRun ? "would keep" : "kept"} ${post.url} (${list.length} replies): ${head}`,
    );
    if (!this.options.dryRun) {
      this.summary.files.add(output.append(this.dir, handle, [line]));
      this.save();
    }
  }
}
