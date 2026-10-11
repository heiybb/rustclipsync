//! The log window.
//!
//! The view owns no log state of its own: it polls [`LogBuffer`] and
//! [`SyncStatus`] revisions and re-reads them when they change. Polling rather
//! than waking on every record is deliberate — records arrive from tokio
//! worker threads at unbounded rates, and a burst of them must not turn into a
//! burst of repaints.

use super::HideLogWindow;
use crate::logging::{LogBuffer, LogEntry};
use crate::status::{StatusSnapshot, SyncState, SyncStatus};
use gpui::{
    App, FocusHandle, Focusable, Hsla, ScrollStrategy, SharedString, Task, UniformListScrollHandle,
    Window, div, prelude::*, px, rgb, uniform_list,
};
use std::sync::Arc;
use std::time::Duration;

/// Repaint cadence while the window is on screen.
const REFRESH_VISIBLE: Duration = Duration::from_millis(100);
/// Cadence while hidden. Entries are still collected — only painting stops —
/// so this only needs to be short enough that the window looks current the
/// moment it is reopened.
const REFRESH_HIDDEN: Duration = Duration::from_millis(1000);

const ROW_HEIGHT: f32 = 18.0;

const BG: u32 = 0x14171C;
const HEADER_BG: u32 = 0x1B1F26;
const BORDER: u32 = 0x2A2F3A;
const TEXT: u32 = 0xC9D1D9;
const DIM: u32 = 0x7D8590;
const ACCENT: u32 = 0x2F6FED;
const CHIP_BG: u32 = 0x22272F;

const LEVEL_ERROR: u32 = 0xF85149;
const LEVEL_WARN: u32 = 0xE3B341;
const LEVEL_INFO: u32 = 0x58A6FF;
const LEVEL_DEBUG: u32 = 0x8B949E;
const LEVEL_TRACE: u32 = 0x6E7681;

const LEVEL_CHOICES: [(log::LevelFilter, &str); 5] = [
    (log::LevelFilter::Error, "ERROR"),
    (log::LevelFilter::Warn, "WARN"),
    (log::LevelFilter::Info, "INFO"),
    (log::LevelFilter::Debug, "DEBUG"),
    (log::LevelFilter::Trace, "TRACE"),
];

pub struct LogView {
    buffer: Arc<LogBuffer>,
    status_source: Arc<SyncStatus>,

    /// Everything currently retained, newest last.
    entries: Vec<Arc<LogEntry>>,
    /// `entries` after the level filter. Rebuilt on every change; the list
    /// element needs random access by row index.
    rows: Vec<Arc<LogEntry>>,

    buffer_revision: u64,
    status_revision: u64,
    status: StatusSnapshot,

    min_level: log::LevelFilter,
    follow_tail: bool,
    visible: bool,

    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    /// Dropping this stops the poll loop, which is what should happen when the
    /// view goes away.
    _poll: Task<()>,
}

impl LogView {
    pub fn new(
        buffer: Arc<LogBuffer>,
        status_source: Arc<SyncStatus>,
        cx: &mut Context<Self>,
    ) -> Self {
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let Ok(next) = this.update(cx, |view, cx| view.pull(cx)) else {
                    // The view was released; nothing left to refresh.
                    break;
                };
                cx.background_executor().timer(next).await;
            }
        });

        let mut view = Self {
            buffer,
            status_source,
            entries: Vec::new(),
            rows: Vec::new(),
            buffer_revision: 0,
            status_revision: 0,
            status: StatusSnapshot {
                state: SyncState::Idle,
                detail: String::new(),
            },
            min_level: log::LevelFilter::Info,
            follow_tail: true,
            visible: true,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _poll: poll,
        };

        // Seed from whatever has already been logged, including anything
        // recorded before the window existed.
        view.reload_entries();
        view.reload_status();
        view.scroll_to_tail();

        view
    }

    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.reload_entries();
            self.reload_status();
            self.scroll_to_tail();
            cx.notify();
        }
    }

    /// Re-reads whatever changed and returns the delay before the next check.
    fn pull(&mut self, cx: &mut Context<Self>) -> Duration {
        let mut changed = false;

        if self.buffer.revision() != self.buffer_revision {
            self.reload_entries();
            changed = true;
        }
        if self.status_source.revision() != self.status_revision {
            self.reload_status();
            changed = true;
        }

        if changed && self.visible {
            self.scroll_to_tail();
            cx.notify();
        }

        if self.visible {
            REFRESH_VISIBLE
        } else {
            REFRESH_HIDDEN
        }
    }

    fn reload_entries(&mut self) {
        // Read the revision first: a record that lands between the two reads
        // makes the revision look stale and costs one extra refresh, whereas
        // reading it second could make a stale snapshot look current.
        self.buffer_revision = self.buffer.revision();
        self.entries = self.buffer.snapshot();
        self.rebuild_rows();
    }

    fn reload_status(&mut self) {
        self.status_revision = self.status_source.revision();
        self.status = self.status_source.snapshot();
    }

    fn rebuild_rows(&mut self) {
        let min_level = self.min_level;
        self.rows = self
            .entries
            .iter()
            .filter(|entry| entry.level <= min_level)
            .cloned()
            .collect();
    }

    fn scroll_to_tail(&self) {
        if !self.follow_tail {
            return;
        }
        if let Some(last) = self.rows.len().checked_sub(1) {
            self.scroll.scroll_to_item(last, ScrollStrategy::Top);
        }
    }

    fn set_min_level(&mut self, min_level: log::LevelFilter, cx: &mut Context<Self>) {
        if self.min_level == min_level {
            return;
        }
        self.min_level = min_level;
        self.rebuild_rows();
        self.scroll_to_tail();
        cx.notify();
    }

    fn render_status(&self) -> impl IntoElement {
        let (dot, label) = match self.status.state {
            SyncState::Idle => (DIM, SyncState::Idle.label()),
            SyncState::Connecting => (LEVEL_INFO, SyncState::Connecting.label()),
            SyncState::Connected => (0x3FB950, SyncState::Connected.label()),
            SyncState::Retrying => (LEVEL_WARN, SyncState::Retrying.label()),
            SyncState::Failed => (LEVEL_ERROR, SyncState::Failed.label()),
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(div().size(px(8.0)).rounded_full().bg(rgb(dot)))
            .child(div().flex_none().text_color(rgb(TEXT)).child(label))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(rgb(DIM))
                    .child(SharedString::from(self.status.detail.clone())),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut levels = div().flex().flex_row().items_center().gap_1();
        for (level, label) in LEVEL_CHOICES {
            let active = self.min_level == level;
            levels = levels.child(
                div()
                    .id(SharedString::from(label))
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .bg(rgb(if active { ACCENT } else { CHIP_BG }))
                    .text_color(rgb(if active { 0xFFFFFF } else { DIM }))
                    .child(label)
                    .on_click(cx.listener(move |view, _, _, cx| view.set_min_level(level, cx))),
            );
        }

        let dropped = self.buffer.dropped();
        let counter = if dropped == 0 {
            format!("{} 条", self.rows.len())
        } else {
            format!("{} 条 (已丢弃 {dropped})", self.rows.len())
        };

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(levels)
            .child(div().flex_1())
            .child(div().flex_none().text_color(rgb(DIM)).child(counter))
            .child(self.render_button(
                "follow",
                if self.follow_tail {
                    "跟随: 开"
                } else {
                    "跟随: 关"
                },
                self.follow_tail,
                cx.listener(|view, _, _, cx| {
                    view.follow_tail = !view.follow_tail;
                    view.scroll_to_tail();
                    cx.notify();
                }),
            ))
            .child(self.render_button(
                "clear",
                "清空",
                false,
                cx.listener(|view, _, _, cx| {
                    view.buffer.clear();
                    view.reload_entries();
                    cx.notify();
                }),
            ))
    }

    fn render_button(
        &self,
        id: &'static str,
        label: &'static str,
        active: bool,
        on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(if active { ACCENT } else { CHIP_BG }))
            .text_color(rgb(if active { 0xFFFFFF } else { TEXT }))
            .child(label)
            .on_click(on_click)
    }

    fn render_rows(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(rgb(DIM))
                .child("暂无日志")
                .into_any_element();
        }

        uniform_list(
            "log-rows",
            self.rows.len(),
            cx.processor(|view, range: std::ops::Range<usize>, _window, _cx| {
                range
                    .filter_map(|ix| view.rows.get(ix).map(|entry| (ix, entry.clone())))
                    .map(|(ix, entry)| render_row(ix, &entry))
                    .collect()
            }),
        )
        .flex_1()
        .track_scroll(self.scroll.clone())
        .into_any_element()
    }
}

impl Focusable for LogView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for LogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle)
            .key_context("LogWindow")
            .on_action(cx.listener(|view, _: &HideLogWindow, window, cx| {
                view.set_visible(false, cx);
                super::native::hide(window);
            }))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_size(px(12.0))
            .font_family("Consolas")
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_none()
                    .px_3()
                    .py_2()
                    .bg(rgb(HEADER_BG))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(self.render_status())
                    .child(self.render_toolbar(cx)),
            )
            .child(self.render_rows(cx))
    }
}

/// `use<>` because the returned element owns everything it needs; without it
/// Rust 2024 makes the opaque type capture the `entry` borrow.
fn render_row(ix: usize, entry: &LogEntry) -> impl IntoElement + use<> {
    div()
        .id(ix)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .h(px(ROW_HEIGHT))
        .px_3()
        .child(
            div()
                .flex_none()
                .w(px(80.0))
                .text_color(rgb(DIM))
                .child(SharedString::from(entry.clock.clone())),
        )
        .child(
            div()
                .flex_none()
                .w(px(42.0))
                .text_color(level_color(entry.level))
                .child(level_label(entry.level)),
        )
        .child(
            div()
                .flex_none()
                .w(px(140.0))
                .truncate()
                .text_color(rgb(DIM))
                .child(SharedString::from(entry.target.clone())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(single_line(&entry.message)),
        )
}

fn level_color(level: log::Level) -> Hsla {
    rgb(match level {
        log::Level::Error => LEVEL_ERROR,
        log::Level::Warn => LEVEL_WARN,
        log::Level::Info => LEVEL_INFO,
        log::Level::Debug => LEVEL_DEBUG,
        log::Level::Trace => LEVEL_TRACE,
    })
    .into()
}

fn level_label(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "ERROR",
        log::Level::Warn => "WARN",
        log::Level::Info => "INFO",
        log::Level::Debug => "DEBUG",
        log::Level::Trace => "TRACE",
    }
}

/// Rows in a uniform list must all be the same height, so an embedded newline
/// (common in `{:?}`-formatted errors) has to be folded into the line.
fn single_line(message: &str) -> SharedString {
    if message.contains('\n') || message.contains('\r') {
        let folded = message
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ⏎ ");
        SharedString::from(folded)
    } else {
        SharedString::from(message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_leaves_plain_messages_alone() {
        assert_eq!(single_line("relay connected").as_ref(), "relay connected");
    }

    #[test]
    fn single_line_folds_embedded_breaks() {
        let folded = single_line("connect failed\n  caused by: timeout\r\n");

        assert_eq!(folded.as_ref(), "connect failed ⏎ caused by: timeout");
    }
}
