//! Show/hide plumbing for the log window.
//!
//! gpui posts `WM_QUIT` as soon as its last window is destroyed, which for a
//! tray-resident program would mean closing the log window kills the sync loop.
//! So the window is never destroyed: its close request is vetoed and the HWND
//! is hidden instead. gpui keeps the window in its registry, the process keeps
//! running, and reopening is instant with scroll position intact.
//!
//! gpui has no API for hiding a window, hence the direct win32 calls.

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, MB_ICONERROR, MB_OK, MessageBoxW, SW_HIDE, SW_RESTORE, SW_SHOW,
    ShowWindowAsync,
};

/// `Window` has an inherent `window_handle()` that returns gpui's own handle
/// type and shadows the `HasWindowHandle` method, so the trait is called
/// explicitly here.
fn hwnd(window: &Window) -> Option<HWND> {
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(win32) => Some(win32.hwnd.get() as HWND),
        other => {
            log::error!("unexpected raw window handle on windows: {other:?}");
            None
        }
    }
}

/// `ShowWindowAsync`, never `ShowWindow`.
///
/// These are called from inside a gpui update, where the window's `RefCell`
/// state is already borrowed. `ShowWindow` dispatches `WM_SHOWWINDOW`,
/// `WM_SIZE` and friends synchronously, re-entering gpui's window procedure
/// and panicking on the double borrow. The async form posts the request
/// instead, which is what gpui's own `activate`/`minimize` do.
fn post_show_command(window: &Window, command: i32) {
    if let Some(hwnd) = hwnd(window) {
        unsafe { ShowWindowAsync(hwnd, command) };
    }
}

pub fn hide(window: &Window) {
    post_show_command(window, SW_HIDE);
}

pub fn show(window: &Window) {
    let Some(hwnd) = hwnd(window) else {
        return;
    };

    // A window the user minimised has to be restored rather than merely shown,
    // or it stays on the taskbar. `IsIconic` is a plain query, so reading it
    // here is safe even mid-update.
    let command = if unsafe { IsIconic(hwnd) } != 0 {
        SW_RESTORE
    } else {
        SW_SHOW
    };

    // Note this only *posts* the show, and deliberately does not activate: see
    // `raise_when_shown` in the supervisor for why activation has to wait.
    post_show_command(window, command);
}

/// Whether the window is actually in front of the user.
///
/// A minimised window is still "visible" to `IsWindowVisible`, but from the
/// tray icon's point of view it is put away: clicking the icon should bring it
/// back, not hide it again.
pub fn is_shown(window: &Window) -> bool {
    hwnd(window).is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 })
}

/// Modal error dialog for failures that leave no working way to report them.
///
/// A `windows_subsystem = "windows"` process has no stderr, and a startup
/// failure means there is no log window either — without this the program would
/// simply not appear, with nothing to explain why.
pub fn fatal_dialog(message: &str) {
    let title: Vec<u16> = "rustclipsync\0".encode_utf16().collect();
    let body: Vec<u16> = format!("{message}\0").encode_utf16().collect();

    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}
