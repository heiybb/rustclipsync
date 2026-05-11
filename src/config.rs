use crate::sync::R2_PAYLOAD_LIMIT_BYTES;
use anyhow::{Result, anyhow, bail};
use std::env;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub client_id: String,
    pub client_name: String,
    pub auth_token: String,
    pub poll_interval_ms: u64,
    pub receive_dir: String,
    pub max_payload_bytes: usize,
}

const DEFAULT_POLL_INTERVAL_MS: u64 = 300;
const DEFAULT_RECEIVE_DIR: &str = "receive";

pub fn parse_config_from_env() -> Result<ClientConfig> {
    parse_config(env::args().skip(1))
}

fn parse_config<I>(args: I) -> Result<ClientConfig>
where
    I: IntoIterator<Item = String>,
{
    let mut args: Vec<String> = args.into_iter().collect();
    if args.is_empty() {
        bail!("{}", usage());
    }

    if matches!(
        args.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) {
        bail!("{}", usage());
    }

    if args.first().map(String::as_str) == Some("client") {
        args.remove(0);
    }
    if args.first().map(String::as_str) == Some("server") {
        bail!("server mode has been removed; deploy the Cloudflare Worker relay instead");
    }

    let server_url = required_option(&args, "--server-url")?;
    let auth_token =
        required_option(&args, "--auth-token").or_else(|_| required_option(&args, "--token"))?;
    let client_id = optional_option(&args, "--client-id").unwrap_or_else(default_client_id);
    let client_name = optional_option(&args, "--client-name").unwrap_or_else(|| client_id.clone());
    reject_unknown_options(
        &args,
        &[
            "--server-url",
            "--auth-token",
            "--token",
            "--client-id",
            "--client-name",
        ],
    )?;

    Ok(ClientConfig {
        server_url,
        client_id,
        client_name,
        auth_token,
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        receive_dir: DEFAULT_RECEIVE_DIR.to_string(),
        max_payload_bytes: R2_PAYLOAD_LIMIT_BYTES,
    })
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    optional_option(args, name).ok_or_else(|| anyhow!("missing required argument {name}"))
}

fn optional_option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn reject_unknown_options(args: &[String], allowed: &[&str]) -> Result<()> {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if !arg.starts_with("--") {
            bail!("unexpected positional argument '{arg}'");
        }
        if !allowed.contains(&arg.as_str()) {
            bail!("unknown argument '{arg}'");
        }
        if index + 1 >= args.len() || args[index + 1].starts_with("--") {
            bail!("missing value for argument '{arg}'");
        }
        index += 2;
    }
    Ok(())
}

fn default_client_id() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .unwrap_or_else(|_| "rustclipsync-client".to_string())
}

fn usage() -> &'static str {
    "Usage:\n  rustclipsync --server-url https://WORKER_URL --auth-token TOKEN [--client-id ID] [--client-name NAME]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_client_required_args_without_mode() {
        let config = parse_config([
            "--server-url".to_string(),
            "https://clipsync.example.com".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
            "--client-id".to_string(),
            "RYZEN".to_string(),
        ])
        .unwrap();

        assert_eq!(config.server_url, "https://clipsync.example.com");
        assert_eq!(config.client_id, "RYZEN");
        assert_eq!(config.auth_token, "secret");
        assert_eq!(config.max_payload_bytes, 100 * 1024 * 1024);
    }

    #[test]
    fn rejects_server_mode() {
        let result = parse_config([
            "server".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn parses_client_required_args() {
        let config = parse_config([
            "client".to_string(),
            "--server-url".to_string(),
            "http://127.0.0.1:7878".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
            "--client-id".to_string(),
            "ubuntu-work".to_string(),
        ])
        .unwrap();

        assert_eq!(config.server_url, "http://127.0.0.1:7878");
        assert_eq!(config.client_id, "ubuntu-work");
        assert_eq!(config.client_name, "ubuntu-work");
        assert_eq!(config.auth_token, "secret");
    }

    #[test]
    fn rejects_missing_client_server_url() {
        let result = parse_config([
            "client".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
        ]);
        assert!(result.is_err());
    }
}
