use rustclipsync::config::load_config;
use rustclipsync::logging::{self, ConsoleTarget};
use rustclipsync::sync::run_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // No in-memory buffer: on the console there is nothing to scroll back
    // through it, and retaining records would just cost memory.
    logging::init(ConsoleTarget::Stderr, None)?;

    run_client(load_config()?).await?;

    Ok(())
}
