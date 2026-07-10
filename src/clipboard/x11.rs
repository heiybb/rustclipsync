use super::{ClipboardBackend, ClipboardItem, clipboard_text_looks_like_png_bytes};
use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::process::{Command, Stdio};

pub struct X11Backend {
    last_text: String,
    last_image_hash: String,
}

impl X11Backend {
    pub fn new() -> Result<Self> {
        let status = Command::new("xclip").arg("-version").status();
        if status.is_err() {
            return Err(anyhow!("xclip is required for X11 clipboard backend"));
        }
        Ok(Self {
            last_text: String::new(),
            last_image_hash: String::new(),
        })
    }

    fn read_target(target: &str) -> Result<Option<Vec<u8>>> {
        let output = Command::new("xclip")
            .args(["-selection", "clipboard", "-o", "-t", target])
            .output()?;
        if output.status.success() && !output.stdout.is_empty() {
            Ok(Some(output.stdout))
        } else {
            Ok(None)
        }
    }

    fn write_target(target: &str, bytes: &[u8]) -> Result<()> {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i", "-t", target])
            .stdin(Stdio::piped())
            .spawn()?;
        child.stdin.as_mut().unwrap().write_all(bytes)?;
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("xclip write failed for target {target}"))
        }
    }
}

impl ClipboardBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn read_snapshot(&mut self) -> Result<Option<ClipboardItem>> {
        if let Some(bytes) = Self::read_target("image/png")? {
            let hash = calculate_bytes_hash(&bytes);
            if hash != self.last_image_hash {
                self.last_image_hash = hash;
                return Ok(Some(ClipboardItem::ImagePng(bytes)));
            }
        }
        if let Some(bytes) = Self::read_target("text/plain")? {
            let text = String::from_utf8_lossy(&bytes).to_string();
            if text != self.last_text && !clipboard_text_looks_like_png_bytes(&text) {
                self.last_text = text.clone();
                return Ok(Some(ClipboardItem::Text(text)));
            }
        }
        Ok(None)
    }

    fn write_item(&mut self, item: ClipboardItem) -> Result<()> {
        match item {
            ClipboardItem::Text(text) => Self::write_target("text/plain", text.as_bytes()),
            ClipboardItem::ImagePng(bytes) => Self::write_target("image/png", &bytes),
        }
    }
}

fn calculate_bytes_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_bytes_exposed_as_text_are_not_emitted() {
        let png_text = String::from_utf8_lossy(b"\x89PNG\r\n\x1a\n").to_string();

        assert!(clipboard_text_looks_like_png_bytes(&png_text));
    }
}
