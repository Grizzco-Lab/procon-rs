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
- **Translate the shell.** App names, status words and the View menu stay
  English in Chinese mode; only Cuttlefish and Knowledge are translated.
- **Load apps on first use.** Every app's script loads with the page today.
  Hidden apps do no work, but the page grows with each app; load an app's
  script the first time it is opened, and free big models (embedder, YOLO)
  in the studio after a long idle time.
- **Layout regression checks.** A scripted headless-Chrome pass that measures
  key boxes (the 3D stage, the chat bar, the one-row top bar) in every theme,
  so layout breakage shows up before the user sees it.

## Server

- **Unknown paths answer 405, not 404**, because the POST-only routes reject
  every other path first; `/favicon.ico` hits it too. Add a favicon (the app
  icon) and a proper 404 for unknown paths.
- **Inkspector frames sometimes fail with 400**: ffmpeg reports "non
  monotonically increasing dts" for some frame ranges right after a restart
  (reproduced with curl). Look at how windows are cut (seek, `-copyts`).

## Process

- Test studios use `[video] input = ""` and scratch on disk
  (`~/.cache/claude-scratch/`), never long screen grabs or big files in /tmp.
