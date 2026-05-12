use anyhow::Result;
use image::{ImageBuffer, ImageFormat, RgbaImage};
use std::io::Cursor;
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

pub trait ClipboardWatcher: Send {
    fn wait_for_change(&mut self) -> Result<()>;
}

pub fn create_backend() -> Result<(Box<dyn ClipboardBackend>, Box<dyn ClipboardWatcher>)> {
    #[cfg(windows)]
    {
        let (backend, watcher) = windows::WindowsBackend::new_pair()?;
        Ok((Box::new(backend), Box::new(watcher)))
    }

    #[cfg(not(windows))]
    {
        let session = std::env::var("XDG_SESSION_TYPE").ok();
        let (backend, watcher) = match select_backend_name(session.as_deref(), false) {
            "x11" => {
                // For now, fallback to a polling watcher for Linux if not implemented
                (
                    Box::new(x11::X11Backend::new()?) as Box<dyn ClipboardBackend>,
                    Box::new(PollingWatcher::new()) as Box<dyn ClipboardWatcher>,
                )
            }
            _ => (
                Box::new(arboard_backend::ArboardBackend::new()?) as Box<dyn ClipboardBackend>,
                Box::new(PollingWatcher::new()) as Box<dyn ClipboardWatcher>,
            ),
        };
        Ok((backend, watcher))
    }
}

#[cfg(not(windows))]
pub struct PollingWatcher;
#[cfg(not(windows))]
impl PollingWatcher {
    pub fn new() -> Self {
        Self
    }
}
#[cfg(not(windows))]
impl ClipboardWatcher for PollingWatcher {
    fn wait_for_change(&mut self) -> Result<()> {
        std::thread::sleep(std::time::Duration::from_millis(500));
        Ok(())
    }
}

pub fn rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let image: RgbaImage = ImageBuffer::from_raw(width as u32, height as u32, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("invalid RGBA clipboard image dimensions"))?;
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

pub fn canonical_image_hash_from_png(bytes: &[u8]) -> Result<String> {
    let image = image::load_from_memory(bytes)?.to_rgba8();
    canonical_image_hash_from_rgba(
        image.width() as usize,
        image.height() as usize,
        image.as_raw(),
    )
}

pub fn canonical_image_hash_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let png = rgba_to_png(width, height, rgba)?;
    Ok(crate::security::calculate_bytes_hash(&png))
}

pub fn mark_current_text_seen(last_text: &mut String, text: Result<String, arboard::Error>) {
    if let Ok(text) = text {
        *last_text = text;
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
