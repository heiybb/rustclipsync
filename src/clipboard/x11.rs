use super::{ClipboardBackend, ClipboardItem, clipboard_text_looks_like_png_bytes};
use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// X11 selection transfers require the current selection owner to answer the
/// conversion request; a hung owner (suspended app, stale lock-screen client)
/// makes xclip block forever. These calls run while holding the shared
/// backend mutex, so an unbounded xclip wedges both sync directions at once —
/// kill the child after this long and treat the attempt as failed instead.
const XCLIP_TIMEOUT: Duration = Duration::from_secs(5);

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
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-o", "-t", target])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        // Drain stdout on its own thread: a payload larger than the pipe
        // buffer would otherwise deadlock against the exit polling below.
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stdout.read_to_end(&mut bytes);
            bytes
        });
        let status = wait_with_timeout(&mut child, XCLIP_TIMEOUT)?;
        let bytes = reader
            .join()
            .map_err(|_| anyhow!("xclip stdout reader panicked"))?;
        match status {
            Some(status) if status.success() && !bytes.is_empty() => Ok(Some(bytes)),
            Some(_) => Ok(None),
            None => {
                log::warn!(
                    "xclip read for target {target} timed out after {}s (selection owner unresponsive), killed",
                    XCLIP_TIMEOUT.as_secs()
                );
                Ok(None)
            }
        }
    }

    fn write_target(target: &str, bytes: &[u8]) -> Result<()> {
        let mut child = Command::new("xclip")
            .args(["-selection", "clipboard", "-i", "-t", target])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        // Feed stdin from its own thread: if xclip never reads (X server
        // unresponsive), a direct write_all on a full pipe would block the
        // caller forever — the exact wedge the timeout exists to prevent.
        // Dropping the handle at thread end gives xclip its EOF.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let payload = bytes.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        });
        let status = wait_with_timeout(&mut child, XCLIP_TIMEOUT)?;
        let _ = writer.join();
        match status {
            Some(status) if status.success() => Ok(()),
            Some(_) => Err(anyhow!("xclip write failed for target {target}")),
            None => Err(anyhow!(
                "xclip write for target {target} timed out after {}s, killed",
                XCLIP_TIMEOUT.as_secs()
            )),
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

/// Waits for the child to exit, polling `try_wait`. Returns `Ok(None)` when
/// the timeout elapses, after killing and reaping the child.
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(25));
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

    #[test]
    fn wait_with_timeout_kills_hung_child() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let started = Instant::now();

        let status = wait_with_timeout(&mut child, Duration::from_millis(200)).unwrap();

        assert!(status.is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn wait_with_timeout_returns_status_of_quick_child() {
        let mut child = Command::new("true").spawn().unwrap();

        let status = wait_with_timeout(&mut child, Duration::from_secs(5)).unwrap();

        assert!(status.expect("child exits before timeout").success());
    }
}
