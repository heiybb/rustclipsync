//! Tray-resident GUI front end.
//!
//! # Thread layout
//!
//! - The **main thread** belongs to gpui. `Application::run` pumps a win32
//!   `GetMessage`/`DispatchMessage` loop there for the lifetime of the process,
//!   which is also what makes the tray icon work: it is created on this thread
//!   and its window messages are dispatched by that same loop.
//! - The **tokio runtime** owns the sync client exactly as the CLI does. It is
//!   built here and never entered from the UI thread; only `Handle::spawn`
//!   crosses over, and results come back through [`LogBuffer`] and
//!   [`SyncStatus`], both of which are lock-per-operation and never block the
//!   UI for meaningful time.
//! - The **clipboard watcher thread** is unchanged from the CLI: it owns its
//!   own message-only window and its own pump.
//!
//! The supervisor task glues them together. It runs on gpui's foreground
//! executor, so it may hold the `!Send` tray handle, and it is the only thing
//! that opens, shows, or hides the log window.

mod log_view;
mod native;
mod tray;

use crate::logging::{self, ConsoleTarget, LogBuffer};
use crate::status::{SyncState, SyncStatus};
use crate::sync::run_client_with_status;
use anyhow::{Context as _, Result};
use futures_util::future::Either;
use gpui::{
    App, AppContext, AsyncApp, Bounds, Focusable, KeyBinding, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions, actions, px, size,
};
use log_view::LogView;
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use tokio::task::JoinHandle;
use tray::{GuiCommand, Tray};

actions!(rustclipsync, [HideLogWindow]);

/// How often the tray tooltip is reconciled with [`SyncStatus`].
const TOOLTIP_INTERVAL: Duration = Duration::from_secs(1);

/// Pass this on the command line (e.g. from a startup-folder shortcut) to start
/// without popping the window open.
const HIDDEN_FLAG: &str = "--hidden";

pub fn run() -> Result<()> {
    // A failure before the log window exists has nowhere to go: no console in
    // release builds, no window yet. Put it in a dialog so the user is not left
    // with a program that simply never appears.
    start().inspect_err(|err| native::fatal_dialog(&format!("启动失败：\n{err:#}")))
}

fn start() -> Result<()> {
    let buffer = LogBuffer::new(logging::DEFAULT_CAPACITY);
    // Debug builds keep their console subsystem, so mirroring to stderr there
    // is both possible and useful; release builds have no console at all.
    let console = if cfg!(debug_assertions) {
        ConsoleTarget::Stderr
    } else {
        ConsoleTarget::Discard
    };
    logging::init(console, Some(buffer.clone()))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start tokio runtime")?;

    let status = SyncStatus::new();
    let (command_tx, command_rx) = mpsc::unbounded_channel::<GuiCommand>();
    let start_hidden = std::env::args().any(|arg| arg == HIDDEN_FLAG);

    let handle = runtime.handle().clone();
    gpui::Application::new().run(move |cx: &mut App| {
        // Closing the log window must not end the process, and gpui posts
        // WM_QUIT when its last window is destroyed. `native::hide` keeps the
        // window alive instead of destroying it, so this only fires on paths
        // that really did remove a window.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                log::warn!("no windows left; the tray icon is still the app");
            }
        })
        .detach();

        cx.bind_keys([KeyBinding::new("escape", HideLogWindow, Some("LogWindow"))]);

        let tray = match tray::build(command_tx) {
            Ok(tray) => tray,
            Err(err) => {
                // Without an icon there is no way to reach the program, so
                // there is nothing useful to keep running for.
                log::error!("failed to create tray icon: {err:#}");
                native::fatal_dialog(&format!("无法创建托盘图标：\n{err:#}"));
                cx.quit();
                return;
            }
        };

        let mut supervisor = Supervisor {
            buffer,
            status,
            runtime: handle,
            tray,
            window: None,
            sync: None,
            tooltip_revision: u64::MAX,
        };

        supervisor.start_sync();
        supervisor.refresh_tooltip();

        if !start_hidden {
            supervisor.show_window(cx);
        }

        cx.spawn(async move |cx| supervisor.serve(command_rx, cx).await)
            .detach();
    });

    // Do not wait for the sync tasks: the clipboard watcher parks in a blocking
    // call and a plain runtime drop would hang here until it woke up.
    runtime.shutdown_background();

    Ok(())
}

/// How long to keep waiting for a posted show to take effect, and how often to
/// look. A few frames is plenty; the bound exists so a window that never
/// becomes visible cannot leave a task spinning.
const RAISE_POLL_INTERVAL: Duration = Duration::from_millis(16);
const RAISE_POLL_ATTEMPTS: u32 = 24;

/// Brings the window to the front, once it is actually on screen.
///
/// Activation cannot happen in the same breath as the show. `native::show`
/// *posts* the show request, while gpui's `activate_window` runs
/// `SetActiveWindow`/`SetFocus`/`SetForegroundWindow` from a task queued behind
/// it. When that task won the race it acted on a still-hidden window: those
/// calls return null, which is not an error, but gpui reports them through
/// `log_err()` with whatever stale `GetLastError()` was left in the thread —
/// the source of the bogus
/// `HRESULT(0x00000000) "The operation completed successfully"` lines.
///
/// Waiting for the window to be visible first makes the calls succeed, so they
/// both work and stay quiet. Activation is still worth doing: gpui pairs
/// `SetForegroundWindow` with a synthetic keypress to get around Windows'
/// foreground lock, which is what actually raises the window after a tray
/// click.
fn raise_when_shown(handle: WindowHandle<LogView>, cx: &mut App) {
    cx.spawn(async move |cx| {
        for _ in 0..RAISE_POLL_ATTEMPTS {
            let shown = handle
                .update(cx, |_, window, _| native::is_shown(window))
                .unwrap_or(false);

            if shown {
                handle
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
                return;
            }

            cx.background_executor().timer(RAISE_POLL_INTERVAL).await;
        }

        log::debug!("log window never became visible; skipping activation");
    })
    .detach();
}

struct Supervisor {
    buffer: Arc<LogBuffer>,
    status: Arc<SyncStatus>,
    runtime: Handle,
    /// `!Send`, and tied to the thread that created it — see the module docs.
    tray: Tray,
    window: Option<WindowHandle<LogView>>,
    sync: Option<JoinHandle<()>>,
    /// Last [`SyncStatus`] revision reflected in the tooltip.
    tooltip_revision: u64,
}

impl Supervisor {
    async fn serve(mut self, mut commands: UnboundedReceiver<GuiCommand>, cx: &mut AsyncApp) {
        loop {
            let tick = cx.background_executor().timer(TOOLTIP_INTERVAL);
            let next = commands.recv();
            futures_util::pin_mut!(tick, next);

            // `UnboundedReceiver::recv` is cancel-safe, so losing the race to
            // the timer cannot drop a queued command.
            match futures_util::future::select(next, tick).await {
                Either::Left((Some(command), _)) => {
                    let quit = cx.update(|cx| self.handle(command, cx)).unwrap_or(true);
                    if quit {
                        break;
                    }
                }
                // Unreachable in practice: the senders live in the tray
                // callbacks, which outlive everything. If it ever happens the
                // tray is about to be dropped, leaving no way to reach the
                // program, so quit rather than linger as an invisible process.
                Either::Left((None, _)) => {
                    log::error!("tray command channel closed; quitting");
                    cx.update(|cx| cx.quit()).ok();
                    break;
                }
                Either::Right((_elapsed, _)) => self.refresh_tooltip(),
            }
        }
    }

    /// Returns true when the process should quit.
    fn handle(&mut self, command: GuiCommand, cx: &mut App) -> bool {
        match command {
            GuiCommand::ShowWindow => self.show_window(cx),
            GuiCommand::ToggleWindow => self.toggle_window(cx),
            GuiCommand::RestartSync => {
                log::info!("restarting sync on request");
                self.start_sync();
                self.refresh_tooltip();
            }
            GuiCommand::Quit => {
                log::info!("quit requested");
                self.stop_sync();
                cx.quit();
                return true;
            }
        }
        false
    }

    fn refresh_tooltip(&mut self) {
        let revision = self.status.revision();
        if revision == self.tooltip_revision {
            return;
        }
        self.tooltip_revision = revision;

        let snapshot = self.status.snapshot();
        let tooltip = if snapshot.detail.is_empty() {
            format!("rustclipsync - {}", snapshot.state.label())
        } else {
            format!(
                "rustclipsync - {}\n{}",
                snapshot.state.label(),
                snapshot.detail
            )
        };
        self.tray.set_tooltip(&tooltip);
    }

    fn start_sync(&mut self) {
        self.stop_sync();

        let config = match crate::config::load_config() {
            Ok(config) => config,
            Err(err) => {
                // Report through the log window rather than dying: the window
                // is the only place the user can read this.
                log::error!("{err:#}");
                self.status.set(SyncState::Failed, "配置无效，见日志");
                return;
            }
        };

        self.status
            .set(SyncState::Connecting, config.server_url.clone());

        let status = self.status.clone();
        self.sync = Some(self.runtime.spawn(async move {
            if let Err(err) = run_client_with_status(config, status.clone()).await {
                log::error!("sync loop exited: {err:#}");
                status.set(SyncState::Failed, format!("{err:#}"));
            }
        }));
    }

    fn stop_sync(&mut self) {
        if let Some(sync) = self.sync.take() {
            sync.abort();
            self.status.set(SyncState::Idle, String::new());
            // The clipboard watcher runs in `spawn_blocking` and cannot be
            // cancelled: its thread stays parked until the next clipboard
            // change, notices its receiver is gone, and unwinds itself. So a
            // restart can briefly overlap with the previous watcher.
            log::info!("previous sync loop aborted");
        }
    }

    fn toggle_window(&mut self, cx: &mut App) {
        let shown = self
            .window
            .as_ref()
            .and_then(|handle| {
                handle
                    .update(cx, |_, window, _| native::is_shown(window))
                    .ok()
            })
            .unwrap_or(false);

        if shown {
            self.hide_window(cx);
        } else {
            self.show_window(cx);
        }
    }

    fn show_window(&mut self, cx: &mut App) {
        if let Some(handle) = self.window {
            let shown = handle.update(cx, |view, window, cx| {
                native::show(window);
                let focus = view.focus_handle(cx);
                window.focus(&focus);
                view.set_visible(true, cx);
            });

            match shown {
                Ok(()) => {
                    raise_when_shown(handle, cx);
                    return;
                }
                Err(err) => {
                    log::warn!("log window is gone, opening a new one: {err}");
                    self.window = None;
                }
            }
        }

        if let Err(err) = self.open_window(cx) {
            log::error!("failed to open log window: {err:#}");
        }
    }

    fn hide_window(&mut self, cx: &mut App) {
        if let Some(handle) = self.window {
            handle
                .update(cx, |view, window, cx| {
                    native::hide(window);
                    view.set_visible(false, cx);
                })
                .ok();
        }
    }

    fn open_window(&mut self, cx: &mut App) -> Result<()> {
        let bounds = Bounds::centered(None, size(px(960.0), px(560.0)), cx);
        let buffer = self.buffer.clone();
        let status = self.status.clone();

        let handle = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("rustclipsync".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(560.0), px(320.0))),
                app_id: Some("rustclipsync".to_string()),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| LogView::new(buffer, status, cx));

                let focus = view.read(cx).focus_handle(cx);
                window.focus(&focus);

                // Veto the close and hide instead, so the sync loop survives
                // the user dismissing the window. A weak handle keeps this
                // callback from holding the view alive by itself.
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    weak.update(cx, |view, cx| view.set_visible(false, cx)).ok();
                    native::hide(window);
                    false
                });

                view
            },
        )?;

        cx.activate(true);
        self.window = Some(handle);

        Ok(())
    }
}
