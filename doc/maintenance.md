# Maintenance backlog

Structural debt to pay down in a quiet period, when no feature work is in
flight. Each item says why it matters; tick it off (or delete it) once done.

## Workspace layout

- **Virtual workspace.** The root `Cargo.toml` becomes a virtual workspace and
  every package lives under `crates/`, as in rustc: `procon-proxy`,
  `procon-studio` (or `procon`), `gameplay-data`, `gameplay-vision`,
  `cuttlefish`, … The proxy then cannot pick up studio dependencies by
  accident: today a `studio` feature on the root package keeps them out of the
  Pi build (`d2703bc`). Before that, `cb7b2d5` and `f13bc86` broke
  `scripts/deploy.sh` by making the musl build compile candle and tokenizers.
- **Name.** An umbrella name for the whole repository (Grizzco Lab, Grizzco
  Studio…), while procon (the proxy and the studio) keeps its own name and
  credit as a finished, self-contained project.

## Web

- **Scope CSS per app.** Class names are global, and `fe72379` reused
  `.p-controller`, which shrank the Studio's 3D view. Give each app's styles a
  prefix or scope them under the app's section, and add a check for duplicate
  class names between apps.
- **Retake doc/index.html screenshots.** The shared player (`web/player.js`,
  `8834637`) moved the Inkspector's frame chip into the player's controls,
  and the shell, rail, themes and Cuttlefish have changed since.
- [x] **Translate the shell.** App names, the status chips and the View menu
  go through `i18n.js`; `app.js` redraws the words it writes on
  `lang-change`.
- **Load apps on first use.** All 16 scripts and styles (about 730 KB
  uncompressed) load with the page today. Hidden apps do no work, but the
  first load grows with each app; load an app's script the first time it is
  opened. Big models in the studio stay loaded (the user's call); if memory
  ever gets tight, decide from the machine's free memory rather than an idle
  timer.
- [x] **Layout regression checks.** `scripts/layout-check.mjs` (see
  CONTRIBUTING.md) measures the 3D stage, the chat bar, the one-row top bar
  and sideways overflow in every app, theme and language, at 1440 px and
  phone width. Its first run found two phone overflows (the Inkspector's
  sessions note, the Vision frame panel's head), fixed.

## Server

- [x] **Unknown paths answer 405, not 404**, because the POST-only routes
  reject every other path first. Routes now check their path before their
  method, and the apps' method-first routes are gated by their prefix;
  `/favicon.ico` and the page's icon link are the Studio's app icon.
- [x] **Inkspector frames sometimes fail with 400** ("non monotonically
  increasing dts", 25 of 60 random windows, not only after a restart): the
  encoder's default 1/fps time base rounded neighbouring frames onto one
  tick after the seek offset. Windows are encoded at the file's time base
  (`-enc_time_base demux`); 200 random windows decode, and a window's frames
  match those decoded from the start.

## Tests

- **Flaky lock tests** (`crates/cuttlefish` `lock::tests`, e.g.
  `one_writer_at_a_time_and_who_it_is`, `records_left_behind`) sometimes fail
  under full parallel load and pass alone. Make them independent of timing.

## Process

- Test studios use `[video] input = ""` and scratch on disk
  (`~/.cache/claude-scratch/`), never long screen grabs or big files in /tmp.
