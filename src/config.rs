use crate::sync::R2_PAYLOAD_LIMIT_BYTES;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

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

/// Places `config.toml` is looked for, in order:
///
/// 1. `$RUSTCLIPSYNC_CONFIG` (a full file path, not a directory).
/// 2. `./config.toml` — how the CLI has always worked.
/// 3. `<exe dir>/config.toml` — the GUI is usually launched from a shortcut or
///    from the startup folder, where the working directory is not the install
///    directory and step 2 finds nothing.
pub fn config_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(from_env) = std::env::var_os("RUSTCLIPSYNC_CONFIG") {
        candidates.push(PathBuf::from(from_env));
    }

    candidates.push(PathBuf::from("config.toml"));

    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        candidates.push(exe_dir.join("config.toml"));
    }

    candidates
}

pub fn load_config() -> Result<ClientConfig> {
    let candidates = config_candidates();
    let Some(config_path) = candidates.iter().find(|path| path.is_file()) else {
        // Write the template where the CLI has always written it, then report
        // every path that was tried so a GUI launch is diagnosable.
        let template_path = Path::new("config.toml");
        let created = create_default_config(template_path);
        let searched = candidates
            .iter()
            .map(|path| format!("  - {}", path.display()))
            .collect::<Vec<_>>()
            .join("\n");

        return Err(match created {
            Ok(()) => anyhow!(
                "config.toml not found. Searched:\n{searched}\nA template was created at {}. Fill it in and restart.",
                template_path.display()
            ),
            Err(err) => anyhow!("config.toml not found. Searched:\n{searched}\n({err:#})"),
        });
    };

    let content = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let config: Config = toml::from_str(&content)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

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
