use super::{ClipboardBackend, ClipboardItem};
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use clipboard_win::{Clipboard as WinClipboard, Getter, formats::FileList};
use image::{ImageBuffer, ImageFormat, RgbaImage};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::Cursor;
use std::path::PathBuf;

pub struct WindowsBackend {
    clipboard: Clipboard,
    last_text: String,
    last_files: Vec<PathBuf>,
    pending_files: VecDeque<PathBuf>,
    last_image_hash: String,
}

impl WindowsBackend {
    pub fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Clipboard::new()?,
            last_text: String::new(),
            last_files: Vec::new(),
            pending_files: VecDeque::new(),
            last_image_hash: String::new(),
        })
    }

    fn read_file_list() -> Result<Vec<PathBuf>> {
        let _clip = WinClipboard::new_attempts(10)
            .map_err(|e| anyhow::anyhow!("windows clipboard lock error: {:?}", e))?;
        let mut paths = Vec::<PathBuf>::new();
        FileList
            .read_clipboard(&mut paths)
            .map_err(|e| anyhow::anyhow!("windows file list read error: {:?}", e))?;
        Ok(paths)
    }
}

impl ClipboardBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn read_snapshot(&mut self) -> Result<Option<ClipboardItem>> {
        if let Some(path) = self.pending_files.pop_front() {
            return Ok(Some(ClipboardItem::FilePath(path)));
        }

        if let Ok(text) = self.clipboard.get_text()
            && text != self.last_text
        {
            self.last_text = text.clone();
            return Ok(Some(ClipboardItem::Text(text)));
        }

        if let Ok(paths) = Self::read_file_list()
            && !paths.is_empty()
            && paths != self.last_files
        {
            self.last_files = paths.clone();
            self.pending_files = paths.into_iter().collect();
            if let Some(path) = self.pending_files.pop_front() {
                return Ok(Some(ClipboardItem::FilePath(path)));
            }
        }

        if let Ok(image) = self.clipboard.get_image() {
            let png = rgba_to_png(image.width, image.height, image.bytes.as_ref())?;
            let hash =
                canonical_image_hash_from_rgba(image.width, image.height, image.bytes.as_ref())?;
            if hash != self.last_image_hash {
                self.last_image_hash = hash;
                return Ok(Some(ClipboardItem::ImagePng(png)));
            }
        }

        Ok(None)
    }

    fn write_item(&mut self, item: ClipboardItem) -> Result<()> {
        match item {
            ClipboardItem::Text(text) => {
                self.clipboard.set_text(text)?;
                Ok(())
            }
            ClipboardItem::ImagePng(bytes) => {
                let image = image::load_from_memory(&bytes)?.to_rgba8();
                let width = image.width() as usize;
                let height = image.height() as usize;
                let raw = image.into_raw();
                let hash = canonical_image_hash_from_rgba(width, height, &raw)?;
                self.clipboard.set_image(ImageData {
                    width,
                    height,
                    bytes: Cow::Owned(raw),
                })?;
                self.last_image_hash = hash;
                mark_current_text_seen(&mut self.last_text, self.clipboard.get_text());
                Ok(())
            }
            ClipboardItem::FilePath(_) => Ok(()),
        }
    }
}

fn rgba_to_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let image: RgbaImage = ImageBuffer::from_raw(width as u32, height as u32, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("invalid RGBA clipboard image dimensions"))?;
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png)?;
    Ok(bytes.into_inner())
}

#[cfg(test)]
fn canonical_image_hash_from_png(bytes: &[u8]) -> Result<String> {
    let image = image::load_from_memory(bytes)?.to_rgba8();
    canonical_image_hash_from_rgba(
        image.width() as usize,
        image.height() as usize,
        image.as_raw(),
    )
}

fn canonical_image_hash_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<String> {
    let png = rgba_to_png(width, height, rgba)?;
    Ok(crate::security::calculate_bytes_hash(&png))
}

fn mark_current_text_seen(last_text: &mut String, text: Result<String, arboard::Error>) {
    if let Ok(text) = text {
        *last_text = text;
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn backend_name_is_windows() {
        let backend = WindowsBackend::new().unwrap();
        assert_eq!(backend.name(), "windows");
    }

    #[test]
    fn rgba_to_png_encodes_png_header() {
        let png = rgba_to_png(1, 1, &[255, 0, 0, 255]).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn image_hash_uses_canonical_rgba_png() {
        let rgba = [255, 0, 0, 255];
        let original_png = rgba_to_png(1, 1, &rgba).unwrap();

        assert_eq!(
            canonical_image_hash_from_png(&original_png).unwrap(),
            canonical_image_hash_from_rgba(1, 1, &rgba).unwrap()
        );
    }
}
