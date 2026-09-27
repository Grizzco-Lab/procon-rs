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
  assert.deepEqual(matches("今晚打工，铁板鱼引到蛋筐前"), ["打工", "铁板鱼"]);
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
