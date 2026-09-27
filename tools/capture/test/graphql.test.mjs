import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import {
  compareIds,
  handleOfStatusUrl,
  operationOf,
  postsIn,
  snowflakeDate,
  usersIn,
} from "../lib/graphql.mjs";

const fixture = (name) =>
  JSON.parse(
    readFileSync(new URL(`./fixtures/${name}`, import.meta.url), "utf8"),
  );

test("operation names come from the GraphQL path", () => {
  assert.equal(
    operationOf(
      "https://x.com/i/api/graphql/AbC-123_x/UserTweets?variables=%7B%7D",
    ),
    "UserTweets",
  );
  assert.equal(
    operationOf("https://x.com/i/api/graphql/Q/TweetDetail"),
    "TweetDetail",
  );
  assert.equal(operationOf("https://x.com/home"), null);
  assert.equal(operationOf(undefined), null);
});

test("snowflakes give their time and order", () => {
  assert.equal(
    snowflakeDate("1971000000000000010").toISOString(),
    "2025-09-24T23:53:14.066Z",
  );
  assert.equal(compareIds("1971000000000000010", "1971000000000000011"), -1);
  assert.equal(compareIds("2", "1"), 1);
  assert.equal(snowflakeDate("x"), null);
});

test("a profile's answer gives its posts, quoted and retweeted ones inside them", () => {
  const posts = postsIn(fixture("user_tweets.json"));
  const ids = posts.map((p) => p.id);
  assert.deepEqual(ids, [
    "1970000000000000001",
    "1971000000000000010",
    "1971000000000000011",
    "1971000000000000012",
    "1971000000000000013",
  ]);
  const [pinned, long, lunch, rt, quote] = posts;
  assert.equal(pinned.author.handle, "ikura_coach");
  assert.equal(
    pinned.text,
    "サーモンランの立ち回りまとめ（固定ツイート） https://example.org/salmon-guide",
  );
  assert.deepEqual(pinned.urls, ["https://example.org/salmon-guide"]);
  assert.equal(
    pinned.url,
    "https://x.com/ikura_coach/status/1970000000000000001",
  );
  assert.equal(pinned.date, "2026-09-01T09:00:00.000Z");
  // The long post's note replaces the truncated text; the author's handle
  // comes from the newer `core` fields; media links leave the text
  assert.equal(long.author.name, "Ikura Coach");
  assert.match(
    long.text,
    /^バクダンは湧いた瞬間に処理。.*続きは動画で https:\/\/youtu.be\/abc123$/,
  );
  assert.deepEqual(long.urls, ["https://youtu.be/abc123"]);
  assert.equal(long.media.length, 2);
  assert.equal(long.media[0].type, "photo");
  assert.equal(long.media[1].video, "https://video.twimg.com/two-high.mp4");
  assert.equal(long.lang, "ja");
  assert.equal(long.reply_to, null);
  // A wrapped result is unwrapped
  assert.equal(lunch.text, "今日のランチはカレー");
  // The retweet is marked; the retweeted post is not listed on its own
  assert.equal(rt.retweet, true);
  assert.equal(quote.retweet, false);
  assert.equal(quote.quoted.author.handle, "salmon_lab");
  assert.equal(
    quote.quoted.text,
    "干潮のハコビヤはカゴ前で待つのが一番安定する",
  );
  assert.equal(
    quote.quoted.url,
    "https://x.com/salmon_lab/status/1971000000000000006",
  );
});

test("a post's page gives the conversation with its replies", () => {
  const posts = postsIn(fixture("tweet_detail.json"));
  assert.equal(posts.length, 4);
  const replies = posts.filter((p) => p.id !== "1971000000000000010");
  assert.ok(replies.every((p) => p.conversation_id === "1971000000000000010"));
  assert.deepEqual(replies[0].reply_to, {
    id: "1971000000000000010",
    handle: "ikura_coach",
  });
  assert.equal(replies[1].reply_to.id, "1971000000000000020");
  assert.equal(replies[2].author.name, "打工人");
  assert.equal(replies[2].lang, "zh");
});

test("a following list gives its users, unavailable ones left out", () => {
  const users = usersIn(fixture("following.json"));
  assert.deepEqual(
    users.map((u) => u.handle),
    ["ikura_coach", "salmon_lab"],
  );
  assert.equal(users[1].name, "Salmon Lab");
});

test("nothing tweet-like gives nothing", () => {
  assert.deepEqual(postsIn({ data: { errors: [{ message: "x" }] } }), []);
  assert.deepEqual(postsIn(null), []);
  assert.deepEqual(usersIn({ a: [1, 2, { __typename: "User" }] }), []);
});

test("status addresses name their handle and id", () => {
  assert.deepEqual(
    handleOfStatusUrl("https://x.com/ikura_coach/status/197/photo/1"),
    {
      handle: "ikura_coach",
      id: "197",
    },
  );
  assert.equal(handleOfStatusUrl("https://x.com/home"), null);
});
