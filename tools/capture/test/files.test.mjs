// The state file, the output files and the knowledge folder's resolution
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { knowledgeFolder, readToml, labKnowledge } from "../lib/config.mjs";
import * as output from "../lib/output.mjs";
import * as state from "../lib/state.mjs";

const scratch = () => mkdtempSync(join(tmpdir(), "xcap-test-"));

test("the state is fresh without a file, and comes back as saved", () => {
  const dir = join(scratch(), "x");
  const st = state.load(dir);
  assert.equal(st.tool, "xcap");
  assert.deepEqual(st.accounts, {});
  state.account(st, "ikura_coach").kept = 3;
  st.seen["1"] = "kept";
  st.day = { day: "2026-09-27", actions: 12 };
  state.save(dir, st);
  const again = state.load(dir);
  assert.equal(again.accounts.ikura_coach.kept, 3);
  assert.equal(again.seen["1"], "kept");
  assert.equal(again.day.actions, 12);
  // The file says whose it is, so the inbox skips it
  assert.match(readFileSync(join(dir, "state.json"), "utf8"), /"tool": "xcap"/);
  // Another tool's file is refused
  writeFileSync(join(dir, "state.json"), '{"tool": "other"}');
  assert.throws(() => state.load(dir), /not xcap's/);
  rmSync(dir, { recursive: true });
});

test("threads are appended as JSON lines per account", () => {
  const dir = scratch();
  const post = {
    id: "10",
    url: "https://x.com/a/status/10",
    author: { handle: "a", name: "A" },
    date: "2026-09-24T12:00:00.000Z",
    text: "サーモンラン",
    lang: "ja",
    urls: [],
    media: [],
    quoted: null,
    reply_to: null,
    conversation_id: "10",
    retweet: false,
  };
  const reply = {
    ...post,
    id: "11",
    text: "reply",
    reply_to: { id: "10", handle: "a" },
  };
  const line = output.record(
    post,
    [reply],
    ["サーモンラン"],
    new Date("2026-09-27T00:00:00Z"),
  );
  assert.equal(line.source, "x");
  assert.equal(line.replies[0].reply_to.id, "10");
  assert.equal("conversation_id" in line, false);
  assert.equal(line.captured_at, "2026-09-27T00:00:00.000Z");
  const path = output.append(dir, "a", [line]);
  output.append(dir, "a", [{ ...line, id: "12" }]);
  assert.equal(path, join(dir, "a", "posts.jsonl"));
  const lines = readFileSync(path, "utf8")
    .trim()
    .split("\n")
    .map((l) => JSON.parse(l));
  assert.deepEqual(
    lines.map((l) => l.id),
    ["10", "12"],
  );
  assert.throws(() => output.folderOf("../etc"), /not an X handle/);
  rmSync(dir, { recursive: true });
});

test("the knowledge folder is found as the cuttlefish CLI finds it", () => {
  const dir = scratch();
  const config = join(dir, "config.toml");
  writeFileSync(
    config,
    `[recording]\nprefix = "recordings/"  # sessions\n\n[cuttlefish]\n# knowledge = "/elsewhere"\nmodel = "claude-opus-5-5"\n`,
  );
  assert.equal(labKnowledge(config), join(dir, "Knowledge"));
  // A knowledge folder set in the config, relative to it
  writeFileSync(config, `[cuttlefish]\nknowledge = "data/Knowledge"\n`);
  assert.equal(labKnowledge(config), join(dir, "data", "Knowledge"));
  // The sessions' root
  writeFileSync(config, `[inspect]\nroot = "/data/procon/sessions"\n`);
  assert.equal(labKnowledge(config), "/data/procon/Knowledge");
  // The dashboard's saved prefix wins over the config's
  writeFileSync(config, `[recording]\nprefix = "recordings/"\n`);
  writeFileSync(
    join(dir, "config.state.json"),
    '{"prefix": "/mnt/rec/sessions/run-"}',
  );
  assert.equal(labKnowledge(config), "/mnt/rec/Knowledge");
  // Nothing to go on
  writeFileSync(config, `[web]\nport = 8090\n`);
  rmSync(join(dir, "config.state.json"));
  assert.throws(() => labKnowledge(config), /no \[recording\] prefix/);

  assert.equal(knowledgeFolder({ data: "K" }, {}, dir), join(dir, "K"));
  assert.equal(
    knowledgeFolder({}, { CUTTLEFISH_DATA: "/k" }, "/nowhere"),
    "/k",
  );
  assert.throws(
    () => knowledgeFolder({}, {}, "/nowhere"),
    /no knowledge folder/,
  );
  mkdirSync(join(dir, "cwd"));
  writeFileSync(
    join(dir, "cwd", "config.toml"),
    `[cuttlefish]\nknowledge = "K2"\n`,
  );
  assert.equal(
    knowledgeFolder({}, {}, join(dir, "cwd")),
    join(dir, "cwd", "K2"),
  );
  rmSync(dir, { recursive: true });
});

test("the TOML reader takes the lab's config", () => {
  const tables = readToml(
    `top = 1\n[a]\nx = "quoted \\"inner\\""  # note\ny = 'single'\nz = ["-c:v", "h264"]\n[b.c]\nn = 8090\n`,
  );
  assert.deepEqual(tables, {
    "": { top: "1" },
    a: { x: 'quoted "inner"', y: "single", z: '["-c:v", "h264"]' },
    "b.c": { n: "8090" },
  });
});
