// Release builds have no console: this is a background program driven from the
// notification area, and a console window flashing up on login would be noise.
// Debug builds keep theirs so panics and stderr logging remain visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    rustclipsync::gui::run()
}

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!(
        "the GUI front end is Windows-only for now; use the `rustclipsync` binary on this platform"
    )
}
