// Going slowly, like a person reading: a random delay before every page
// action (a navigation or a scroll), a longer pause now and then, and caps
// per run and per day. The clock and the random source are injectable, so
// tests run without waiting.

/** A range written `4-10` (or one number, `6`) as `[min, max]` */
export function parseRange(text) {
  const bad = () => {
    throw new Error(`${text}: expected a number or a range such as 4-10`);
  };
  const number = (t) => {
    const n = Number(String(t).trim());
    return Number.isFinite(n) && n >= 0 ? n : bad();
  };
  const parts = String(text).split("-");
  if (parts.length > 2) bad();
  const min = number(parts[0]);
  const max = parts.length === 2 ? number(parts[1]) : min;
  if (min > max) bad();
  return [min, max];
}

/** The mean of a range */
export const mean = ([min, max]) => (min + max) / 2;

/** How long `actions` page actions take at these settings, in seconds,
 * about: a mean delay each plus a mean pause per mean stretch, and about
 * `loadSeconds` for each page to load */
export function estimateSeconds(actions, options, loadSeconds = 3) {
  const pauses = Math.floor(actions / Math.max(1, mean(options.pauseEvery)));
  return (
    actions * (mean(options.delay) + loadSeconds) + pauses * mean(options.pause)
  );
}

/** Why a run ended before its work did */
export class Stop extends Error {
  constructor(reason, detail) {
    super(detail);
    this.reason = reason;
  }
}

export const DEFAULTS = Object.freeze({
  /** Seconds between page actions */
  delay: [4, 10],
  /** Page actions between longer pauses */
  pauseEvery: [15, 30],
  /** Seconds of a longer pause */
  pause: [60, 180],
  /** Page actions per day (UTC), all runs together */
  dailyCap: 800,
  /** Page actions this run */
  maxActions: 400,
  /** Minutes this run */
  maxMinutes: null,
});

export class Pace {
  /**
   * @param {object} options ranges as `[min, max]`, caps as numbers or null
   * @param {object} [io] `random()` in [0, 1), `sleep(ms)` (resolves false
   *   when interrupted), `now()` in ms, `today()` as `YYYY-MM-DD` (UTC)
   */
  constructor(options = {}, io = {}) {
    this.options = { ...DEFAULTS, ...options };
    this.random = io.random ?? Math.random;
    this.sleep =
      io.sleep ??
      ((ms) => new Promise((resolve) => setTimeout(() => resolve(true), ms)));
    this.now = io.now ?? Date.now;
    this.today = io.today ?? (() => new Date().toISOString().slice(0, 10));
    this.started = this.now();
    /** Page actions this run */
    this.actions = 0;
    /** Actions since the last pause, and when the next one comes */
    this.sinceLastPause = 0;
    this.nextPauseAt = Math.round(this.draw(this.options.pauseEvery));
    /** A note per pause or wait, for the log */
    this.onWait = io.onWait ?? (() => {});
    this.interrupted = false;
  }

  /** A number drawn from a range */
  draw([min, max]) {
    return min + this.random() * (max - min);
  }

  /** Checks the caps, then waits the delay (and a pause when due) before
   * the next page action, and counts it. `day` is the state's day record
   * (`{day, actions}`), kept up to date. Throws a `Stop` at a cap or when
   * interrupted. */
  async before(day) {
    const o = this.options;
    if (this.interrupted) throw new Stop("interrupted", "interrupted");
    if (o.maxActions != null && this.actions >= o.maxActions)
      throw new Stop(
        "max-actions",
        `${this.actions} page actions: this run's limit; the next run continues`,
      );
    if (
      o.maxMinutes != null &&
      this.now() - this.started >= o.maxMinutes * 60_000
    )
      throw new Stop(
        "max-minutes",
        `${o.maxMinutes} minutes: this run's limit; the next run continues`,
      );
    const today = this.today();
    if (day.day !== today) {
      day.day = today;
      day.actions = 0;
    }
    if (o.dailyCap != null && day.actions >= o.dailyCap)
      throw new Stop(
        "daily-cap",
        `${day.actions} page actions today: the daily cap; run again tomorrow`,
      );
    if (this.actions > 0) {
      let wait = this.draw(o.delay);
      if (this.sinceLastPause >= this.nextPauseAt) {
        const pause = this.draw(o.pause);
        this.onWait(`pausing ${pause.toFixed(0)} s`);
        wait += pause;
        this.sinceLastPause = 0;
        this.nextPauseAt = Math.round(this.draw(o.pauseEvery));
      }
      if (!(await this.sleep(wait * 1000)))
        throw new Stop("interrupted", "interrupted");
      if (this.interrupted) throw new Stop("interrupted", "interrupted");
    }
    this.actions++;
    this.sinceLastPause++;
    day.actions++;
  }
}
