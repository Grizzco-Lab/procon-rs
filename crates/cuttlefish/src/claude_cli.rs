//! The Claude Code CLI (`claude -p`, headless) as the model backend.
//!
//! Runs the locally installed and logged-in `claude` on the user's own
//! Claude subscription: for personal testing when the API key has no credit.
//! The prompt goes to the CLI unchanged. The system prompt (persona, rules
//! and digest) replaces Claude Code's own with `--system-prompt`, so the
//! coding assistant does not leak into the answers; the user turn (knowledge
//! excerpts, glossary, comments, frames and the task) is one `stream-json`
//! message on stdin with text and base64 JPEG image blocks, so no file tool
//! is needed and the CLI runs with no tools at all (`--tools ""`,
//! `--restricted`, no MCP servers, no settings files) in an empty temporary
//! folder. Earlier turns of a conversation are rendered into the message,
//! and a JSON schema is asked for in words; the caller parses the answer
//! leniently. The CLI never sees `ANTHROPIC_API_KEY` or
//! `ANTHROPIC_AUTH_TOKEN`, which it would bill instead of the subscription.
//!
//! Prompt caching: the opening blocks (a chat's frames) come first here
//! too, but the CLI places its own cache breakpoints (the system prompt
//! and the end of the message), so only the system prompt is read back
//! from the cache; a follow-up about the same moment sends its frames
//! again at full price. Answers count against the subscription's limits,
//! not per token.
//!
//! [`args`], [`message`] and [`parse_output`] are pure; the [`Runner`] trait
//! is the only part that starts a process, so tests use a fake one.

use crate::llm::{Prompt, Reply, Role, Settings, Usage, block_json};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Instant;

/// The CLI's name on PATH
pub const PROGRAM: &str = "claude";
/// Longest run, as the API client's timeout
pub const TIMEOUT: Duration = Duration::from_secs(600);
/// Runs at once at most; further requests wait their turn
pub const MAX_RUNNING: usize = 8;
/// Taken out of the CLI's environment: with a key or token the CLI bills
/// them instead of the subscription; `CLAUDECODE` marks a session started
/// inside Claude Code
pub const ENV_REMOVED: [&str; 3] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDECODE"];
/// The error when the CLI has no account
const NOT_LOGGED_IN: &str =
    "the claude CLI is not logged in: run `claude` once and log in with your Claude subscription";

/// What a run of the CLI produced
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    /// The process exited with 0
    pub success: bool,
    /// Its standard output: `stream-json` lines
    pub stdout: String,
    /// Its standard error
    pub stderr: String,
}

/// Starts the prepared command with text on its stdin and waits for it
pub trait Runner: Send + Sync {
    fn run(&self, command: &mut Command, stdin: &str) -> Result<Output>;
}

/// Runs the process, killing it after [`TIMEOUT`]
pub struct Spawn;

impl Runner for Spawn {
    fn run(&self, command: &mut Command, stdin: &str) -> Result<Output> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "`{}` (the Claude Code CLI) was not found at {}; install it and log in",
                    PROGRAM,
                    command.get_program().to_string_lossy()
                )
            } else {
                anyhow::Error::from(e).context("starting the claude CLI")
            }
        })?;
        // The message can be megabytes of frames: written on its own thread
        // while the outputs are read, so no pipe fills up
        let mut input = child.stdin.take().context("no stdin")?;
        let text = stdin.to_string();
        let writer = std::thread::spawn(move || {
            let _ = input.write_all(text.as_bytes());
        });
        fn read<R: Read + Send + 'static>(stream: Option<R>) -> std::thread::JoinHandle<String> {
            std::thread::spawn(move || {
                let mut out = String::new();
                if let Some(mut stream) = stream {
                    let _ = stream.read_to_string(&mut out);
                }
                out
            })
        }
        let stdout = read(child.stdout.take());
        let stderr = read(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().context("waiting for the claude CLI")? {
                break status;
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                bail!(
                    "the claude CLI took longer than {} s and was stopped",
                    TIMEOUT.as_secs()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let _ = writer.join();
        Ok(Output {
            success: status.success(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

/// The CLI: where it is and how it is run
pub struct Cli {
    program: PathBuf,
    runner: Box<dyn Runner>,
}

impl Cli {
    /// The `claude` on PATH, if any
    pub fn find() -> Option<Self> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(PROGRAM))
            .find(|p| p.is_file())
            .map(|program| Self::with_runner(program, Box::new(Spawn)))
    }

    /// A CLI at `program`, run by `runner` (tests use a fake one)
    pub fn with_runner(program: PathBuf, runner: Box<dyn Runner>) -> Self {
        Cli { program, runner }
    }

    /// The command for a prompt's system prompt: [`args`], run in `cwd`,
    /// without [`ENV_REMOVED`]
    pub fn command(&self, settings: &Settings, system: &str, cwd: &Path) -> Command {
        let mut command = Command::new(&self.program);
        command.args(args(settings, system)).current_dir(cwd);
        for name in ENV_REMOVED {
            command.env_remove(name);
        }
        command
    }

    /// Sends a prompt: one run of the CLI in a fresh empty folder, removed
    /// afterwards
    pub fn send(&self, settings: &Settings, prompt: &Prompt) -> Result<Reply> {
        let _slot = Slot::take();
        let dir = scratch_dir()?;
        let mut command = self.command(settings, &prompt.system, &dir);
        let stdin = message(prompt);
        log::debug!(
            "{}: {} blocks, {} bytes on stdin",
            self.program.display(),
            prompt.user.len(),
            stdin.len()
        );
        let run = self.runner.run(&mut command, &stdin);
        let _ = std::fs::remove_dir_all(&dir);
        parse_output(&run?)
    }
}

/// The arguments: headless, `stream-json` both ways, no tools, no MCP
/// servers, no settings files, no session kept, the system prompt replacing
/// Claude Code's own, the effort, and the model: the configured one, else
/// [`crate::llm::DEFAULT_MODEL`], never the CLI's own default
pub fn args(settings: &Settings, system: &str) -> Vec<String> {
    [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--tools",
        "",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--permission-prompts",
        "none",
        "--effort",
        &settings.effort,
        "--model",
        settings
            .model
            .as_deref()
            .unwrap_or(crate::llm::DEFAULT_MODEL),
        "--system-prompt",
        system,
    ]
    .iter()
    .map(|s| String::from(*s))
    .collect()
}

/// The `stream-json` user message for a prompt, one line: the opening
/// blocks (a chat's frames) first, then the earlier turns rendered as
/// text, the user blocks as text and image blocks, and the schema asked
/// for in words
pub fn message(prompt: &Prompt) -> String {
    let mut content: Vec<Value> = prompt.opening.iter().map(block_json).collect();
    if !prompt.history.is_empty() {
        let mut s = String::from("<conversation>\n");
        for turn in &prompt.history {
            let role = match turn.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            s.push_str(&alloc::format!(
                "<turn role=\"{role}\">\n{}\n</turn>\n",
                turn.text.trim()
            ));
        }
        s.push_str("</conversation>\nThe conversation so far is above; the new message follows.");
        content.push(json!({"type": "text", "text": s}));
    }
    content.extend(prompt.user.iter().map(block_json));
    if let Some(schema) = &prompt.schema {
        content.push(json!({
            "type": "text",
            "text": alloc::format!(
                "Answer with one JSON object that follows this JSON schema, and nothing else \
                 (no prose around it, no code fence):\n{schema}"
            ),
        }));
    }
    let mut line =
        json!({"type": "user", "message": {"role": "user", "content": content}}).to_string();
    line.push('\n');
    line
}

/// Reads a run's output: the `result` line's text and usage, or a clear
/// error (a missing login named as such)
pub fn parse_output(out: &Output) -> Result<Reply> {
    let result = out
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|v| v["type"] == "result");
    let Some(v) = result else {
        let said = tail(if out.stderr.trim().is_empty() {
            &out.stdout
        } else {
            &out.stderr
        });
        if login_problem(&said) {
            bail!(NOT_LOGGED_IN);
        }
        if said.is_empty() {
            bail!("the claude CLI ended without a result");
        }
        bail!("the claude CLI ended without a result: {said}");
    };
    let subtype = v["subtype"].as_str().unwrap_or_default();
    let failed = v["is_error"].as_bool().unwrap_or(false) || subtype.starts_with("error");
    if failed {
        let mut said = String::from(v["result"].as_str().unwrap_or_default().trim());
        if said.is_empty() {
            said = v["errors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("; ");
        }
        if said.is_empty() {
            said = String::from(subtype);
        }
        if login_problem(&said) {
            bail!(NOT_LOGGED_IN);
        }
        bail!("the claude CLI failed: {said}");
    }
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Ok(Reply {
        text: String::from(v["result"].as_str().unwrap_or_default()),
        stop_reason: String::from(v["stop_reason"].as_str().unwrap_or("end_turn")),
        usage: Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        },
        model: main_model(&v["modelUsage"]),
    })
}

/// The model that wrote the answer, from a result's `modelUsage` (the CLI
/// chooses one when none is configured, and may use a small one on the
/// side): the one with the most output tokens
fn main_model(usage: &Value) -> Option<String> {
    let models = usage.as_object()?;
    log::debug!(
        "claude CLI answered with {}",
        models.keys().cloned().collect::<Vec<_>>().join(", ")
    );
    models
        .iter()
        .max_by_key(|(_, u)| u["outputTokens"].as_u64().unwrap_or(0))
        .map(|(name, _)| name.clone())
}

/// Whether an error is about the account rather than the request
fn login_problem(said: &str) -> bool {
    let s = said.to_lowercase();
    [
        "not logged in",
        "log in",
        "login",
        "authentication",
        "invalid api key",
        "oauth",
    ]
    .iter()
    .any(|w| s.contains(w))
}

/// The last few hundred characters of a text, trimmed
fn tail(text: &str) -> String {
    let text = text.trim();
    let start = text.char_indices().rev().nth(600).map_or(0, |(i, _)| i);
    String::from(text[start..].trim_start())
}

/// Number of runs so far, naming the scratch folders
static RUNS: AtomicUsize = AtomicUsize::new(0);

/// A fresh empty folder for one run
fn scratch_dir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(alloc::format!(
        "cuttlefish-claude-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).with_context(|| alloc::format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Runs under way
static RUNNING: Mutex<usize> = Mutex::new(0);
/// Signalled when one ends
static FREED: Condvar = Condvar::new();

/// One of the [`MAX_RUNNING`] places, held while a run lasts
struct Slot;

impl Slot {
    fn take() -> Slot {
        let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
        while *running >= MAX_RUNNING {
            running = FREED.wait(running).unwrap_or_else(PoisonError::into_inner);
        }
        *running += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        *RUNNING.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        FREED.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Backend, Block, Client, Turn};
    use std::sync::Arc;

    /// What the fake runner was asked to run
    #[derive(Debug, Default)]
    struct Seen {
        program: String,
        args: Vec<String>,
        envs: Vec<(String, Option<String>)>,
        cwd: Option<PathBuf>,
        cwd_existed: bool,
        stdin: String,
    }

    /// Answers with a canned output and keeps what it was asked to run
    struct Fake {
        output: Output,
        seen: Arc<Mutex<Seen>>,
    }

    impl Runner for Fake {
        fn run(&self, command: &mut Command, stdin: &str) -> Result<Output> {
            let cwd = command.get_current_dir().map(Path::to_path_buf);
            *self.seen.lock().unwrap() = Seen {
                program: command.get_program().to_string_lossy().into_owned(),
                args: command
                    .get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect(),
                envs: command
                    .get_envs()
                    .map(|(k, v)| {
                        (
                            k.to_string_lossy().into_owned(),
                            v.map(|v| v.to_string_lossy().into_owned()),
                        )
                    })
                    .collect(),
                cwd_existed: cwd.as_ref().is_some_and(|d| d.is_dir()),
                cwd,
                stdin: String::from(stdin),
            };
            Ok(self.output.clone())
        }
    }

    /// A successful run: init and assistant lines, then the result
    fn success(text: &str) -> Output {
        let result = json!({
            "type": "result", "subtype": "success", "is_error": false,
            "duration_ms": 1200, "num_turns": 1, "result": text,
            "session_id": "s", "total_cost_usd": 0.0,
            "usage": {"input_tokens": 12, "output_tokens": 7, "cache_read_input_tokens": 300,
                      "cache_creation_input_tokens": 4}
        });
        Output {
            success: true,
            stdout: alloc::format!(
                "{}\n{}\n{}\n",
                json!({"type": "system", "subtype": "init", "tools": []}),
                json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}),
                result
            ),
            stderr: String::new(),
        }
    }

    fn prompt() -> Prompt {
        Prompt {
            system: String::from("You are Cuttlefish."),
            opening: Vec::new(),
            history: alloc::vec![
                Turn {
                    role: Role::User,
                    text: String::from("Hi")
                },
                Turn {
                    role: Role::Assistant,
                    text: String::from("Hello!")
                },
            ],
            user: alloc::vec![
                Block::Text(String::from("<knowledge>\n</knowledge>")),
                Block::Jpeg(alloc::vec![1, 2, 3]),
                Block::Text(String::from("Review this.")),
            ],
            schema: Some(json!({"type": "object"})),
        }
    }

    fn make(output: Output, settings: Settings) -> (Client, Arc<Mutex<Seen>>) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let fake = Fake {
            output,
            seen: Arc::clone(&seen),
        };
        let cli = Cli::with_runner(PathBuf::from("/opt/fake/claude"), Box::new(fake));
        (Client::with_cli(cli, settings), seen)
    }

    /// The argument after a flag
    fn after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    }

    #[test]
    fn builds_the_command_line() {
        let settings = Settings {
            model: Some(String::from("sonnet")),
            effort: String::from("low"),
            backend: Backend::ClaudeCli,
            ..Settings::default()
        };
        let (client, seen) = make(success("ok"), settings);
        assert_eq!(client.backend(), Backend::ClaudeCli);
        assert_eq!(client.send(&prompt()).unwrap().text, "ok");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.program, "/opt/fake/claude");
        let a = &seen.args;
        assert_eq!(a[0], "-p");
        assert_eq!(after(a, "--input-format"), Some("stream-json"));
        assert_eq!(after(a, "--output-format"), Some("stream-json"));
        assert_eq!(after(a, "--tools"), Some(""));
        assert!(a.iter().any(|x| x == "--restricted"));
        assert!(a.iter().any(|x| x == "--strict-mcp-config"));
        assert!(a.iter().any(|x| x == "--no-session-persistence"));
        assert_eq!(after(a, "--permission-prompts"), Some("none"));
        assert_eq!(after(a, "--system-prompt"), Some("You are Cuttlefish."));
        assert!(!a.iter().any(|x| x == "--append-system-prompt"));
        assert_eq!(after(a, "--effort"), Some("low"));
        assert_eq!(after(a, "--model"), Some("sonnet"));
        // An empty folder of its own, gone afterwards
        let cwd = seen.cwd.clone().unwrap();
        assert!(cwd.starts_with(std::env::temp_dir()));
        assert!(
            cwd.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cuttlefish-claude-")
        );
        assert!(seen.cwd_existed);
        assert!(!cwd.exists());
        // No model configured: ours, at the default effort, never the
        // CLI's own choice
        let (client, seen) = make(success("ok"), Settings::default());
        client.send(&prompt()).unwrap();
        let a = &seen.lock().unwrap().args;
        assert_eq!(after(a, "--model"), Some("claude-opus-5-5"));
        assert_eq!(after(a, "--effort"), Some("medium"));
    }

    #[test]
    fn the_cli_never_sees_the_api_key() {
        let (client, seen) = make(success("ok"), Settings::default());
        client.send(&prompt()).unwrap();
        let seen = seen.lock().unwrap();
        for name in ENV_REMOVED {
            assert!(
                seen.envs.contains(&(String::from(name), None)),
                "{name} not removed: {:?}",
                seen.envs
            );
        }
        assert!(seen.envs.iter().all(|(_, v)| v.is_none()));
    }

    #[test]
    fn renders_the_message() {
        let (client, seen) = make(success("ok"), Settings::default());
        client.send(&prompt()).unwrap();
        let stdin = seen.lock().unwrap().stdin.clone();
        assert!(stdin.ends_with('\n'));
        assert_eq!(stdin.trim().lines().count(), 1);
        let v: Value = serde_json::from_str(&stdin).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        let content = v["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 5);
        let history = content[0]["text"].as_str().unwrap();
        assert!(history.starts_with("<conversation>\n<turn role=\"user\">\nHi\n</turn>\n"));
        assert!(history.contains("<turn role=\"assistant\">\nHello!\n</turn>\n</conversation>"));
        assert_eq!(content[1]["text"], "<knowledge>\n</knowledge>");
        assert_eq!(content[2]["type"], "image");
        assert_eq!(content[2]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[2]["source"]["data"], "AQID");
        assert_eq!(content[3]["text"], "Review this.");
        let schema = content[4]["text"].as_str().unwrap();
        assert!(schema.starts_with("Answer with one JSON object"));
        assert!(schema.contains(r#"{"type":"object"}"#));
        // Nothing to say about history or schema when there are none
        let bare = Prompt {
            system: String::new(),
            opening: Vec::new(),
            history: Vec::new(),
            user: alloc::vec![Block::Text(String::from("hi"))],
            schema: None,
        };
        let v: Value = serde_json::from_str(&message(&bare)).unwrap();
        assert_eq!(v["message"]["content"].as_array().unwrap().len(), 1);
        // The opening blocks (a chat's frames) come before the conversation
        let mut framed = prompt();
        framed.opening = alloc::vec![
            Block::Text(String::from("Frame at 1.00 s:")),
            Block::Jpeg(alloc::vec![4, 5])
        ];
        let v: Value = serde_json::from_str(&message(&framed)).unwrap();
        let content = v["message"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 7);
        assert_eq!(content[0]["text"], "Frame at 1.00 s:");
        assert_eq!(content[1]["type"], "image");
        assert!(
            content[2]["text"]
                .as_str()
                .unwrap()
                .starts_with("<conversation>")
        );
    }

    #[test]
    fn parses_results() {
        let reply = parse_output(&success("The answer.")).unwrap();
        assert_eq!(reply.text, "The answer.");
        assert_eq!(reply.stop_reason, "end_turn");
        assert_eq!(
            reply.usage,
            Usage {
                input: 12,
                output: 7,
                cache_read: 300,
                cache_write: 4
            }
        );
        // A result naming no model names none; the client then says which
        // it asked for
        assert_eq!(reply.model, None);
        // The sent client logs and passes it through
        let (client, _) = make(success("fine"), Settings::default());
        assert_eq!(client.send(&prompt()).unwrap().text, "fine");
        assert!(alloc::format!("{client:?}").contains("ClaudeCli"));
        // The model the CLI chose: the one that wrote the most, not the
        // small one it used on the side
        let mut chose = success("The answer.");
        chose.stdout = chose.stdout.replace(
            "\"type\":\"result\"",
            "\"modelUsage\":{\"claude-haiku-4-5\":{\"outputTokens\":3},\
             \"claude-opus-4-8\":{\"outputTokens\":900}},\"type\":\"result\"",
        );
        let (client, _) = make(chose, Settings::default());
        let reply = client.send(&prompt()).unwrap();
        assert_eq!(
            client.answered_by(&reply),
            crate::llm::AnsweredBy {
                backend: Some(Backend::ClaudeCli),
                model: Some(String::from("claude-opus-4-8")),
                effort: Some(String::from("medium")),
            }
        );
        // Else the one the CLI was told to run
        let told = Settings {
            model: Some(String::from("sonnet")),
            ..Settings::default()
        };
        let (client, _) = make(success("ok"), told);
        assert_eq!(
            client.send(&prompt()).unwrap().model.as_deref(),
            Some("sonnet")
        );
        let (client, _) = make(success("ok"), Settings::default());
        assert_eq!(
            client.send(&prompt()).unwrap().model.as_deref(),
            Some("claude-opus-5-5")
        );
    }

    #[test]
    fn explains_errors() {
        let error = |result: Value| Output {
            success: false,
            stdout: alloc::format!("{result}\n"),
            stderr: String::new(),
        };
        let e = parse_output(&error(json!({
            "type": "result", "subtype": "success", "is_error": true,
            "result": "Not logged in · Please run /login"
        })))
        .unwrap_err();
        assert!(e.to_string().contains("not logged in"), "{e}");
        let e = parse_output(&error(json!({
            "type": "result", "subtype": "error_during_execution",
            "errors": ["rate limit reached", "try later"]
        })))
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "the claude CLI failed: rate limit reached; try later"
        );
        let e = parse_output(&error(
            json!({"type": "result", "subtype": "error_max_turns"}),
        ))
        .unwrap_err();
        assert_eq!(e.to_string(), "the claude CLI failed: error_max_turns");
        // No result line: stderr explains, or nothing does
        let e = parse_output(&Output {
            success: false,
            stdout: String::new(),
            stderr: String::from("Error: something broke\n"),
        })
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "the claude CLI ended without a result: Error: something broke"
        );
        let e = parse_output(&Output {
            success: false,
            stdout: String::new(),
            stderr: String::from("Invalid API key · Please run /login"),
        })
        .unwrap_err();
        assert!(e.to_string().contains("not logged in"));
        assert_eq!(
            parse_output(&Output::default()).unwrap_err().to_string(),
            "the claude CLI ended without a result"
        );
        // A runner that fails (the CLI missing) fails the send
        struct Missing;
        impl Runner for Missing {
            fn run(&self, _: &mut Command, _: &str) -> Result<Output> {
                bail!("`claude` (the Claude Code CLI) was not found")
            }
        }
        let cli = Cli::with_runner(PathBuf::from("claude"), Box::new(Missing));
        let e = Client::with_cli(cli, Settings::default())
            .send(&prompt())
            .unwrap_err();
        assert!(e.to_string().contains("was not found"));
    }

    #[test]
    fn the_real_runner_reports_a_missing_program() {
        let cli = Cli::with_runner(PathBuf::from("/nonexistent/claude"), Box::new(Spawn));
        let e = Client::with_cli(cli, Settings::default())
            .send(&prompt())
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("was not found at /nonexistent/claude"),
            "{e}"
        );
    }

    #[test]
    fn tails_long_text() {
        assert_eq!(tail("  short  "), "short");
        let long: String = core::iter::repeat_n('x', 700).collect();
        assert_eq!(tail(&long).chars().count(), 601);
    }
}
