//! What the proxy's `proxy.toml` and the lab's `config.toml` have in
//! common: both are TOML files with a `[logging]` section

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Logging configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level: error, warn, info, debug, trace
    pub level: String,
}

impl LoggingConfig {
    /// Check the log level is one env_logger knows
    pub fn validate(&self) -> Result<()> {
        match self.level.to_lowercase().as_str() {
            "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
            _ => anyhow::bail!("Invalid log level: {}", self.level),
        }
    }
}

/// Load any configuration struct from a TOML file
pub fn load<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<T> {
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read config file: {}", path.as_ref().display()))?;

    toml::from_str(&contents)
        .with_context(|| format!("Failed to parse config file: {}", path.as_ref().display()))
}
