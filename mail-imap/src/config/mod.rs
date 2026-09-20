use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub ssl: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: "localhost".to_string(),
            port: 993,
            username: "".to_string(),
            password: "".to_string(),
            ssl: true,
        }
    }
}

pub fn load_config(path: Option<&str>) -> Result<Config> {
    let config_path = path.unwrap_or("/etc/mail-imap.conf");
    
    // Try to read and parse as JSON first
    if let Ok(content) = fs::read_to_string(config_path) {
        // Try to deserialize as JSON
        if let Ok(config) = serde_json::from_str::<Config>(&content) {
            return Ok(config);
        }
    }
    
    // Return default config if nothing else works
    Ok(Config::default())
}