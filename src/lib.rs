/// Crate version and the commit it was built from, e.g. `0.1.0 (c1ac099)`.
/// `-dirty` after the hash means the sources had uncommitted changes.
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("RUSTCLIPSYNC_GIT"),
    ")"
);

pub mod clipboard;
pub mod cloudflare;
pub mod config;
pub mod logging;
pub mod protocol;
pub mod security;
pub mod status;
pub mod sync;

#[cfg(all(feature = "gui", windows))]
pub mod gui;
