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

/// Text targets to read, best first. `UTF8_STRING` is what xclip asks for when
/// no `-t` is given, and what arboard-based apps (snip and others) offer next to
/// `text/plain;charset=utf-8`; they never offer bare `text/plain`, so reading
/// only that skipped their text entirely. `text/plain` stays as the fallback for
/// owners that offer nothing else, which includes this backend's own writes.
const TEXT_READ_TARGETS: [&str; 2] = ["UTF8_STRING", "text/plain"];

/// Outcome of asking the selection owner for one target.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TargetRead {
    Data(Vec<u8>),
    /// The owner refused the target or returned nothing for it.
    Unavailable,
    /// The owner never answered and xclip was killed. It will not answer the
    /// next target either, so callers stop instead of trying one.
    TimedOut,
}

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

    fn read_target(target: &str) -> Result<TargetRead> {
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
            Some(status) if status.success() && !bytes.is_empty() => Ok(TargetRead::Data(bytes)),
            Some(_) => Ok(TargetRead::Unavailable),
            None => {
                log::warn!(
                    "xclip read for target {target} timed out after {}s (selection owner unresponsive), killed",
                    XCLIP_TIMEOUT.as_secs()
                );
                Ok(TargetRead::TimedOut)
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
        match Self::read_target("image/png")? {
            TargetRead::Data(bytes) => {
                let hash = calculate_bytes_hash(&bytes);
                if hash != self.last_image_hash {
                    self.last_image_hash = hash;
                    return Ok(Some(ClipboardItem::ImagePng(bytes)));
                }
            }
            TargetRead::Unavailable => {}
            TargetRead::TimedOut => return Ok(None),
        }
        if let Some(bytes) = read_first_text_target(Self::read_target)? {
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

/// Reads the first of [`TEXT_READ_TARGETS`] the owner provides. Stops at the
/// first data, even unchanged data, so a steady clipboard costs one read per
/// poll, and stops at a timeout so a hung owner costs one `XCLIP_TIMEOUT`.
fn read_first_text_target(
    mut read: impl FnMut(&str) -> Result<TargetRead>,
) -> Result<Option<Vec<u8>>> {
    for target in TEXT_READ_TARGETS {
        match read(target)? {
            TargetRead::Data(bytes) => return Ok(Some(bytes)),
            TargetRead::Unavailable => continue,
            TargetRead::TimedOut => return Ok(None),
        }
    }
    Ok(None)
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

    /// Simulates a selection owner offering `offered`; records each request.
    fn owner<'a>(
        offered: &'a [(&'a str, TargetRead)],
        asked: &'a mut Vec<String>,
    ) -> impl FnMut(&str) -> Result<TargetRead> + 'a {
        move |target| {
            asked.push(target.to_string());
            Ok(offered
                .iter()
                .find(|(t, _)| *t == target)
                .map(|(_, read)| read.clone())
                .unwrap_or(TargetRead::Unavailable))
        }
    }

    #[test]
    fn reads_utf8_string_from_owner_without_bare_text_plain() {
        // What arboard (snip) offers: never bare text/plain.
        let offered = [
            ("UTF8_STRING", TargetRead::Data(b"/tmp/snip.png".to_vec())),
            (
                "text/plain;charset=utf-8",
                TargetRead::Data(b"/tmp/snip.png".to_vec()),
            ),
        ];
        let mut asked = Vec::new();

        let text = read_first_text_target(owner(&offered, &mut asked)).unwrap();

        assert_eq!(text.as_deref(), Some(&b"/tmp/snip.png"[..]));
        assert_eq!(asked, ["UTF8_STRING"]);
    }

    #[test]
    fn falls_back_to_text_plain_when_utf8_string_is_refused() {
        // What `xclip -i -t text/plain` (this backend's own write) offers.
        let offered = [("text/plain", TargetRead::Data(b"remote".to_vec()))];
        let mut asked = Vec::new();

        let text = read_first_text_target(owner(&offered, &mut asked)).unwrap();

        assert_eq!(text.as_deref(), Some(&b"remote"[..]));
        assert_eq!(asked, ["UTF8_STRING", "text/plain"]);
    }

    #[test]
    fn stops_after_a_timeout_instead_of_waiting_again() {
        let offered = [
            ("UTF8_STRING", TargetRead::TimedOut),
            ("text/plain", TargetRead::Data(b"unreachable".to_vec())),
        ];
        let mut asked = Vec::new();

        let text = read_first_text_target(owner(&offered, &mut asked)).unwrap();

        assert_eq!(text, None);
        assert_eq!(asked, ["UTF8_STRING"]);
    }

    #[test]
    fn no_text_when_owner_offers_none_of_the_targets() {
        let offered = [("image/png", TargetRead::Data(b"\x89PNG".to_vec()))];
        let mut asked = Vec::new();

        let text = read_first_text_target(owner(&offered, &mut asked)).unwrap();

        assert_eq!(text, None);
        assert_eq!(asked, ["UTF8_STRING", "text/plain"]);
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
