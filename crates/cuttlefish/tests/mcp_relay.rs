//! The Claude CLI's side of the knowledge tools, played by a fake `claude`:
//! it starts the MCP server its `--mcp-config` names (this crate's own
//! binary as `cuttlefish mcp --socket <path>`, the relay), shakes hands,
//! lists the tools and calls them through it, as the real CLI does, then
//! answers citing what they showed.

use anyhow::{Context, Result};
use cuttlefish::claude_cli::{Cli, Output, Runner};
use cuttlefish::doc::{Document, SourceKind};
use cuttlefish::embed::HashEmbedder;
use cuttlefish::llm::{Backend, Client, Settings};
use cuttlefish::review;
use cuttlefish::store::Store;
use cuttlefish::tools::{Library, Session};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, RwLock};

/// What the fake CLI saw: the tools listed and each call's answer
#[derive(Default)]
struct Seen {
    tools: Vec<String>,
    answers: Vec<Value>,
    allowed: String,
}

/// A fake `claude -p` that uses the knowledge tools through the relay its
/// MCP config names
struct FakeClaude {
    calls: Vec<(&'static str, Value)>,
    answer: &'static str,
    seen: Arc<Mutex<Seen>>,
}

/// The argument after a flag
fn after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let i = args.iter().position(|a| a == flag)?;
    args.get(i + 1).map(String::as_str)
}

impl Runner for FakeClaude {
    fn run(&self, command: &mut Command, _stdin: &str) -> Result<Output> {
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let config: Value =
            serde_json::from_str(after(&args, "--mcp-config").context("no config")?)?;
        let server = &config["mcpServers"]["cuttlefish"];
        assert_eq!(server["type"], "stdio");
        let server_args: Vec<&str> = server["args"]
            .as_array()
            .context("no args")?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        // The MCP server, started as the CLI starts it, in the run's folder
        let mut relay = Command::new(server["command"].as_str().context("no command")?)
            .args(&server_args)
            .current_dir(command.get_current_dir().context("no folder")?)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("starting the relay")?;
        let mut to_server = relay.stdin.take().context("no stdin")?;
        let mut from_server = BufReader::new(relay.stdout.take().context("no stdout")?);
        let mut id = 0;
        let mut ask = |method: &str, params: Value| -> Result<Value> {
            id += 1;
            let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            writeln!(to_server, "{message}")?;
            to_server.flush()?;
            let mut line = String::new();
            from_server.read_line(&mut line)?;
            Ok(serde_json::from_str(&line)?)
        };
        let init = ask(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "claude-code", "version": "2"}}),
        )?;
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        let mut seen = self.seen.lock().unwrap();
        seen.allowed = String::from(after(&args, "--allowedTools").unwrap_or_default());
        seen.tools = ask("tools/list", json!({}))?["result"]["tools"]
            .as_array()
            .context("no tools")?
            .iter()
            .filter_map(|t| t["name"].as_str().map(String::from))
            .collect();
        for (name, arguments) in &self.calls {
            let answer = ask("tools/call", json!({"name": name, "arguments": arguments}))?;
            seen.answers.push(answer);
        }
        drop(ask);
        // The relay ends with its input
        drop(to_server);
        let status = relay.wait()?;
        assert!(status.success(), "the relay failed: {status}");
        let result = json!({
            "type": "result", "subtype": "success", "is_error": false,
            "result": self.answer, "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        Ok(Output {
            success: true,
            stdout: format!("{result}\n"),
            stderr: String::new(),
        })
    }
}

#[test]
fn the_cli_calls_the_tools_through_the_relay() {
    let root = std::env::temp_dir().join(format!("cuttlefish-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let e = HashEmbedder { dim: 64 };
    let mut store = Store::open(&root, &e).unwrap();
    let mut card = Document::new(
        SourceKind::GameData,
        "leanny:enemy/SakelienBomber",
        String::from("Steelhead (Salmonid, game data)"),
        String::from("# Steelhead (Salmonid)\n\nThe bomb explodes 180 frames after the throw."),
    );
    card.url = Some(String::from("https://leanny.github.io/splat3/coop.html"));
    store.add(&card, &e).unwrap();
    let store = RwLock::new(store);
    let library = Library::new(&root, None);
    let seen = Arc::new(Mutex::new(Seen::default()));
    let fake = FakeClaude {
        calls: vec![
            ("names", json!({"text": "炸弹鱼"})),
            (
                "search",
                json!({"query": "Steelhead bomb explode", "kinds": ["game-data"]}),
            ),
            ("open", json!({"id": "S1", "around": 0})),
        ],
        answer: "It explodes 180 frames (3 s) after the throw [S1].",
        seen: Arc::clone(&seen),
    };
    let settings = Settings {
        backend: Backend::ClaudeCli,
        mcp_relay: Some(PathBuf::from(env!("CARGO_BIN_EXE_cuttlefish"))),
        ..Settings::default()
    };
    let client = Client::with_cli(
        Cli::with_runner(PathBuf::from("claude"), Box::new(fake)),
        settings,
    );
    let tools = Session::new(&store, &e, &library, 1);
    let answer = review::ask_with_tools(&tools, &client, "炸弹鱼的炸弹多久爆炸？").unwrap();
    assert_eq!(
        answer.text,
        "It explodes 180 frames (3 s) after the throw [S1]."
    );
    assert_eq!(answer.sources.len(), 1);
    assert_eq!(answer.sources[0].title, "Steelhead (Salmonid, game data)");
    // What it looked up, as the page lists it
    let tools_used: Vec<&str> = answer.lookups.iter().map(|l| l.tool.as_str()).collect();
    assert_eq!(tools_used, ["names", "search", "open"]);
    assert_eq!(answer.lookups[0].found[0].id, "steelhead");
    assert_eq!(answer.lookups[1].found[0].id, "S1");
    assert!(answer.lookups.iter().all(|l| l.error.is_none()));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.tools, ["search", "open", "pedia", "thread", "names"]);
    assert_eq!(
        seen.allowed,
        "mcp__cuttlefish__search,mcp__cuttlefish__open,mcp__cuttlefish__pedia,\
         mcp__cuttlefish__thread,mcp__cuttlefish__names"
    );
    let opened = seen.answers[2]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        opened.starts_with("[S1] game-data · Steelhead (Salmonid, game data)"),
        "{opened}"
    );
    assert!(opened.contains("The bomb explodes 180 frames after the throw."));
    std::fs::remove_dir_all(&root).unwrap();
}
