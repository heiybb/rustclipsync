use super::{
    ClipboardBackend, ClipboardItem, canonical_image_hash_from_png, canonical_image_hash_from_rgba,
    clipboard_text_looks_like_png_bytes, mark_current_text_seen, rgba_to_png,
};
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use std::borrow::Cow;

pub struct ArboardBackend {
    clipboard: Clipboard,
    last_text: String,
    last_image_hash: String,
}

impl ArboardBackend {
    pub fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Clipboard::new()?,
            last_text: String::new(),
            last_image_hash: String::new(),
        })
    }
}

impl ClipboardBackend for ArboardBackend {
    fn name(&self) -> &'static str {
        "arboard"
    }

    fn read_snapshot(&mut self) -> Result<Option<ClipboardItem>> {
        if let Ok(image) = self.clipboard.get_image() {
            let png = rgba_to_png(image.width, image.height, image.bytes.as_ref())?;
            let hash =
                canonical_image_hash_from_rgba(image.width, image.height, image.bytes.as_ref())?;
            if hash != self.last_image_hash {
                self.last_image_hash = hash;
                return Ok(Some(ClipboardItem::ImagePng(png)));
            }
        }

        if let Ok(text) = self.clipboard.get_text()
            && text != self.last_text
            && !clipboard_text_looks_like_png_bytes(&text)
        {
            self.last_text = text.clone();
            return Ok(Some(ClipboardItem::Text(text)));
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
                self.clipboard.set_image(ImageData {
                    width,
                    height,
                    bytes: Cow::Owned(image.into_raw()),
                })?;
                self.last_image_hash = canonical_image_hash_from_png(&bytes)?;
                mark_current_text_seen(&mut self.last_text, self.clipboard.get_text());
                Ok(())
            }
            ClipboardItem::FilePath(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_png_bytes_exposed_as_text() {
        let text = String::from_utf8_lossy(b"\x89PNG\r\n\x1a\n").to_string();
        assert!(clipboard_text_looks_like_png_bytes(&text));
    }

    #[test]
    fn stale_text_is_marked_seen_after_image_write() {
        let mut last_text = String::new();
        mark_current_text_seen(&mut last_text, Ok("stale text".to_string()));
        assert_eq!(last_text, "stale text");
    }
}
