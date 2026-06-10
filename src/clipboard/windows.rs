use super::{
    ClipboardBackend, ClipboardItem, ClipboardWatcher, canonical_image_hash_from_png,
    canonical_image_hash_from_rgba, mark_current_text_seen, rgba_to_png,
};
use anyhow::Result;
use arboard::Clipboard;
use clipboard_win::{Clipboard as WinClipboard, Getter, formats::FileList};
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

        // Try custom "PNG" format first to preserve transparency and avoid quality loss.
        if let Some(png_format_id) = clipboard_win::register_format("PNG") {
            let format_id = png_format_id.get();
            if clipboard_win::is_format_avail(format_id) {
                if let Ok(_clip) = WinClipboard::new_attempts(10) {
                    let mut png_bytes = Vec::new();
                    use clipboard_win::Getter;
                    if clipboard_win::formats::RawData(format_id).read_clipboard(&mut png_bytes).is_ok()
                        && !png_bytes.is_empty()
                    {
                        if let Ok(hash) = canonical_image_hash_from_png(&png_bytes) {
                            if hash != self.last_image_hash {
                                self.last_image_hash = hash;
                                return Ok(Some(ClipboardItem::ImagePng(png_bytes)));
                            }
                            // If hash is same, we don't fall back (it's the same image)
                            return Ok(None);
                        }
                    }
                }
            }
        }

        // Fallback to standard CF_DIB image via arboard
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
                let width = image.width();
                let height = image.height();
                let mut bgra = image.into_raw();
                // Swap R and B channels to get BGRA
                for chunk in bgra.chunks_exact_mut(4) {
                    chunk.swap(0, 2);
                }

                // Construct CF_DIB payload
                let mut dib_payload = Vec::with_capacity(40 + bgra.len());
                // Write BITMAPINFOHEADER
                dib_payload.extend_from_slice(&40u32.to_ne_bytes());
                dib_payload.extend_from_slice(&(width as i32).to_ne_bytes());
                dib_payload.extend_from_slice(&(-(height as i32)).to_ne_bytes());
                dib_payload.extend_from_slice(&1u16.to_ne_bytes());
                dib_payload.extend_from_slice(&32u16.to_ne_bytes());
                dib_payload.extend_from_slice(&0u32.to_ne_bytes());
                dib_payload.extend_from_slice(&(bgra.len() as u32).to_ne_bytes());
                dib_payload.extend_from_slice(&0i32.to_ne_bytes());
                dib_payload.extend_from_slice(&0i32.to_ne_bytes());
                dib_payload.extend_from_slice(&0u32.to_ne_bytes());
                dib_payload.extend_from_slice(&0u32.to_ne_bytes());
                dib_payload.extend_from_slice(&bgra);

                // Open clipboard, empty it, and set both formats
                {
                    let _clip = WinClipboard::new_attempts(10)
                        .map_err(|e| anyhow::anyhow!("windows clipboard lock error: {:?}", e))?;
                    clipboard_win::empty()
                        .map_err(|e| anyhow::anyhow!("windows clipboard empty error: {:?}", e))?;

                    // Write custom "PNG" format first
                    if let Some(png_format_id) = clipboard_win::register_format("PNG") {
                        let _ = clipboard_win::raw::set_without_clear(png_format_id.get(), &bytes);
                    }

                    // Write CF_DIB format
                    let _ = clipboard_win::raw::set_without_clear(8, &dib_payload);
                }

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

    #[test]
    fn test_png_clipboard_roundtrip() {
        let mut backend = WindowsBackend::new().unwrap();
        // Construct a small 2x2 red PNG
        let image = image::RgbaImage::from_fn(2, 2, |_, _| image::Rgba([255, 0, 0, 255]));
        let mut png_bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png_bytes, image::ImageFormat::Png).unwrap();
        let png_bytes = png_bytes.into_inner();

        // Write it using write_item
        backend.write_item(ClipboardItem::ImagePng(png_bytes.clone())).unwrap();

        // Read it back using read_snapshot
        // We clear last_image_hash to force it to read
        backend.last_image_hash = String::new();
        let item = backend.read_snapshot().unwrap().unwrap();
        match item {
            ClipboardItem::ImagePng(retrieved_bytes) => {
                let decoded = image::load_from_memory(&retrieved_bytes).unwrap().to_rgba8();
                assert_eq!(decoded.width(), 2);
                assert_eq!(decoded.height(), 2);
                assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
            }
            _ => panic!("Expected ImagePng"),
        }
    }
}
