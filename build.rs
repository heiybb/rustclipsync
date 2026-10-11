//! Embeds the Windows resource script into the tray GUI binary.
//!
//! Only that binary: `compile_for` emits a per-binary link argument, so the CLI
//! executable stays free of the 64 KB icon it would never show.
//!
//! `gpui` embeds a manifest resource of its own from its build script. The two
//! coexist because they carry different resource types (RT_MANIFEST versus
//! RT_GROUP_ICON/RT_ICON), so neither overwrites the other.

fn main() {
    #[cfg(feature = "gui")]
    gui_resources::embed();
}

#[cfg(feature = "gui")]
mod gui_resources {
    /// Cargo names the binary; the resource has to be attached to that exact
    /// name or it is silently linked into nothing.
    const GUI_BIN: &str = "rustclipsync-gui";

    const RC_FILE: &str = "assets/rustclipsync.rc";
    const ICO_FILE: &str = "assets/rustclipsync.ico";

    pub fn embed() {
        println!("cargo:rerun-if-changed={RC_FILE}");
        println!("cargo:rerun-if-changed={ICO_FILE}");

        // Build scripts are compiled for the host, so `cfg!(windows)` would
        // describe the wrong machine when cross-compiling. The target OS is
        // only available as an environment variable.
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
            return;
        }

        // `manifest_optional`: this resource is an icon, not a manifest, so a
        // missing resource compiler should not fail an otherwise fine build.
        // A build without an icon is cosmetic damage; the tray falls back to
        // reporting the failure rather than starting up invisible.
        if let Err(err) = embed_resource::compile_for(RC_FILE, [GUI_BIN], embed_resource::NONE)
            .manifest_optional()
        {
            println!("cargo:warning=failed to embed {RC_FILE}: {err}");
        }
    }
}
