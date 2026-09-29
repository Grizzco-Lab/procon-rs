// The cover check: file ids out of the site's image addresses, the
// thumbnail fetched with a fallback, the helper's line protocol and the
// judge a crawl gets
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import {
  CoverCheck,
  fetchImage,
  fileIdOf,
  judge,
  thumbnailUrl,
} from "../lib/cover.mjs";

const ID = "1040g2sg31puunoms6u7043j3irqf2hn78qm4t8o";
const SIGNED = `http://sns-web-i10.rednotecdn.com/202609280910/250440ea64762c759fb5c37b8efb9f47/${ID}!nd_dft_wgth_webp_3`;
const FAKE = [
  process.execPath,
  fileURLToPath(new URL("./fixtures/fake-cover.mjs", import.meta.url)),
];

/** A fetch answering by address: `[status, type, body]` */
const fetchOf =
  (table, calls = []) =>
  async (url) => {
    calls.push(url);
    const [status, type, body] = table[url] ?? [404, "text/html", "no"];
    return {
      ok: status < 300,
      status,
      headers: { get: (h) => (h === "content-type" ? type : null) },
      arrayBuffer: async () => Buffer.from(body),
    };
  };

test("file ids come out of signed and unsigned addresses, folders and all", () => {
  assert.equal(fileIdOf(SIGNED), ID);
  assert.equal(
    fileIdOf(
      `https://sns-webpic-qc.xhscdn.com/202609281200/0123456789abcdef0123456789abcdef/notes_pre_post/${ID}!nd_dft_wlteh_webp_3`,
    ),
    `notes_pre_post/${ID}`,
  );
  assert.equal(
    fileIdOf(`https://sns-img-qc.xhscdn.com/110/0/${ID}_0.jpg`),
    `110/0/${ID}_0.jpg`,
  );
  assert.equal(fileIdOf(thumbnailUrl(ID)), ID);
  assert.equal(fileIdOf("https://www.xiaohongshu.com/explore/66aa"), null);
  assert.equal(fileIdOf("not a url"), null);
  assert.equal(fileIdOf(null), null);
  assert.equal(
    thumbnailUrl(`notes_pre_post/${ID}`),
    `https://ci.xiaohongshu.com/notes_pre_post/${ID}?imageView2/2/w/360/format/webp`,
  );
});

test("the thumbnail: the listed address, else the unsigned one by file id, else nothing", async () => {
  const calls = [];
  const fetch = fetchOf(
    {
      [SIGNED]: [403, "text/html", "expired"],
      [thumbnailUrl(ID)]: [200, "image/webp", "RIFF"],
    },
    calls,
  );
  assert.equal(String(await fetchImage(SIGNED, { fetch })), "RIFF");
  assert.deepEqual(calls, [SIGNED, thumbnailUrl(ID)]);
  // The listed address answers: one request
  const direct = fetchOf({ [SIGNED]: [200, "image/jpeg", "JFIF"] }, calls);
  calls.length = 0;
  assert.equal(String(await fetchImage(SIGNED, { fetch: direct })), "JFIF");
  assert.deepEqual(calls, [SIGNED]);
  // Neither, or not a picture: null
  assert.equal(await fetchImage(SIGNED, { fetch: fetchOf({}) }), null);
  assert.equal(
    await fetchImage(SIGNED, {
      fetch: fetchOf({ [SIGNED]: [200, "text/html", "<html>"] }),
    }),
    null,
  );
  assert.equal(
    await fetchImage("https://x/no-id", {
      fetch: async () => {
        throw new Error("down");
      },
    }),
    null,
  );
});

test("the helper answers each picture in turn; an error is the picture's alone", async () => {
  const lines = [];
  const check = new CoverCheck({ command: FAKE, log: (l) => lines.push(l) });
  await check.start();
  assert.deepEqual(lines, ["cover check: fake on the cpu"]);
  const [a, b] = await Promise.all([
    check.score(Buffer.from("SPLAT gameplay")),
    check.score(Buffer.from("a cat")),
  ]);
  assert.deepEqual([a, b], [0.9, 0.1]);
  await assert.rejects(check.score(Buffer.from("BAD")), /UnidentifiedImage/);
  assert.equal(await check.score(Buffer.from("SPLAT again")), 0.9);
  check.stop();
  await assert.rejects(check.score(Buffer.from("x")), /not running/);
});

test("a helper that cannot start fails the start, not the run", async () => {
  const check = new CoverCheck({ command: ["/nonexistent/python"] });
  await assert.rejects(check.start(), /could not start/);
});

test("the judge scores a cover and answers null for what it cannot judge", async () => {
  const check = new CoverCheck({ command: FAKE });
  const lines = [];
  const fetch = fetchOf({
    "https://pic/game": [200, "image/webp", "SPLAT"],
    "https://pic/cat": [200, "image/webp", "meow"],
    "https://pic/bad": [200, "image/webp", "BAD"],
  });
  const score = judge(check, { fetch, log: (l) => lines.push(l) });
  assert.equal(await score("https://pic/game"), 0.9);
  assert.equal(await score("https://pic/cat"), 0.1);
  assert.equal(await score("https://pic/bad"), null);
  assert.equal(await score("https://pic/gone"), null);
  assert.deepEqual(lines, [
    "  cover https://pic/bad: UnidentifiedImageError: no",
    "  cover https://pic/gone: no picture",
  ]);
  check.stop();
});
