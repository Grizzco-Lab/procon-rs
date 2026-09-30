//! The knowledge tools as an MCP server, for the Claude Code CLI.
//!
//! With the CLI backend ([`crate::claude_cli`]) the model looks things up
//! itself: the tools of [`crate::tools`] are offered to `claude -p` as an
//! MCP server over stdio. The store and the embedder stay loaded in the
//! process that asks (the lab, or the `cuttlefish` CLI), which serves the
//! tools on a Unix socket of its own for the length of one answer
//! ([`Listener`]). The MCP server the CLI starts is that process's own
//! binary run as `<program> mcp --socket <path>` ([`config`]): it only
//! relays bytes between its stdin and stdout and the socket ([`relay`]),
//! so it starts at once, loads nothing, and every lookup of an answer is
//! seen by the process that asked for it.
//!
//! The protocol is JSON-RPC 2.0, one message per line ([`serve`],
//! [`handle`]): `initialize` (the client's protocol version is echoed:
//! only the basics every version shares are used), `tools/list`,
//! `tools/call` and `ping`; notifications are read and left unanswered;
//! other methods are "method not found". `cuttlefish mcp --data <folder>`
//! serves the same tools on its own stdin and stdout over a store it loads
//! itself, for any MCP client.

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use anyhow::{Context, Result};
use core::sync::atomic::{AtomicBool, Ordering};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The server's name: the model sees its tools as `mcp__cuttlefish__search`
pub const SERVER: &str = "cuttlefish";

/// The protocol version answered when the client names none
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC's code for a method this server does not have
const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's code for a line that is not JSON
const PARSE_ERROR: i64 = -32700;
/// JSON-RPC's code for JSON that is not a request
const INVALID_REQUEST: i64 = -32600;

/// Tools an MCP server offers
pub trait Tools: Sync {
    /// The tools as `tools/list` gives them: name, description and input
    /// schema of each
    fn list(&self) -> Vec<Value>;
    /// Runs a tool with its arguments: the text for the model, and whether
    /// the call failed
    fn call(&self, name: &str, arguments: &Value) -> (String, bool);
}

/// The model's names of the tools (`mcp__cuttlefish__search`), for the
/// CLI's `--allowedTools`
pub fn allowed(tools: &dyn Tools) -> Vec<String> {
    tools
        .list()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .map(|name| alloc::format!("mcp__{SERVER}__{name}"))
        .collect()
}

/// The `--mcp-config` for the CLI: one stdio server, `program mcp --socket
/// socket`, relaying to the tools served on `socket`
pub fn config(program: &Path, socket: &Path) -> Value {
    json!({
        "mcpServers": {
            SERVER: {
                "type": "stdio",
                "command": program.to_string_lossy(),
                "args": ["mcp", "--socket", socket.to_string_lossy()],
            }
        }
    })
}

/// A JSON-RPC error answer
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The answer to one message; `None` for a notification (no `id`)
pub fn handle(tools: &dyn Tools, message: &Value) -> Option<Value> {
    if !message.is_object() {
        return Some(error(
            Value::Null,
            INVALID_REQUEST,
            "not a JSON-RPC request",
        ));
    }
    let id = message.get("id")?.clone();
    let method = message["method"].as_str().unwrap_or_default();
    let params = &message["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools.list() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let empty = json!({});
            let arguments = match &params["arguments"] {
                Value::Null => &empty,
                a => a,
            };
            let (text, is_error) = tools.call(name, arguments);
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        _ => {
            return Some(error(
                id,
                METHOD_NOT_FOUND,
                &alloc::format!("method not found: {method}"),
            ));
        }
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// Serves the tools on a stream of JSON-RPC lines until it ends: each
/// request's answer is written as one line and flushed
pub fn serve(tools: &dyn Tools, input: impl BufRead, mut output: impl Write) -> Result<()> {
    for line in input.lines() {
        let line = line.context("reading a request")?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(tools, &message),
            Err(e) => Some(error(
                Value::Null,
                PARSE_ERROR,
                &alloc::format!("not JSON: {e}"),
            )),
        };
        if let Some(answer) = answer {
            let mut text = answer.to_string();
            text.push('\n');
            output
                .write_all(text.as_bytes())
                .context("writing an answer")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// The `mcp --socket` subcommand: relays this process's stdin to the tools
/// served on `socket` and their answers to its stdout, until the server
/// closes the connection
pub fn relay(socket: &Path) -> Result<()> {
    relay_with(socket, std::io::stdin(), std::io::stdout())
}

/// [`relay`] between any input and output: the input is copied to the
/// socket on a thread of its own (its end shuts the socket's writing
/// half, so the server sees the end too), the socket's answers to the
/// output
pub fn relay_with(
    socket: &Path,
    mut input: impl Read + Send + 'static,
    mut output: impl Write,
) -> Result<()> {
    let stream = UnixStream::connect(socket).with_context(|| {
        alloc::format!("connecting to the knowledge tools at {}", socket.display())
    })?;
    let mut to_server = stream.try_clone()?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut input, &mut to_server);
        let _ = to_server.shutdown(Shutdown::Write);
    });
    // Answers are lines: copied as they come, each flushed
    let mut reader = BufReader::new(&stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        output.write_all(&line)?;
        output.flush()?;
    }
}

/// The tools served on a Unix socket for one run of the CLI: the relay it
/// starts connects here
pub struct Listener {
    path: PathBuf,
    listener: UnixListener,
    stop: AtomicBool,
    /// The connection being served, so [`Listener::stop`] can end it
    serving: Mutex<Option<UnixStream>>,
}

impl Listener {
    /// Listens on `path` (a new file in a private folder)
    pub fn bind(path: &Path) -> Result<Self> {
        let listener = UnixListener::bind(path)
            .with_context(|| alloc::format!("serving the knowledge tools on {}", path.display()))?;
        Ok(Listener {
            path: path.to_path_buf(),
            listener,
            stop: AtomicBool::new(false),
            serving: Mutex::default(),
        })
    }

    /// Serves each connection in turn until [`Listener::stop`]; true when
    /// a client connected
    pub fn serve(&self, tools: &dyn Tools) -> bool {
        let mut connected = false;
        for stream in self.listener.incoming() {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("knowledge tools: accepting a connection failed: {e}");
                    break;
                }
            };
            connected = true;
            if let Ok(copy) = stream.try_clone() {
                *self.serving.lock().unwrap_or_else(PoisonError::into_inner) = Some(copy);
            }
            if let Err(e) = serve(tools, BufReader::new(&stream), &stream) {
                log::debug!("knowledge tools: the connection ended: {e:#}");
            }
            *self.serving.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        connected
    }

    /// Ends [`Listener::serve`]: the connection being served is shut, and
    /// a waiting accept is woken by a connection of our own
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(s) = self
            .serving
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
        let _ = UnixStream::connect(&self.path);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A line of text, for tests and logs: the text of a `tools/call` answer
pub fn answer_text(answer: &Value) -> Option<String> {
    answer["result"]["content"][0]["text"]
        .as_str()
        .map(ToString::to_string)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Two tools: `echo` answers with its `text`, `fail` fails
    pub struct Echo;

    impl Tools for Echo {
        fn list(&self) -> Vec<Value> {
            alloc::vec![
                json!({"name": "echo", "description": "Echoes", "inputSchema": {"type": "object"}}),
                json!({"name": "fail", "description": "Fails", "inputSchema": {"type": "object"}}),
            ]
        }

        fn call(&self, name: &str, arguments: &Value) -> (String, bool) {
            match name {
                "echo" => (
                    String::from(arguments["text"].as_str().unwrap_or("(nothing)")),
                    false,
                ),
                _ => (alloc::format!("no tool {name}"), true),
            }
        }
    }

    /// A session as the CLI runs it: handshake, list, calls, then noise
    pub fn session_lines() -> String {
        [
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                   "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                              "clientInfo": {"name": "claude-code", "version": "2"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "echo", "arguments": {"text": "hello"},
                              "_meta": {"claudecode/toolUseId": "toolu_1"}}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "fail"}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}),
        ]
        .iter()
        .map(|v| alloc::format!("{v}\n"))
        .collect::<String>()
            + "not json\n\n"
    }

    /// Checks the answers to [`session_lines`]
    pub fn check_answers(out: &str) {
        let answers: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(answers.len(), 7, "{out}");
        let init = &answers[0]["result"];
        assert_eq!(answers[0]["id"], 0);
        assert_eq!(init["protocolVersion"], "2025-11-25");
        assert_eq!(init["serverInfo"]["name"], "cuttlefish");
        assert!(init["capabilities"]["tools"].is_object());
        assert_eq!(answers[1]["result"]["tools"][0]["name"], "echo");
        assert_eq!(answer_text(&answers[2]).as_deref(), Some("hello"));
        assert_eq!(answers[2]["result"]["isError"], false);
        assert_eq!(answers[3]["result"]["isError"], true);
        assert_eq!(answer_text(&answers[3]).as_deref(), Some("no tool fail"));
        assert_eq!(answers[4]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(answers[5]["result"], json!({}));
        assert_eq!(answers[6]["error"]["code"], PARSE_ERROR);
        assert!(answers[6]["id"].is_null());
    }

    #[test]
    fn serves_json_rpc_lines() {
        let mut out = Vec::new();
        serve(&Echo, session_lines().as_bytes(), &mut out).unwrap();
        check_answers(&String::from_utf8(out).unwrap());
        // A notification gets no answer; an array is no request
        assert_eq!(
            handle(
                &Echo,
                &json!({"jsonrpc": "2.0", "method": "notifications/cancelled"})
            ),
            None
        );
        assert_eq!(
            handle(&Echo, &json!([1, 2])).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        // No protocol version asked: ours
        let init = handle(&Echo, &json!({"id": 9, "method": "initialize"})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[test]
    fn configures_the_relay() {
        let c = config(
            Path::new("/opt/lab/grizzco-lab"),
            Path::new("/tmp/x/mcp.sock"),
        );
        let server = &c["mcpServers"]["cuttlefish"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], "/opt/lab/grizzco-lab");
        assert_eq!(
            server["args"],
            json!(["mcp", "--socket", "/tmp/x/mcp.sock"])
        );
        assert_eq!(
            allowed(&Echo),
            ["mcp__cuttlefish__echo", "mcp__cuttlefish__fail"]
        );
    }

    #[test]
    fn relays_through_a_socket() {
        let dir =
            std::env::temp_dir().join(alloc::format!("cuttlefish-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mcp.sock");
        let listener = Listener::bind(&path).unwrap();
        let out = std::thread::scope(|scope| {
            let server = scope.spawn(|| listener.serve(&Echo));
            let mut out = Vec::new();
            let input = std::io::Cursor::new(session_lines().into_bytes());
            relay_with(&path, input, &mut out).unwrap();
            listener.stop();
            assert!(server.join().unwrap(), "a client connected");
            out
        });
        check_answers(&String::from_utf8(out).unwrap());
        // Stopped without a client: the accept wakes up and says so
        let quiet = Listener::bind(&dir.join("quiet.sock")).unwrap();
        let connected = std::thread::scope(|scope| {
            let server = scope.spawn(|| quiet.serve(&Echo));
            std::thread::sleep(core::time::Duration::from_millis(50));
            quiet.stop();
            server.join().unwrap()
        });
        assert!(!connected);
        drop(listener);
        drop(quiet);
        assert!(!path.exists(), "the socket file goes with the listener");
        std::fs::remove_dir_all(&dir).unwrap();
        // Nothing listening: a clear error
        let e = relay_with(&path, std::io::empty(), Vec::new()).unwrap_err();
        assert!(e.to_string().contains("connecting to the knowledge tools"));
    }
}
