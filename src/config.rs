use anyhow::{Result, anyhow, bail};
use std::env;

#[derive(Debug, Clone)]
pub enum AppConfig {
    Server(ServerConfig),
    Client(ClientConfig),
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind_addr: String,
    pub auth_token: String,
    pub max_payload_bytes: usize,
    pub max_queue_messages: usize,
    pub max_queue_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub server_url: String,
    pub client_id: String,
    pub client_name: String,
    pub auth_token: String,
    pub poll_interval_ms: u64,
    pub remote_poll_interval_ms: u64,
    pub receive_dir: String,
    pub max_payload_bytes: usize,
}

const DEFAULT_BIND_ADDR: &str = "0.0.0.0:7878";
const DEFAULT_POLL_INTERVAL_MS: u64 = 300;
const DEFAULT_REMOTE_POLL_INTERVAL_MS: u64 = 500;
const DEFAULT_RECEIVE_DIR: &str = "receive";
const DEFAULT_MAX_PAYLOAD_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_QUEUE_MESSAGES: usize = 100;
const DEFAULT_MAX_QUEUE_BYTES: usize = 1024 * 1024 * 1024;

pub fn parse_config_from_env() -> Result<AppConfig> {
    parse_config(env::args().skip(1))
}

fn parse_config<I>(args: I) -> Result<AppConfig>
where
    I: IntoIterator<Item = String>,
{
    let mut args: Vec<String> = args.into_iter().collect();
    if args.is_empty() {
        bail!("{}", usage());
    }

    let mode = args.remove(0);
    match mode.as_str() {
        "server" => parse_server(args),
        "client" => parse_client(args),
        "-h" | "--help" | "help" => bail!("{}", usage()),
        other => bail!("unknown mode '{other}'\n\n{}", usage()),
    }
}

fn parse_server(args: Vec<String>) -> Result<AppConfig> {
    let auth_token =
        required_option(&args, "--auth-token").or_else(|_| required_option(&args, "--token"))?;
    let bind_addr = optional_option(&args, "--bind-addr")
        .or_else(|| optional_option(&args, "--bind"))
        .unwrap_or_else(|| DEFAULT_BIND_ADDR.to_string());
    let max_queue_messages = optional_option(&args, "--max-queue-messages")
        .map(|value| parse_positive_usize(&value, "--max-queue-messages"))
        .transpose()?
        .unwrap_or(DEFAULT_MAX_QUEUE_MESSAGES);
    let max_queue_bytes = optional_option(&args, "--max-queue-bytes")
        .map(|value| parse_size_bytes(&value, "--max-queue-bytes"))
        .transpose()?
        .unwrap_or(DEFAULT_MAX_QUEUE_BYTES);
    reject_unknown_options(
        &args,
        &[
            "--auth-token",
            "--token",
            "--bind-addr",
            "--bind",
            "--max-queue-messages",
            "--max-queue-bytes",
        ],
    )?;

    Ok(AppConfig::Server(ServerConfig {
        bind_addr,
        auth_token,
        max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
        max_queue_messages,
        max_queue_bytes,
    }))
}

fn parse_client(args: Vec<String>) -> Result<AppConfig> {
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

    Ok(AppConfig::Client(ClientConfig {
        server_url,
        client_id,
        client_name,
        auth_token,
        poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        remote_poll_interval_ms: DEFAULT_REMOTE_POLL_INTERVAL_MS,
        receive_dir: DEFAULT_RECEIVE_DIR.to_string(),
        max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
    }))
}

fn required_option(args: &[String], name: &str) -> Result<String> {
    optional_option(args, name).ok_or_else(|| anyhow!("missing required argument {name}"))
}

fn optional_option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn parse_positive_usize(value: &str, name: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid value for {name}: {value}"))?;
    if parsed == 0 {
        bail!("{name} must be greater than 0");
    }
    Ok(parsed)
}

fn parse_size_bytes(value: &str, name: &str) -> Result<usize> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("invalid value for {name}: {value}");
    }

    let upper = trimmed.to_ascii_uppercase();
    let (number, multiplier) = if let Some(number) = upper.strip_suffix("KB") {
        (number, 1024usize)
    } else if let Some(number) = upper.strip_suffix('K') {
        (number, 1024usize)
    } else if let Some(number) = upper.strip_suffix("MB") {
        (number, 1024usize * 1024)
    } else if let Some(number) = upper.strip_suffix('M') {
        (number, 1024usize * 1024)
    } else if let Some(number) = upper.strip_suffix("GB") {
        (number, 1024usize * 1024 * 1024)
    } else if let Some(number) = upper.strip_suffix('G') {
        (number, 1024usize * 1024 * 1024)
    } else {
        (upper.as_str(), 1usize)
    };

    let parsed = number
        .parse::<usize>()
        .map_err(|_| anyhow!("invalid value for {name}: {value}"))?;
    let bytes = parsed
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("value for {name} is too large: {value}"))?;
    if bytes == 0 {
        bail!("{name} must be greater than 0");
    }
    Ok(bytes)
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
    "Usage:\n  rustclipsync server --auth-token TOKEN [--bind-addr 0.0.0.0:7878] [--max-queue-messages 100] [--max-queue-bytes 1G]\n  rustclipsync client --server-url http://HOST:7878 --auth-token TOKEN [--client-id ID] [--client-name NAME]"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_defaults() {
        let config = parse_config([
            "server".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
        ])
        .unwrap();

        match config {
            AppConfig::Server(config) => {
                assert_eq!(config.bind_addr, DEFAULT_BIND_ADDR);
                assert_eq!(config.auth_token, "secret");
                assert_eq!(config.max_payload_bytes, DEFAULT_MAX_PAYLOAD_BYTES);
                assert_eq!(config.max_queue_messages, DEFAULT_MAX_QUEUE_MESSAGES);
                assert_eq!(config.max_queue_bytes, DEFAULT_MAX_QUEUE_BYTES);
            }
            AppConfig::Client(_) => panic!("expected server config"),
        }
    }

    #[test]
    fn parses_server_queue_limits() {
        let config = parse_config([
            "server".to_string(),
            "--auth-token".to_string(),
            "secret".to_string(),
            "--max-queue-messages".to_string(),
            "25".to_string(),
            "--max-queue-bytes".to_string(),
            "64M".to_string(),
        ])
        .unwrap();

        match config {
            AppConfig::Server(config) => {
                assert_eq!(config.max_queue_messages, 25);
                assert_eq!(config.max_queue_bytes, 64 * 1024 * 1024);
            }
            AppConfig::Client(_) => panic!("expected server config"),
        }
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

        match config {
            AppConfig::Client(config) => {
                assert_eq!(config.server_url, "http://127.0.0.1:7878");
                assert_eq!(config.client_id, "ubuntu-work");
                assert_eq!(config.client_name, "ubuntu-work");
                assert_eq!(config.auth_token, "secret");
                assert_eq!(config.remote_poll_interval_ms, 500);
            }
            AppConfig::Server(_) => panic!("expected client config"),
        }
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
