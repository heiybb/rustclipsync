mod clipboard;
mod cloudflare;
mod config;
mod file_transfer;
mod protocol;
mod security;
mod sync;

use crate::config::load_config;
use crate::sync::run_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    run_client(load_config()?).await?;

    Ok(())
}
