//! Stamps the build's git commit into every binary, and embeds the Windows
//! resource script into the tray GUI binary.
//!
//! The resource goes into that binary only: `compile_for` emits a per-binary
//! link argument, so the CLI executable stays free of the 64 KB icon it would
//! never show.
//!
//! `gpui` embeds a manifest resource of its own from its build script. The two
//! coexist because they carry different resource types (RT_MANIFEST versus
//! RT_GROUP_ICON/RT_ICON), so neither overwrites the other.

fn main() {
    git_version::emit();
    #[cfg(feature = "gui")]
    gui_resources::embed();
}

/// Sets `RUSTCLIPSYNC_GIT` to the short commit hash, so the startup log says
/// which build is running. The crate version never changes, and CI names each
/// release `build-<7-char hash>`, so the hash is what identifies a binary.
mod git_version {
    use std::path::Path;
    use std::process::Command;

    /// What the binary is built from. `-dirty` is judged against these paths
    /// only, so an edited README does not mark a build that is byte-for-byte
    /// the commit, and each one is also a rerun trigger so the mark cannot go
    /// stale.
    const BUILD_INPUTS: [&str; 5] = ["src", "assets", "build.rs", "Cargo.toml", "Cargo.lock"];

    pub fn emit() {
        // Any rerun-if-changed line replaces Cargo's default of rerunning on
        // every package file change, so the build inputs are listed too.
        for input in BUILD_INPUTS {
            println!("cargo:rerun-if-changed={input}");
        }
        // Rerun when HEAD moves: a checkout rewrites HEAD, a commit rewrites
        // the branch ref (or packed-refs). `--git-path` resolves these inside
        // a linked worktree as well. A missing path would force a rerun on
        // every build, so only existing ones are listed.
        let mut git_paths = vec!["HEAD".to_string(), "packed-refs".to_string()];
        if let Some(branch_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            git_paths.push(branch_ref);
        }
        for git_path in &git_paths {
            if let Some(path) = git(&["rev-parse", "--git-path", git_path]) {
                if Path::new(&path).exists() {
                    println!("cargo:rerun-if-changed={path}");
                }
            }
        }
        println!("cargo:rerun-if-env-changed=GITHUB_SHA");

        let commit = git(&["rev-parse", "--short=7", "HEAD"])
            .or_else(|| {
                std::env::var("GITHUB_SHA")
                    .ok()
                    .map(|sha| sha.chars().take(7).collect())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let mut status_args = vec!["status", "--porcelain", "--untracked-files=no", "--"];
        status_args.extend(BUILD_INPUTS);
        let dirty = git(&status_args).is_some_and(|changes| !changes.is_empty());
        let suffix = if dirty { "-dirty" } else { "" };
        println!("cargo:rustc-env=RUSTCLIPSYNC_GIT={commit}{suffix}");
    }

    /// Trimmed stdout of a successful git run; `None` if git is missing or the
    /// tree is not a repository (a source tarball, say).
    fn git(args: &[&str]) -> Option<String> {
        let output = Command::new("git")
            .args(args)
            // `git status` otherwise rewrites the index to refresh its stat
            // cache, which would race a concurrent git command for the lock.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
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
