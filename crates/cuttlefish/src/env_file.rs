//! The env file `scripts/run.sh` loads, read the same way by the CLI:
//! `$PROCON_ENV`, else `~/.config/procon/env`, else `.env` in the working
//! directory. Lines are `KEY=value` (`#` comments, an optional `export `,
//! one pair of surrounding quotes stripped); variables already set in the
//! shell win. Secrets such as `ANTHROPIC_API_KEY` and `DISCORD_USER_TOKEN`
//! live there, never on the command line or in a config file.

use alloc::string::String;
use alloc::vec::Vec;
use std::path::PathBuf;

/// The env file to read, if one exists
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PROCON_ENV").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    [
        PathBuf::from(home).join(".config/procon/env"),
        PathBuf::from(".env"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// The `KEY=value` pairs of an env file's text
pub fn parse(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let valid = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            continue;
        }
        let value = match value.as_bytes() {
            [q, .., r] if q == r && (*q == b'"' || *q == b'\'') => &value[1..value.len() - 1],
            _ => value,
        };
        out.push((String::from(key), String::from(value)));
    }
    out
}

/// Sets the variables of the env file that are not set yet; returns the
/// file read, if any. Call it at the start of `main`, before any thread
/// runs, since setting the environment is not thread-safe.
pub fn load() -> Option<PathBuf> {
    let path = path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    for (key, value) in parse(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: called from `main` before any other thread exists
            unsafe { std::env::set_var(key, value) };
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_like_the_shell_script() {
        let vars = parse(
            "# secrets\nANTHROPIC_API_KEY=sk-1\r\nexport DISCORD_USER_TOKEN=\"abc=def\"\n\nbad key=1\n=2\nX='q'\nNOEQ\n",
        );
        assert_eq!(
            vars,
            [
                (String::from("ANTHROPIC_API_KEY"), String::from("sk-1")),
                (String::from("DISCORD_USER_TOKEN"), String::from("abc=def")),
                (String::from("X"), String::from("q")),
            ]
        );
    }
}
