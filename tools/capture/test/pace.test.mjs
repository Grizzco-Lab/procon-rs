import assert from "node:assert/strict";
import { test } from "node:test";
import { Pace, Stop, estimateSeconds, parseRange } from "../lib/pace.mjs";

/** A clock that does not wait: what was slept is recorded */
function fakeIo(random = 0.5) {
  const io = {
    slept: [],
    waits: [],
    time: 0,
    date: "2026-09-27",
    stopAfter: Infinity,
  };
  io.random = () => random;
  io.sleep = async (ms) => {
    io.slept.push(ms);
    io.time += ms;
    return io.slept.length <= io.stopAfter;
  };
  io.now = () => io.time;
  io.today = () => io.date;
  io.onWait = (line) => io.waits.push(line);
  return io;
}

test("ranges parse as written for the discord fetcher", () => {
  assert.deepEqual(parseRange("4-10"), [4, 10]);
  assert.deepEqual(parseRange("6"), [6, 6]);
  assert.deepEqual(parseRange(" 0.5 - 2 "), [0.5, 2]);
  assert.throws(() => parseRange("10-4"));
  assert.throws(() => parseRange("x"));
  assert.throws(() => parseRange("1-2-3"));
});

test("the first action waits for nothing; the rest wait a delay in the range", async () => {
  const io = fakeIo(0.5);
  const pace = new Pace({ delay: [4, 10], pauseEvery: [100, 100] }, io);
  const day = { day: "", actions: 0 };
  await pace.before(day);
  assert.deepEqual(io.slept, []);
  await pace.before(day);
  await pace.before(day);
  assert.deepEqual(io.slept, [7000, 7000]);
  assert.equal(pace.actions, 3);
  assert.deepEqual(day, { day: "2026-09-27", actions: 3 });
});

test("a longer pause comes every so many actions", async () => {
  const io = fakeIo(0);
  // random 0: the delay is 4 s, the pause 60 s, every 3 actions
  const pace = new Pace(
    { delay: [4, 10], pauseEvery: [3, 5], pause: [60, 180] },
    io,
  );
  const day = { day: "", actions: 0 };
  for (let i = 0; i < 8; i++) await pace.before(day);
  // Actions 2, 3 wait 4 s; the 4th (3 since the start) pauses too, then 4
  // more delays until the next pause at the 7th
  assert.deepEqual(io.slept, [4000, 4000, 64000, 4000, 4000, 64000, 4000]);
  assert.deepEqual(io.waits, ["pausing 60 s", "pausing 60 s"]);
});

test("caps stop the run with the reason", async () => {
  const io = fakeIo();
  const pace = new Pace({ maxActions: 2, dailyCap: 100 }, io);
  const day = { day: "", actions: 0 };
  await pace.before(day);
  await pace.before(day);
  await assert.rejects(
    pace.before(day),
    (e) => e instanceof Stop && e.reason === "max-actions",
  );

  const daily = new Pace({ maxActions: null, dailyCap: 5 }, fakeIo());
  const used = { day: "2026-09-27", actions: 5 };
  await assert.rejects(daily.before(used), (e) => e.reason === "daily-cap");
  // Another day: the count starts over
  const later = { day: "2026-09-26", actions: 5 };
  await daily.before(later);
  assert.deepEqual(later, { day: "2026-09-27", actions: 1 });

  const timed = fakeIo();
  const minutes = new Pace(
    { maxActions: null, dailyCap: null, maxMinutes: 1 },
    timed,
  );
  await minutes.before({ day: "", actions: 0 });
  timed.time += 61_000;
  await assert.rejects(
    minutes.before({ day: "", actions: 0 }),
    (e) => e.reason === "max-minutes",
  );
});

test("an interrupted wait stops the run", async () => {
  const io = fakeIo();
  io.stopAfter = 0;
  const pace = new Pace({}, io);
  const day = { day: "", actions: 0 };
  await pace.before(day);
  await assert.rejects(pace.before(day), (e) => e.reason === "interrupted");
  const flagged = new Pace({}, fakeIo());
  flagged.interrupted = true;
  await assert.rejects(flagged.before(day), (e) => e.reason === "interrupted");
});

test("the estimate sums delays, loads and pauses", () => {
  const options = { delay: [4, 10], pauseEvery: [20, 20], pause: [60, 180] };
  // 40 actions: 40 * (7 + 3) + 2 pauses of 120 s
  assert.equal(estimateSeconds(40, options), 640);
});
