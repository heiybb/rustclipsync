use super::{
    ClipboardBackend, ClipboardItem, ClipboardWatcher, canonical_image_hash_from_png,
    canonical_image_hash_from_rgba, mark_current_text_seen, rgba_to_png,
};
use anyhow::Result;
use arboard::{Clipboard, ImageData};
use clipboard_win::{Clipboard as WinClipboard, Getter, formats::FileList};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub struct WindowsBackend {
    clipboard: Clipboard,
    last_text: String,
    last_files: Vec<PathBuf>,
    pending_files: VecDeque<PathBuf>,
    last_image_hash: String,
}

pub struct WindowsWatcher {
    rx: mpsc::Receiver<()>,
    _thread: thread::JoinHandle<()>,
}

// WindowsWatcher is now thread-safe because it just holds a channel receiver and a handle.
unsafe impl Send for WindowsWatcher {}

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

    pub fn new_pair() -> Result<(Self, WindowsWatcher)> {
        let backend = Self::new()?;
        let watcher = WindowsWatcher::new()?;
        Ok((backend, watcher))
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

impl WindowsWatcher {
    pub fn new() -> Result<Self> {
        let (tx, rx) = mpsc::channel();

        // Start a dedicated thread for the message window and loop
        let _thread = thread::spawn(move || {
            unsafe {
                let instance = GetModuleHandleW(std::ptr::null());
                let class_name = "RustClipSyncWatcher\0".encode_utf16().collect::<Vec<u16>>();

                let wnd_class = WNDCLASSW {
                    style: 0,
                    lpfnWndProc: Some(DefWindowProcW),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: std::ptr::null_mut(),
                    hCursor: std::ptr::null_mut(),
                    hbrBackground: std::ptr::null_mut(),
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class_name.as_ptr(),
                };

                RegisterClassW(&wnd_class);

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null_mut(),
                );

                if hwnd.is_null() {
                    log::error!("failed to create message window");
                    return;
                }

                if AddClipboardFormatListener(hwnd) == 0 {
                    log::error!("failed to add clipboard listener");
                    DestroyWindow(hwnd);
                    return;
                }

                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, hwnd, 0, 0) != 0 {
                    if msg.message == WM_CLIPBOARDUPDATE && tx.send(()).is_err() {
                        break; // Main thread hung up
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                RemoveClipboardFormatListener(hwnd);
                DestroyWindow(hwnd);
            }
        });

        Ok(Self { rx, _thread })
    }
}

impl ClipboardWatcher for WindowsWatcher {
    fn wait_for_change(&mut self) -> Result<()> {
        self.rx
            .recv()
            .map_err(|e| anyhow::anyhow!("watcher thread disconnected: {}", e))
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

                self.clipboard.set_image(ImageData {
                    width,
                    height,
                    bytes: Cow::Owned(raw),
                })?;

                self.last_image_hash = canonical_image_hash_from_png(&bytes)?;
                mark_current_text_seen(&mut self.last_text, self.clipboard.get_text());
                Ok(())
            }
            ClipboardItem::FilePath(_) => Ok(()),
        }
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
}
