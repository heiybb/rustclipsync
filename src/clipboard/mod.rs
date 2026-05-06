use anyhow::Result;
use std::path::PathBuf;

#[cfg(any(not(windows), test))]
#[cfg_attr(all(test, windows), allow(dead_code))]
mod arboard_backend;
#[cfg(not(windows))]
mod x11;

#[cfg(windows)]
mod windows;

pub enum ClipboardItem {
    Text(String),
    ImagePng(Vec<u8>),
    FilePath(PathBuf),
}

pub trait ClipboardBackend: Send {
    fn name(&self) -> &'static str;
    fn read_snapshot(&mut self) -> Result<Option<ClipboardItem>>;
    fn write_item(&mut self, item: ClipboardItem) -> Result<()>;
}

pub fn create_backend() -> Result<Box<dyn ClipboardBackend>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsBackend::new()?))
    }

    #[cfg(not(windows))]
    {
        let session = std::env::var("XDG_SESSION_TYPE").ok();
        match select_backend_name(session.as_deref(), false) {
            "x11" => Ok(Box::new(x11::X11Backend::new()?)),
            _ => Ok(Box::new(arboard_backend::ArboardBackend::new()?)),
        }
    }
}

#[cfg(any(not(windows), test))]
fn select_backend_name(session_type: Option<&str>, is_windows: bool) -> &'static str {
    if is_windows {
        return "windows";
    }
    match session_type {
        Some("x11") => "x11",
        _ => "arboard",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_x11_backend_for_x11_session() {
        let backend = select_backend_name(Some("x11"), false);
        assert_eq!(backend, "x11");
    }

    #[test]
    fn selects_windows_backend_on_windows() {
        let backend = select_backend_name(Some("x11"), true);
        assert_eq!(backend, "windows");
    }

    #[test]
    fn falls_back_to_arboard_for_unknown_linux_session() {
        let backend = select_backend_name(Some("unknown"), false);
        assert_eq!(backend, "arboard");
    }
}
