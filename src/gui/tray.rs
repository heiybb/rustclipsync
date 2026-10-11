//! Notification-area icon and its context menu.
//!
//! `tray-icon` needs a win32 message loop on the thread that creates the icon,
//! and it needs the icon to stay on that thread. gpui's `Application::run`
//! pumps `GetMessage`/`DispatchMessage` on the main thread for the lifetime of
//! the process, so the icon is created there and never moved.
//!
//! Its event callbacks run inside the window procedure, i.e. re-entrantly with
//! respect to gpui. They therefore do no work beyond posting a [`GuiCommand`]
//! onto a channel that the supervisor drains from a normal gpui task.

use anyhow::{Context, Result};
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::WindowsAndMessaging::{SM_CXSMICON, SM_CYSMICON};

/// Resource id of the icon in `assets/rustclipsync.rc`, embedded by `build.rs`.
const ICON_RESOURCE_ID: u16 = 1;

const MENU_SHOW: &str = "rustclipsync.show";
const MENU_RESTART: &str = "rustclipsync.restart";
const MENU_QUIT: &str = "rustclipsync.quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiCommand {
    /// Left click on the icon: show the window, or hide it if it is already up.
    ToggleWindow,
    ShowWindow,
    /// Re-read `config.toml` and restart the sync loop.
    RestartSync,
    Quit,
}

/// Owns the tray icon and the menu items it refers to.
///
/// `muda` menu items are reference-counted internally, but keeping them here
/// documents that their lifetime has to cover the icon's.
pub struct Tray {
    icon: TrayIcon,
    _menu: Menu,
    _items: Vec<MenuItem>,
}

impl Tray {
    pub fn set_tooltip(&self, tooltip: &str) {
        if let Err(err) = self.icon.set_tooltip(Some(tooltip)) {
            log::warn!("failed to update tray tooltip: {err}");
        }
    }
}

/// The notification area's icon size for the current system DPI.
///
/// `GetSystemMetrics` would answer 16x16 no matter the scaling factor, because
/// gpui's manifest makes this process per-monitor DPI aware. Asking for the
/// metric *at* the system DPI gives 16/20/24/32 as the user's scaling factor
/// rises, which are exactly the entries the bundled icon carries — so Windows
/// picks one verbatim instead of resampling.
fn notification_icon_size() -> (u32, u32) {
    unsafe {
        let dpi = GetDpiForSystem();
        (
            GetSystemMetricsForDpi(SM_CXSMICON, dpi).max(1) as u32,
            GetSystemMetricsForDpi(SM_CYSMICON, dpi).max(1) as u32,
        )
    }
}

pub fn build(commands: UnboundedSender<GuiCommand>) -> Result<Tray> {
    let icon_size = notification_icon_size();
    log::debug!(
        "tray icon size for the current dpi: {}x{}",
        icon_size.0,
        icon_size.1
    );

    // Loaded from the executable's own resources rather than decoded here, so
    // the icon on the taskbar and the icon in the tray can never disagree.
    let image = tray_icon::Icon::from_resource(ICON_RESOURCE_ID, Some(icon_size))
        .context("failed to load tray icon from executable resources")?;

    let items = vec![
        MenuItem::with_id(MENU_SHOW, "显示日志窗口", true, None),
        MenuItem::with_id(MENU_RESTART, "重载配置并重启同步", true, None),
        MenuItem::with_id(MENU_QUIT, "退出", true, None),
    ];

    let menu = Menu::new();
    menu.append(&items[0])?;
    menu.append(&items[1])?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&items[2])?;

    let icon = TrayIconBuilder::new()
        .with_menu(Box::new(menu.clone()))
        .with_icon(image)
        .with_tooltip("rustclipsync")
        // Left click toggles the window; the menu is right-click only.
        .with_menu_on_left_click(false)
        .build()
        .context("failed to create tray icon")?;

    let menu_commands = commands.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let Some(command) = command_for_menu_id(event.id().as_ref()) else {
            log::warn!("unknown tray menu id: {}", event.id().as_ref());
            return;
        };
        // A closed channel means the supervisor is gone, i.e. the app is
        // already shutting down; nothing to report.
        let _ = menu_commands.send(command);
    }));

    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        // Act on release, not press, so a click that opens the context menu
        // does not also toggle the window.
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let _ = commands.send(GuiCommand::ToggleWindow);
        }
    }));

    Ok(Tray {
        icon,
        _menu: menu,
        _items: items,
    })
}

fn command_for_menu_id(id: &str) -> Option<GuiCommand> {
    match id {
        MENU_SHOW => Some(GuiCommand::ShowWindow),
        MENU_RESTART => Some(GuiCommand::RestartSync),
        MENU_QUIT => Some(GuiCommand::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_id_maps_to_a_command() {
        assert_eq!(command_for_menu_id(MENU_SHOW), Some(GuiCommand::ShowWindow));
        assert_eq!(
            command_for_menu_id(MENU_RESTART),
            Some(GuiCommand::RestartSync)
        );
        assert_eq!(command_for_menu_id(MENU_QUIT), Some(GuiCommand::Quit));
    }

    #[test]
    fn unknown_menu_id_maps_to_nothing() {
        assert_eq!(command_for_menu_id("rustclipsync.nope"), None);
    }

    /// Sizes the notification area can ask for, across the DPI scaling factors
    /// Windows offers. An icon missing one of these gets resampled and looks
    /// soft, which is the whole reason for shipping a multi-resolution file.
    const REQUIRED_ICON_SIZES: [u32; 4] = [16, 20, 24, 32];

    /// Alpha at or above this counts as visibly drawn. Antialiased edges fade
    /// to nearly nothing, and counting those as content would make even a
    /// heavily padded icon measure as full-bleed.
    const SOLID_ALPHA: u8 = 128;

    /// The glyph has to reach the canvas edge on its longer axis, or it renders
    /// smaller than every neighbouring icon in the notification area.
    const MIN_LONG_AXIS_FILL: f32 = 0.95;

    const ICON: &[u8] = include_bytes!("../../assets/rustclipsync.ico");

    struct IconEntry {
        width: u32,
        height: u32,
        offset: usize,
        length: usize,
    }

    /// Parses an `.ico` directory.
    ///
    /// Deliberately hand-rolled: the point is to check the file's own header,
    /// not to trust a decoder that might normalise it.
    fn icon_entries(blob: &[u8]) -> Vec<IconEntry> {
        assert!(blob.len() >= 6, "icon is too short to hold a directory");
        assert_eq!(
            u16::from_le_bytes([blob[2], blob[3]]),
            1,
            "not an icon resource (type must be 1)"
        );

        let count = u16::from_le_bytes([blob[4], blob[5]]) as usize;
        (0..count)
            .map(|ix| {
                let entry = 6 + ix * 16;
                assert!(blob.len() >= entry + 16, "truncated icon directory");
                // A zero byte means 256: the field is only one byte wide.
                let width = blob[entry] as u32;
                let height = blob[entry + 1] as u32;
                IconEntry {
                    width: if width == 0 { 256 } else { width },
                    height: if height == 0 { 256 } else { height },
                    offset: u32::from_le_bytes([
                        blob[entry + 12],
                        blob[entry + 13],
                        blob[entry + 14],
                        blob[entry + 15],
                    ]) as usize,
                    length: u32::from_le_bytes([
                        blob[entry + 8],
                        blob[entry + 9],
                        blob[entry + 10],
                        blob[entry + 11],
                    ]) as usize,
                }
            })
            .collect()
    }

    /// Fraction of the canvas the visibly drawn pixels span, as (width, height).
    fn glyph_fill(entry: &IconEntry, blob: &[u8]) -> (f32, f32) {
        let payload = &blob[entry.offset..entry.offset + entry.length];
        let image = image::load_from_memory_with_format(payload, image::ImageFormat::Png)
            .expect("icon entries are PNG-compressed")
            .to_rgba8();

        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel.0[3] >= SOLID_ALPHA {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
        assert!(min_x != u32::MAX, "icon entry is entirely transparent");

        (
            (max_x - min_x + 1) as f32 / image.width() as f32,
            (max_y - min_y + 1) as f32 / image.height() as f32,
        )
    }

    #[test]
    fn bundled_icon_covers_every_notification_area_size() {
        // `build.rs` hands this file to the resource compiler, and
        // `Icon::from_resource` then asks Windows for one specific size out of
        // it. Swapping in a single-resolution file would still build and still
        // run, only blurry -- so assert the resolutions here instead.
        let sizes: Vec<(u32, u32)> = icon_entries(ICON)
            .iter()
            .map(|entry| (entry.width, entry.height))
            .collect();

        for required in REQUIRED_ICON_SIZES {
            assert!(
                sizes.contains(&(required, required)),
                "assets/rustclipsync.ico is missing a {required}x{required} entry; has {sizes:?}"
            );
        }
    }

    #[test]
    fn bundled_icon_glyph_reaches_the_canvas_edge() {
        // Transparent padding baked into the artwork is invisible to every
        // other check here: the file parses, the sizes are all present, the
        // resource embeds, the tray loads it -- and it still renders visibly
        // smaller than its neighbours, because the drawn pixels only occupy the
        // middle of each canvas. Measure the pixels, not the canvas.
        for entry in icon_entries(ICON) {
            let (fill_width, fill_height) = glyph_fill(&entry, ICON);
            let long_axis = fill_width.max(fill_height);

            assert!(
                long_axis >= MIN_LONG_AXIS_FILL,
                "{}x{} entry only fills {:.0}% x {:.0}% of its canvas; \
                 crop the transparent padding out of the artwork",
                entry.width,
                entry.height,
                fill_width * 100.0,
                fill_height * 100.0
            );
        }
    }
}
