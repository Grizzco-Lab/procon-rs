//! Anthropic Messages API over HTTPS.
//!
//! The API key is read from `ANTHROPIC_API_KEY` only, and never written or
//! logged. Requests are built by [`build_body`] and answers read by
//! [`parse_reply`], both pure so they are tested without the network; the
//! [`Transport`] trait is the only part that talks to the server.

use alloc::string::String;
use alloc::vec::Vec;
use anyhow::{Context, Result, bail};
use base64::Engine;
use core::time::Duration;
use serde_json::{Value, json};

/// Messages endpoint
pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
/// API version header
pub const API_VERSION: &str = "2023-06-01";
/// Default model
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Default effort (`low`, `medium`, `high`, `xhigh`, `max`); reviewing
/// gameplay is judgment work, so above the model's `medium` default
pub const DEFAULT_EFFORT: &str = "high";

/// A piece of the user message
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// Text
    Text(String),
    /// A JPEG image
    Jpeg(Vec<u8>),
}

/// One request: a system prompt (cached across requests) and one user turn
#[derive(Clone, Debug, PartialEq)]
pub struct Prompt {
    /// Stable instructions and reference material
    pub system: String,
    /// The user turn: knowledge, frames, the question
    pub user: Vec<Block>,
    /// JSON schema the answer must follow (structured output), if any
    pub schema: Option<Value>,
}

/// Model settings
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Model name
    pub model: String,
    /// Effort level
    pub effort: String,
    /// Output limit, thinking included
    pub max_tokens: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            model: String::from(DEFAULT_MODEL),
            effort: String::from(DEFAULT_EFFORT),
            max_tokens: 16000,
        }
    }
}

/// The request body for a prompt
pub fn build_body(settings: &Settings, prompt: &Prompt) -> Value {
    let content: Vec<Value> = prompt
        .user
        .iter()
        .map(|b| match b {
            Block::Text(t) => json!({"type": "text", "text": t}),
            Block::Jpeg(bytes) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                }
            }),
        })
        .collect();
    let mut output_config = json!({"effort": settings.effort});
    if let Some(schema) = &prompt.schema {
        output_config["format"] = json!({"type": "json_schema", "schema": schema});
    }
    json!({
        "model": settings.model,
        "max_tokens": settings.max_tokens,
        "system": [{
            "type": "text",
            "text": prompt.system,
            "cache_control": {"type": "ephemeral"},
        }],
        "messages": [{"role": "user", "content": content}],
        "output_config": output_config,
    })
}

/// Token counts of one call
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// Uncached input tokens
    pub input: u64,
    /// Output tokens, thinking included
    pub output: u64,
    /// Input tokens read from the prompt cache
    pub cache_read: u64,
    /// Input tokens written to the prompt cache
    pub cache_write: u64,
}

/// The model's answer
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// The text blocks, joined
    pub text: String,
    /// Why generation stopped
    pub stop_reason: String,
    /// Token counts
    pub usage: Usage,
}

/// Reads a Messages API response body (success or error)
pub fn parse_reply(status: u16, body: &str) -> Result<Reply> {
    let v: Value = serde_json::from_str(body)
        .with_context(|| alloc::format!("HTTP {status}: unreadable answer"))?;
    if v["type"] == "error" || !(200..300).contains(&status) {
        bail!(
            "Anthropic API HTTP {status}: {}: {}",
            v["error"]["type"].as_str().unwrap_or("error"),
            v["error"]["message"].as_str().unwrap_or(body)
        );
    }
    let stop_reason = String::from(v["stop_reason"].as_str().unwrap_or_default());
    if stop_reason == "refusal" {
        bail!(
            "the model declined ({})",
            v["stop_details"]["category"]
                .as_str()
                .unwrap_or("no category")
        );
    }
    if stop_reason == "max_tokens" {
        bail!("the answer was cut off at max_tokens; raise it");
    }
    let text = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    Ok(Reply {
        text,
        stop_reason,
        usage: Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        },
    })
}

/// Sends a request body; (HTTP status, response body)
pub trait Transport: Send + Sync {
    /// POSTs `body` to the Messages API with the given API key
    fn post(&self, api_key: &str, body: &Value) -> Result<(u16, String)>;
}

/// HTTPS through ureq
pub struct Https {
    agent: ureq::Agent,
}

impl Default for Https {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(600)))
            .http_status_as_error(false)
            .build()
            .into();
        Https { agent }
    }
}

impl Transport for Https {
    fn post(&self, api_key: &str, body: &Value) -> Result<(u16, String)> {
        let mut resp = self
            .agent
            .post(API_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .send_json(body)
            .context("calling the Anthropic API")?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string()?;
        Ok((status, text))
    }
}

/// A Messages API client
pub struct Client {
    transport: Box<dyn Transport>,
    api_key: String,
    /// Model settings
    pub settings: Settings,
}

impl core::fmt::Debug for Client {
    /// Leaves the key out
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client over HTTPS with the key from `ANTHROPIC_API_KEY`
    pub fn from_env(settings: Settings) -> Result<Self> {
        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        if key.trim().is_empty() {
            bail!("ANTHROPIC_API_KEY is not set; export it to use the model");
        }
        Ok(Self::with_transport(
            Box::new(Https::default()),
            key,
            settings,
        ))
    }

    /// A client over any transport (tests use a fake one)
    pub fn with_transport(
        transport: Box<dyn Transport>,
        api_key: String,
        settings: Settings,
    ) -> Self {
        Client {
            transport,
            api_key,
            settings,
        }
    }

    /// Sends a prompt, retrying twice on rate limits, overload and server
    /// errors
    pub fn send(&self, prompt: &Prompt) -> Result<Reply> {
        let body = build_body(&self.settings, prompt);
        let mut attempt = 0;
        loop {
            let (status, text) = self.transport.post(&self.api_key, &body)?;
            let retry = status == 429 || status >= 500;
            if retry && attempt < 2 {
                attempt += 1;
                log::warn!("Anthropic API HTTP {status}; retrying");
                std::thread::sleep(Duration::from_secs(2u64.pow(attempt)));
                continue;
            }
            let reply = parse_reply(status, &text)?;
            let u = reply.usage;
            log::info!(
                "{}: {} input + {} cached + {} cache write, {} output tokens",
                self.settings.model,
                u.input,
                u.cache_read,
                u.cache_write,
                u.output
            );
            return Ok(reply);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers with canned responses and keeps the bodies it was sent
    pub struct Fake {
        pub replies: Mutex<Vec<(u16, String)>>,
        pub sent: std::sync::Arc<Mutex<Vec<Value>>>,
    }

    impl Transport for Fake {
        fn post(&self, api_key: &str, body: &Value) -> Result<(u16, String)> {
            assert_eq!(api_key, "test-key");
            self.sent.lock().unwrap().push(body.clone());
            Ok(self.replies.lock().unwrap().remove(0))
        }
    }

    /// A successful response with one text block
    pub fn ok(text: &str) -> (u16, String) {
        let v = json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100}
        });
        (200, v.to_string())
    }

    #[test]
    fn builds_bodies() {
        let prompt = Prompt {
            system: String::from("sys"),
            user: alloc::vec![
                Block::Text(String::from("hi")),
                Block::Jpeg(alloc::vec![1, 2, 3])
            ],
            schema: Some(json!({"type": "object"})),
        };
        let body = build_body(&Settings::default(), &prompt);
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["text"], "hi");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "AQID");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert!(body.get("thinking").is_none());
        assert!(body.get("temperature").is_none());
    }

    #[test]
    fn parses_replies_and_errors() {
        let (s, b) = ok("answer");
        let r = parse_reply(s, &b).unwrap();
        assert_eq!(r.text, "answer");
        assert_eq!(r.usage.cache_read, 100);
        let err = parse_reply(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("authentication_error"));
        let refusal = r#"{"content":[],"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"}}"#;
        assert!(
            parse_reply(200, refusal)
                .unwrap_err()
                .to_string()
                .contains("cyber")
        );
    }

    #[test]
    fn retries_overload_then_succeeds() {
        let sent = std::sync::Arc::new(Mutex::new(Vec::new()));
        let fake = Fake {
            replies: Mutex::new(alloc::vec![(529, String::from("{}")), ok("fine")]),
            sent: sent.clone(),
        };
        let client = Client::with_transport(
            Box::new(fake),
            String::from("test-key"),
            Settings::default(),
        );
        assert_eq!(
            client
                .send(&Prompt {
                    system: String::new(),
                    user: alloc::vec![],
                    schema: None
                })
                .unwrap()
                .text,
            "fine"
        );
        assert_eq!(sent.lock().unwrap().len(), 2);
        assert!(!alloc::format!("{client:?}").contains("test-key"));
    }
}
