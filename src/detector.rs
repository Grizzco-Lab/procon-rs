//! The Salmon Run detector: AgentZero's `agentzero-detect-serve`, a small
//! local HTTP service with our own trained network, used by the Vision app
//! next to the COCO models it runs itself
//!
//! The studio talks to it as Follow talks to the tracker (see
//! [`crate::follow`]): `GET /health` says whether it answers, which
//! checkpoint it serves and how that was trained; `POST /detect` streams one
//! JSON line per frame ([`Message`]), and closing the connection stops it.
//! The service runs one request at a time and answers another with 409.
//! The page can start it with `[vision] detector_command` in
//! `detector_dir`; it then ends with the studio.
//!
//! A run on the GPU is refused while a session is being recorded (a
//! `session.json` without `stopped_at_unix_ms` under the sessions' root):
//! the GPU is the recording's then.

use crate::config::VisionConfig;
use crate::follow::{agent, port_of};
use anyhow::{Context, Result, bail, ensure};
use core::time::Duration;
use gameplay_vision::labels::{ObjectBox, Source};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

/// Where the detector listens unless `[vision] detector` says otherwise
pub const DEFAULT_DETECTOR: &str = "http://127.0.0.1:7341";

/// The Vision app's name for this model in a run request
pub const MODEL: &str = "salmon";

/// How the Vision app calls it
pub const MODEL_NAME: &str = "Salmon Run detector · AgentZero";

/// The detector's settings, from `[vision]`
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// The service's base URL
    pub url: String,
    /// Command that starts it
    pub command: Vec<String>,
    /// Folder the command runs in, where relative checkpoints are
    pub dir: PathBuf,
}

impl Settings {
    /// Settings from `[vision]`; relative paths start at `config_dir`
    pub fn from_config(config: &VisionConfig, config_dir: &Path) -> Result<Self> {
        let url = config
            .detector
            .as_deref()
            .unwrap_or(DEFAULT_DETECTOR)
            .trim_end_matches('/')
            .to_string();
        ensure!(
            url.starts_with("http://"),
            "[vision] detector must be an http:// URL, not {url:?}"
        );
        let command = match &config.detector_command {
            Some(command) => command.clone(),
            None => {
                let mut command = ["uv", "run", "agentzero-detect-serve"]
                    .map(String::from)
                    .to_vec();
                if let Some(port) = port_of(&url) {
                    command.extend(["--port".to_string(), port.to_string()]);
                }
                command
            }
        };
        Ok(Self {
            url,
            command,
            dir: config_dir.join(config.detector_dir.as_deref().unwrap_or("../AgentZero")),
        })
    }

    /// The command as the page shows it, to run by hand
    fn command_line(&self) -> String {
        format!("cd {} && {}", self.dir.display(), self.command.join(" "))
    }
}

/// Mean ms per frame of a chunk the service decoded and ran together
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct FrameMs {
    pub decode: f64,
    pub network: f64,
    pub total: f64,
}

/// A line of the detector's answer to `POST /detect`
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    /// The model is loaded; `reason` says why this device
    Start {
        device: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        load_ms: Option<f64>,
    },
    /// One frame's boxes, in the label format
    Frame {
        frame: u64,
        boxes: Vec<ObjectBox>,
        #[serde(default)]
        ms: FrameMs,
    },
    End {
        frames: u64,
        #[serde(default)]
        ms_per_frame: Option<f64>,
    },
    Error {
        error: String,
    },
}

/// A line of the answer, `None` for a blank one; boxes are always the
/// model's, whatever the line says
pub fn parse_line(line: &str) -> Result<Option<Message>> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let mut message: Message = serde_json::from_str(line)
        .with_context(|| format!("bad line from the detector: {line}"))?;
    if let Message::Frame { boxes, .. } = &mut message {
        for b in boxes {
            b.by = Source::Model;
        }
    }
    Ok(Some(message))
}

/// What `POST /detect` asks for
#[derive(Clone, Debug, PartialEq)]
pub struct Request<'a> {
    pub video: &'a Path,
    pub start: u64,
    pub count: u64,
    pub step: u64,
    pub min_score: f32,
    /// `auto` or `cpu`
    pub device: &'a str,
}

/// The session being recorded under `root`, if any: its `session.json`
/// has no `stopped_at_unix_ms` yet
pub fn recording_in_progress(root: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.into_iter().find(|name| {
        std::fs::read(root.join(name).join("session.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|info| info.get("stopped_at_unix_ms").is_none())
    })
}

/// The detector service, and the process the studio started for it
pub struct Service {
    settings: Settings,
    child: Mutex<Option<Child>>,
}

impl Service {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            child: Mutex::default(),
        }
    }

    /// Whether it answers, what it serves (with the time its checkpoint
    /// was saved, `saved_ms`) and how to start it
    pub fn status(&self) -> Value {
        let health = agent(Duration::from_millis(1500))
            .get(format!("{}/health", self.settings.url))
            .call()
            .context("no answer")
            .and_then(|mut response| {
                let text = response.body_mut().read_to_string()?;
                ensure!(response.status() == 200, "answered {}", response.status());
                Ok(serde_json::from_str::<Value>(&text)?)
            });
        let saved_ms = health
            .as_ref()
            .ok()
            .and_then(|h| h["model"].as_str())
            .and_then(|model| self.saved_ms(model));
        json!({
            "url": self.settings.url,
            "ok": health.is_ok(),
            "health": health.as_ref().ok(),
            "error": health.as_ref().err().map(|e| format!("{e:#}")),
            "saved_ms": saved_ms,
            "command": self.settings.command_line(),
            "can_start": !self.settings.command.is_empty() && self.settings.dir.is_dir(),
            "started": self.started(),
        })
    }

    /// When the checkpoint folder `model` (relative to the command's
    /// folder) was written, in Unix ms
    fn saved_ms(&self, model: &str) -> Option<u64> {
        let dir = self.settings.dir.join(model);
        let file = [dir.join("model.safetensors"), dir]
            .into_iter()
            .find(|p| p.exists())?;
        let modified = std::fs::metadata(file).ok()?.modified().ok()?;
        Some(
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_millis() as u64,
        )
    }

    /// What became of the process the studio started, if it did
    fn started(&self) -> Option<String> {
        let mut child = self.child.lock().unwrap();
        let child = child.as_mut()?;
        Some(match child.try_wait() {
            Ok(None) => "running".to_string(),
            Ok(Some(status)) => format!("exited ({status})"),
            Err(e) => format!("unknown ({e})"),
        })
    }

    /// Start the service with the configured command, unless the one
    /// started before still runs
    pub fn start(&self) -> Result<Value> {
        let mut current = self.child.lock().unwrap();
        if let Some(child) = current.as_mut()
            && child.try_wait()?.is_none()
        {
            return Ok(json!({ "started": "running" }));
        }
        let (program, args) = self
            .settings
            .command
            .split_first()
            .context("no [vision] detector_command")?;
        ensure!(
            self.settings.dir.is_dir(),
            "no folder {}",
            self.settings.dir.display()
        );
        // A group of its own, so the Python behind `uv run` stops with it
        let child = Command::new(program)
            .args(args)
            .current_dir(&self.settings.dir)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("cannot run {}", self.settings.command_line()))?;
        log::info!(
            "Started the detector (pid {}): {}",
            child.id(),
            self.settings.command_line()
        );
        *current = Some(child);
        Ok(json!({ "started": "running" }))
    }

    /// Stop the service the studio started, if it did, with its group
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            if let Ok(None) = child.try_wait() {
                log::info!("Stopping the detector (pid {})", child.id());
                // SAFETY: kill(2) with a negative pid signals the group the
                // child leads; it touches no memory
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGTERM) };
            }
            let _ = child.wait();
        }
    }

    /// Detect on a stretch of a video, handing each line to `on_message`
    /// as it comes; stops (closing the connection, which stops the
    /// service) when `on_message` answers `false`
    pub fn detect(
        &self,
        request: &Request,
        mut on_message: impl FnMut(Message) -> Result<bool>,
    ) -> Result<()> {
        let body = json!({
            "video": request.video,
            "start": request.start,
            "count": request.count,
            "step": request.step,
            "min_score": request.min_score,
            "device": request.device,
        });
        // The first request may load the model
        let response = agent(Duration::from_secs(300))
            .post(format!("{}/detect", self.settings.url))
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|e| {
                anyhow::anyhow!(
                    "the Salmon Run detector is not running at {} ({e}); start it with `{}`",
                    self.settings.url,
                    self.settings.command_line()
                )
            })?;
        let status = response.status();
        let mut reader = std::io::BufReader::new(response.into_body().into_reader());
        if status != 200 {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut reader, &mut text)?;
            let error = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or(text);
            if status == 409 {
                bail!("the detector is busy with another request: {error}");
            }
            bail!("the detector answered {status}: {error}");
        }
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let Some(message) = parse_line(&line)? else {
                continue;
            };
            if let Message::Error { error } = message {
                bail!("the detector failed: {error}");
            }
            if !on_message(message)? {
                // Dropping the answer closes the connection
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_from_config() {
        let dir = Path::new("/studio");
        let settings = Settings::from_config(&VisionConfig::default(), dir).unwrap();
        assert_eq!(settings.url, DEFAULT_DETECTOR);
        assert_eq!(
            settings.command,
            ["uv", "run", "agentzero-detect-serve", "--port", "7341"]
        );
        assert_eq!(settings.dir, Path::new("/studio/../AgentZero"));

        let config = VisionConfig {
            detector: Some("http://gpu-box:7400/".to_string()),
            detector_command: Some(vec!["./serve".to_string(), "--x".to_string()]),
            detector_dir: Some("/opt/az".to_string()),
            ..VisionConfig::default()
        };
        let settings = Settings::from_config(&config, dir).unwrap();
        assert_eq!(settings.url, "http://gpu-box:7400");
        assert_eq!(settings.command_line(), "cd /opt/az && ./serve --x");

        let https = VisionConfig {
            detector: Some("https://x".to_string()),
            ..VisionConfig::default()
        };
        assert!(Settings::from_config(&https, dir).is_err());
    }

    #[test]
    fn config_section() {
        let config: VisionConfig = toml::from_str(
            r#"detector = "http://127.0.0.1:7341"
detector_command = ["uv", "run", "agentzero-detect-serve"]
detector_dir = "../AgentZero""#,
        )
        .unwrap();
        assert_eq!(config.detector.as_deref(), Some(DEFAULT_DETECTOR));
        assert_eq!(config.detector_command.unwrap().len(), 3);
        assert_eq!(config.detector_dir.as_deref(), Some("../AgentZero"));
    }

    #[test]
    fn detector_lines() {
        let start = r#"{"type": "start", "device": "cuda", "reason": "9.1 GiB free", "classes": ["chum"], "load_ms": 812.4}"#;
        assert_eq!(
            parse_line(start).unwrap(),
            Some(Message::Start {
                device: "cuda".to_string(),
                reason: "9.1 GiB free".to_string(),
                load_ms: Some(812.4),
            })
        );
        // A box without `by` is still the model's
        let frame = r#"{"type": "frame", "frame": 1155, "boxes": [{"class": "chum", "x": 0.1, "y": 0.2, "w": 0.05, "h": 0.1, "score": 0.71}, {"class": "player", "x": 0.4, "y": 0.5, "w": 0.1, "h": 0.2, "by": "model", "score": 0.4}], "ms": {"decode": 3.2, "network": 11.8, "total": 15.0}}"#;
        let Some(Message::Frame { frame, boxes, ms }) = parse_line(frame).unwrap() else {
            panic!("not a frame");
        };
        assert_eq!(frame, 1155);
        assert_eq!(boxes.len(), 2);
        assert!(boxes.iter().all(|b| b.by == Source::Model));
        assert_eq!(
            (boxes[0].class.as_str(), boxes[0].score),
            ("chum", Some(0.71))
        );
        assert_eq!(ms.total, 15.0);
        assert_eq!(
            parse_line(r#"{"type": "end", "frames": 3, "ms_per_frame": 20.5}"#).unwrap(),
            Some(Message::End {
                frames: 3,
                ms_per_frame: Some(20.5)
            })
        );
        assert!(matches!(
            parse_line(r#"{"type": "error", "error": "CUDA out of memory"}"#).unwrap(),
            Some(Message::Error { .. })
        ));
        assert_eq!(parse_line("  \n").unwrap(), None);
        assert!(parse_line(r#"{"type": "frame"}"#).is_err());
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn recording_is_found() {
        let root = std::env::temp_dir().join(format!("procon-detector-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let session = |name: &str, text: &str| {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("session.json"), text).unwrap();
        };
        assert_eq!(recording_in_progress(&root), None);
        session("2026-09-25_11-26-22", r#"{"stopped_at_unix_ms": 1}"#);
        assert_eq!(recording_in_progress(&root), None);
        session("2026-09-27_10-00-00", r#"{"started_at_unix_ms": 1}"#);
        assert_eq!(
            recording_in_progress(&root).as_deref(),
            Some("2026-09-27_10-00-00")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
