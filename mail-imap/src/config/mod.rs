use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub server: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Use implicit TLS (server port is a TLS port, e.g. 993).
    #[serde(default = "default_ssl")]
    pub ssl: bool,
    /// Use STARTTLS on a plain port (e.g. 143) when ssl is false.
    #[serde(default)]
    pub starttls: bool,
    /// Accept invalid TLS certificates (for self-signed local servers).
    #[serde(default)]
    pub insecure: bool,
    /// Default folder for commands that need one.
    #[serde(default = "default_folder")]
    pub folder: String,
    /// Maximum number of search results to fetch (0 = no limit).
    #[serde(default = "default_max")]
    pub max: usize,
    /// Sort spec for search/unread (`-S`/`--sort` overrides), e.g.
    /// "-date" or "subject,-size". None = default (most recent first).
    #[serde(default)]
    pub sort: Option<String>,
    /// Use the in-memory mock backend instead of a real IMAP server.
    /// Intended for testing/demos; the released binary talks to a real server.
    #[serde(default)]
    pub mock: bool,
}

fn default_port() -> u16 {
    993
}

fn default_ssl() -> bool {
    true
}

fn default_folder() -> String {
    "INBOX".to_string()
}

fn default_max() -> usize {
    50
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: "localhost".to_string(),
            port: 993,
            username: "".to_string(),
            password: "".to_string(),
            ssl: true,
            starttls: false,
            insecure: false,
            folder: "INBOX".to_string(),
            max: default_max(),
            sort: None,
            mock: false,
        }
    }
}

pub fn load_config(path: Option<&str>) -> Result<Config> {
    let path = match path {
        Some(p) => p.to_string(),
        None => match std::env::var("MAIL_IMAP_CONFIG") {
            Ok(p) => p,
            Err(_) => "/etc/mail-imap.conf".to_string(),
        },
    };

    if !Path::new(&path).exists() {
        anyhow::bail!(
            "config file not found: {} (pass one with --config or set MAIL_IMAP_CONFIG)",
            path
        );
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("could not read config file: {}", path))?;
    serde_json::from_str::<Config>(&content)
        .with_context(|| format!("could not parse config file as JSON: {}", path))
}
