mod clipboard;
mod config;
mod file_transfer;
mod network;
mod protocol;
mod security;
mod server;
mod sync;

use crate::config::{AppConfig, parse_config_from_env};
use crate::server::run_server;
use crate::sync::run_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    match parse_config_from_env()? {
        AppConfig::Server(config) => run_server(config).await?,
        AppConfig::Client(config) => run_client(config).await?,
    }

    Ok(())
}
