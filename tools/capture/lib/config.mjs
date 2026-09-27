// The knowledge folder, found as the cuttlefish CLI finds it: `--data`,
// else the studio's config (`--config`, or `./config.toml` when there is
// one): `[cuttlefish] knowledge` relative to the config file, else
// `Knowledge` next to the sessions' folder (`[inspect] root`, else the
// folder of the recording prefix, the dashboard's saved choice in
// `<config>.state.json` before `[recording] prefix`); else
// `$CUTTLEFISH_DATA`.

import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

/** The string values of a TOML file's tables, `{table: {key: value}}`.
 * Enough for the studio's config: `[table]` headers, `key = "string"`
 * lines; other values are kept as their raw text. */
export function readToml(text) {
  const tables = {};
  let table = "";
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/\s+#.*$/, "").trim();
    if (!line || line.startsWith("#")) continue;
    const header = /^\[([^\]]+)\]$/.exec(line);
    if (header) {
      table = header[1].trim();
      continue;
    }
    const pair = /^([A-Za-z0-9_.-]+)\s*=\s*(.+)$/.exec(line);
    if (!pair) continue;
    let value = pair[2].trim();
    const quoted =
      /^"((?:[^"\\]|\\.)*)"$/.exec(value) ?? /^'([^']*)'$/.exec(value);
    if (quoted) value = quoted[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\");
    (tables[table] ??= {})[pair[1]] = value;
  }
  return tables;
}

/** The knowledge folder of the studio with this config */
export function studioKnowledge(configPath) {
  const config = readToml(readFileSync(configPath, "utf8"));
  const dir = dirname(configPath);
  const knowledge = config.cuttlefish?.knowledge;
  if (knowledge) return resolve(dir, knowledge);
  let sessions;
  const root = config.inspect?.root;
  if (root) sessions = resolve(dir, root);
  else {
    let prefix = null;
    try {
      const state = JSON.parse(
        readFileSync(configPath.replace(/\.toml$/, "") + ".state.json", "utf8"),
      );
      if (typeof state.prefix === "string") prefix = state.prefix;
    } catch {
      // No saved dashboard state
    }
    prefix ??= config.recording?.prefix;
    if (!prefix) throw new Error(`${configPath} has no [recording] prefix`);
    // As the recorder: "a/b-" lives in "a", "a/b/" in "a/b"
    const parent = dirname(`${prefix}x`);
    sessions = resolve(dir, parent === "" ? "." : parent);
  }
  return join(dirname(sessions), "Knowledge");
}

/** The knowledge folder for these options (`data`, `config`) and this
 * environment; throws with what to give when none is found */
export function knowledgeFolder(
  { data, config },
  env = process.env,
  cwd = process.cwd(),
) {
  if (data) return resolve(cwd, data);
  const path =
    config ?? (existsSync(resolve(cwd, "config.toml")) ? "config.toml" : null);
  if (path) return studioKnowledge(resolve(cwd, path));
  if (env.CUTTLEFISH_DATA) return resolve(cwd, env.CUTTLEFISH_DATA);
  throw new Error(
    "no knowledge folder: run where the studio's config.toml is, or give --config <studio config>, --data <folder> or $CUTTLEFISH_DATA",
  );
}
