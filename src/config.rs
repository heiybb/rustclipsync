use crate::sync::R2_PAYLOAD_LIMIT_BYTES;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub auth_token: String,
    pub client_name: Option<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
}

fn default_poll_interval() -> u64 {
    300
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub client_id: String,
    pub client_name: String,
    pub auth_token: String,
    pub poll_interval_ms: u64,
    pub max_payload_bytes: usize,
}

pub fn load_config() -> Result<ClientConfig> {
    let config_path = Path::new("config.toml");

    if !config_path.exists() {
        create_default_config(config_path)?;
        return Err(anyhow!(
            "Config file 'config.toml' not found. A template has been created. Please fill it in and restart."
        ));
    }

    let content = fs::read_to_string(config_path).with_context(|| "Failed to read config.toml")?;

    let config: Config = toml::from_str(&content)
        .with_context(|| "Failed to parse config.toml. Please check the format.")?;

    let client_id = default_client_id();
    let client_name = config.client_name.unwrap_or_else(|| client_id.clone());

    Ok(ClientConfig {
        server_url: config.server_url,
        client_id,
        client_name,
        auth_token: config.auth_token,
        poll_interval_ms: config.poll_interval_ms,
        max_payload_bytes: R2_PAYLOAD_LIMIT_BYTES,
    })
}

fn create_default_config(path: &Path) -> Result<()> {
    let example = r#"# rustclipsync configuration
server_url = "https://your-relay.workers.dev"
auth_token = "your-secret-token"

# Optional: Custom name for this device (defaults to hostname)
# client_name = "my-desktop"

# Optional: How often to poll local clipboard in ms
# poll_interval_ms = 300
"#;
    fs::write(path, example).with_context(|| "Failed to create default config.toml")
}

fn default_client_id() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rustclipsync-client".to_string())
}
