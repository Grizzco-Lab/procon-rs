// Posts and users out of the GraphQL JSON the X web app loads for its own
// pages (UserTweets, TweetDetail, Following, ...). The shapes change now
// and then, so nothing here depends on the instruction layout: the whole
// response is walked for objects that look like a Tweet or a User.

/** Twitter's snowflake epoch, ms */
const EPOCH = 1288834974657n;

/** When a snowflake id was made */
export function snowflakeDate(id) {
  try {
    return new Date(Number((BigInt(id) >> 22n) + EPOCH));
  } catch {
    return null;
  }
}

/** Compares two snowflake ids as numbers (a newer id is larger) */
export function compareIds(a, b) {
  const x = BigInt(a);
  const y = BigInt(b);
  return x < y ? -1 : x > y ? 1 : 0;
}

/** The operation of a GraphQL request, from its URL
 * (`/i/api/graphql/<query id>/<Operation>?...`), else null */
export function operationOf(url) {
  const m = /\/i\/api\/graphql\/[^/]+\/([A-Za-z0-9_]+)/.exec(url ?? "");
  return m ? m[1] : null;
}

/** Every object in `value`, depth first */
function* objects(value, depth = 0) {
  if (!value || typeof value !== "object" || depth > 60) return;
  if (Array.isArray(value)) {
    for (const item of value) yield* objects(item, depth + 1);
    return;
  }
  yield value;
  for (const item of Object.values(value)) yield* objects(item, depth + 1);
}

/** A tweet result unwrapped: `TweetWithVisibilityResults` holds the tweet
 * under `tweet`; a `Tweet` is itself. Null for tombstones and the rest */
function unwrapTweet(result) {
  if (!result || typeof result !== "object") return null;
  if (result.__typename === "TweetWithVisibilityResults" && result.tweet)
    return unwrapTweet(result.tweet);
  const looksLikeTweet =
    result.__typename === "Tweet" ||
    (typeof result.rest_id === "string" &&
      result.legacy &&
      typeof result.legacy.full_text === "string");
  return looksLikeTweet && result.legacy ? result : null;
}

/** A user result's handle and name, from the older `legacy` fields or the
 * newer `core` ones */
function userOf(result) {
  if (!result || typeof result !== "object") return null;
  const r = result.__typename === "UserUnavailable" ? null : result;
  if (!r) return null;
  const handle = r.legacy?.screen_name ?? r.core?.screen_name ?? null;
  const name = r.legacy?.name ?? r.core?.name ?? null;
  if (!handle) return null;
  return { id: r.rest_id ?? null, handle, name: name ?? handle };
}

/** `created_at` as X writes it ("Wed Sep 24 12:00:00 +0000 2026") as an
 * ISO string; the snowflake's time when it does not parse */
function dateOf(legacy, id) {
  const t = Date.parse(legacy?.created_at ?? "");
  const date = Number.isFinite(t) ? new Date(t) : snowflakeDate(id);
  return date ? date.toISOString() : null;
}

/** The text with t.co links written out and the media links (which the
 * text carries at its end) removed */
function textOf(tweet) {
  const legacy = tweet.legacy;
  const note = tweet.note_tweet?.note_tweet_results?.result;
  let text = note?.text ?? legacy.full_text ?? "";
  const entities = note?.entity_set ?? legacy.entities ?? {};
  for (const u of entities.urls ?? []) {
    if (u.url && u.expanded_url) text = text.split(u.url).join(u.expanded_url);
  }
  const media = legacy.extended_entities?.media ?? legacy.entities?.media ?? [];
  for (const m of media) {
    if (m.url) text = text.split(m.url).join("");
  }
  // Only the text of the note (a long post) is kept when there is one; the
  // legacy text is its truncated start
  return text.replace(/[ \t]+$/gm, "").trim();
}

/** The links a post carries (expanded, without the media's own t.co links) */
function urlsOf(tweet) {
  const note = tweet.note_tweet?.note_tweet_results?.result;
  const entities = note?.entity_set ?? tweet.legacy.entities ?? {};
  const urls = (entities.urls ?? []).map((u) => u.expanded_url).filter(Boolean);
  return [...new Set(urls)];
}

/** The post's photos and videos: the picture's address, and for a video
 * the largest MP4 variant */
function mediaOf(tweet) {
  const media =
    tweet.legacy.extended_entities?.media ?? tweet.legacy.entities?.media ?? [];
  return media.map((m) => {
    const entry = {
      type: m.type ?? "photo",
      url: m.media_url_https ?? m.media_url,
    };
    const variants = (m.video_info?.variants ?? []).filter(
      (v) => v.content_type === "video/mp4" && v.url,
    );
    if (variants.length) {
      variants.sort((a, b) => (b.bitrate ?? 0) - (a.bitrate ?? 0));
      entry.video = variants[0].url;
    }
    return entry;
  });
}

/** A post as the capture keeps it, from a tweet result */
function postOf(tweet, depth = 0) {
  const legacy = tweet.legacy;
  const id = tweet.rest_id ?? legacy.id_str;
  if (!id) return null;
  const author = userOf(tweet.core?.user_results?.result) ?? {
    id: legacy.user_id_str ?? null,
    handle: null,
    name: null,
  };
  const post = {
    id,
    url: author.handle
      ? `https://x.com/${author.handle}/status/${id}`
      : `https://x.com/i/status/${id}`,
    author: { handle: author.handle, name: author.name },
    date: dateOf(legacy, id),
    text: textOf(tweet),
    lang: legacy.lang ?? null,
    urls: urlsOf(tweet),
    media: mediaOf(tweet),
    conversation_id: legacy.conversation_id_str ?? id,
    reply_to: legacy.in_reply_to_status_id_str
      ? {
          id: legacy.in_reply_to_status_id_str,
          handle: legacy.in_reply_to_screen_name ?? null,
        }
      : null,
    quoted: null,
    retweet:
      !!legacy.retweeted_status_result || /^RT @/.test(legacy.full_text ?? ""),
  };
  const quoted = unwrapTweet(tweet.quoted_status_result?.result);
  if (quoted && depth < 2) {
    const q = postOf(quoted, depth + 1);
    if (q)
      post.quoted = {
        id: q.id,
        url: q.url,
        author: q.author,
        date: q.date,
        text: q.text,
      };
  }
  return post;
}

/** Every post in a GraphQL response body (parsed JSON), once each by id,
 * with quoted posts kept inside the posts that quote them. Retweets are
 * marked `retweet`; the retweeted post itself is not listed. */
export function postsIn(json) {
  const seen = new Map();
  for (const o of objects(json)) {
    const tweet = unwrapTweet(o);
    if (!tweet) continue;
    const post = postOf(tweet);
    if (!post || seen.has(post.id)) continue;
    seen.set(post.id, post);
  }
  // Quoted and retweeted posts are also reached by the walk as objects of
  // their own; keep only the ones that are not merely quoted or retweeted
  const inner = new Set();
  for (const o of objects(json)) {
    const q = unwrapTweet(o?.quoted_status_result?.result);
    if (q) inner.add(q.rest_id ?? q.legacy?.id_str);
    const rt = unwrapTweet(o?.legacy?.retweeted_status_result?.result);
    if (rt) inner.add(rt.rest_id ?? rt.legacy?.id_str);
  }
  return [...seen.values()].filter((p) => !inner.has(p.id));
}

/** Every user in a GraphQL response body, once each by handle */
export function usersIn(json) {
  const seen = new Map();
  for (const o of objects(json)) {
    if (o.__typename !== "User") continue;
    const u = userOf(o);
    if (u && !seen.has(u.handle)) seen.set(u.handle, u);
  }
  return [...seen.values()];
}

/** The handle a status address names, else null */
export function handleOfStatusUrl(url) {
  const m =
    /^https?:\/\/(?:x|twitter)\.com\/([A-Za-z0-9_]{1,15})\/status\/(\d+)/.exec(
      url ?? "",
    );
  return m ? { handle: m[1], id: m[2] } : null;
}
