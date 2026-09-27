import assert from "node:assert/strict";
import { test } from "node:test";
import { isSalmonRun, matches } from "../lib/filter.mjs";

test("Salmon Run terms match in three languages", () => {
  assert.deepEqual(matches("今日のサーモンラン、バクダン多すぎ"), [
    "サーモンラン",
    "バクダン",
  ]);
  assert.deepEqual(
    matches("Two Flyfish at once on Spawning Grounds, EVP 999"),
    ["flyfish", "spawning grounds", "evp"],
  );
  assert.deepEqual(matches("今晚打工，铁板鱼引到蛋筐前"), [
    "打工",
    "铁板鱼",
    "蛋筐",
  ]);
  // The stages' short names and the players' jargon, as Xiaohongshu
  // titles have them
  for (const title of [
    "斗技场非开门野良不连续无败",
    "生筋子非开门无败",
    "破船非开门无败🫟",
    "发电所非开门0咪",
    "清空熊商会商店…到此为止",
    "不想搬蛋你为什么不上炮…",
  ])
    assert.ok(matches(title).length, title);
  for (const title of ["基拉祈基拉祈基拉祈的家", "久违的岛建～～💖"])
    assert.deepEqual(matches(title), [], title);
  assert.deepEqual(matches("#ｻｰﾏﾞﾉﾝﾞﾞﾙﾞ"), []);
  // Full-width and half-width forms are one
  assert.deepEqual(matches("ＳＡＬＭＯＮ　ＲＵＮ"), ["salmon run"]);
});

test("Latin terms are whole words; scripts do not bleed", () => {
  assert.deepEqual(matches("a chummy evening with grizzled friends"), []);
  assert.deepEqual(matches("Chum and Cohocks everywhere"), ["chum", "cohocks"]);
  assert.deepEqual(matches("Today's lunch was curry"), []);
  assert.deepEqual(matches("今日のランチはカレー"), []);
});

test("a post is about Salmon Run through its own or its quoted text", () => {
  assert.equal(
    isSalmonRun({
      text: "これは本当にそう",
      quoted: { text: "干潮のハコビヤ" },
    }),
    true,
  );
  assert.equal(isSalmonRun({ text: "これは本当にそう", quoted: null }), false);
  assert.equal(isSalmonRun({ text: "Big Run results are in" }), true);
  assert.equal(isSalmonRun(null), false);
});
