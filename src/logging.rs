//! Logging fan-out.
//!
//! The CLI only ever needed stderr, but the GUI has to render the same records
//! in a window, so a single `log::Log` implementation feeds both sinks. The
//! filter decision is made once, by the `env_logger` instance, so what the
//! window shows and what the console shows can never drift apart.

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// How many records the GUI keeps. Each entry is a short line, so a few
/// thousand of them stay well under a megabyte.
pub const DEFAULT_CAPACITY: usize = 4000;

#[derive(Debug)]
pub struct LogEntry {
    /// Wall-clock time of the record, `HH:MM:SS.mmm`. Local time on Windows,
    /// UTC elsewhere (see [`now_clock`]).
    pub clock: String,
    pub level: log::Level,
    pub target: String,
    pub message: String,
}

/// A bounded, thread-safe record buffer that readers poll through
/// [`LogBuffer::revision`].
///
/// Readers live on the UI thread and must never block behind a writer for
/// long, so the lock is only ever held for a push or a pointer-clone of the
/// whole ring; entries themselves are shared by `Arc` and never copied.
pub struct LogBuffer {
    entries: Mutex<VecDeque<Arc<LogEntry>>>,
    capacity: usize,
    /// Bumped on every mutation. A reader that sees an unchanged revision can
    /// skip re-reading (and re-rendering) entirely.
    revision: AtomicU64,
    /// Records evicted because the ring was full, so the UI can say so.
    dropped: AtomicU64,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity: capacity.max(1),
            revision: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn push(&self, entry: LogEntry) {
        {
            let mut entries = self.entries.lock().unwrap_or_else(|err| err.into_inner());
            while entries.len() >= self.capacity {
                entries.pop_front();
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            entries.push_back(Arc::new(entry));
        }
        // Released after the lock so a reader that observes the new revision is
        // guaranteed to see the entry behind it.
        self.revision.fetch_add(1, Ordering::Release);
    }

    /// Clones the current ring. Only the `Arc`s are copied, so this is cheap
    /// enough to call whenever the revision changes.
    pub fn snapshot(&self) -> Vec<Arc<LogEntry>> {
        let entries = self.entries.lock().unwrap_or_else(|err| err.into_inner());
        entries.iter().cloned().collect()
    }

    pub fn clear(&self) {
        {
            let mut entries = self.entries.lock().unwrap_or_else(|err| err.into_inner());
            entries.clear();
        }
        self.dropped.store(0, Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
    }
}

/// Where formatted records go besides the in-memory buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleTarget {
    Stderr,
    /// Used by the GUI binary: with `windows_subsystem = "windows"` there is no
    /// console, and writing to a closed stderr handle would make `log` calls
    /// fail from inside arbitrary code paths.
    Discard,
}

struct FanoutLogger {
    /// Owns the `RUST_LOG` filter *and* the console writer. Asking it whether a
    /// record matches keeps both sinks in agreement, including per-module
    /// directives like `RUST_LOG=rustclipsync=debug,reqwest=warn`.
    console: env_logger::Logger,
    buffer: Option<Arc<LogBuffer>>,
}

impl log::Log for FanoutLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.console.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        if !self.console.matches(record) {
            return;
        }

        self.console.log(record);

        if let Some(buffer) = &self.buffer {
            buffer.push(LogEntry {
                clock: now_clock(),
                level: record.level(),
                target: source_label(record),
                message: record.args().to_string(),
            });
        }
    }

    fn flush(&self) {
        self.console.flush();
    }
}

/// Where a record came from, for the log window's source column.
///
/// Normally the `log` target, but `gpui` emits its internal errors with an
/// empty one: its helper derives the target by splitting the caller's file path
/// on a `crates/` segment, which only exists in Zed's own monorepo layout, so a
/// crates.io build always misses. Those records do still carry file and line,
/// so fall back to the file name rather than showing an unattributable error.
fn source_label(record: &log::Record) -> String {
    let target = record.target();
    if !target.is_empty() {
        return target.to_string();
    }

    match record.file() {
        // Forward slashes too: the path is whatever `file!()` produced on the
        // machine that built the crate.
        Some(file) => file.rsplit(['/', '\\']).next().unwrap_or(file).to_string(),
        None => "unknown".to_string(),
    }
}

static CONSOLE_PROGRESS: AtomicBool = AtomicBool::new(true);

/// Whether transfer progress may be drawn on the terminal.
///
/// A `windows_subsystem = "windows"` process has no stdout or stderr handle, and
/// `println!` *panics* when the write fails, so the progress-bar code has to be
/// able to ask before it draws.
pub fn console_progress() -> bool {
    CONSOLE_PROGRESS.load(Ordering::Relaxed)
}

/// Installs the process-wide logger. Call once, before anything logs.
pub fn init(target: ConsoleTarget, buffer: Option<Arc<LogBuffer>>) -> Result<()> {
    CONSOLE_PROGRESS.store(target == ConsoleTarget::Stderr, Ordering::Relaxed);

    let mut builder =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));

    if target == ConsoleTarget::Discard {
        builder.target(env_logger::Target::Pipe(Box::new(std::io::sink())));
    }

    let console = builder.build();
    log::set_max_level(console.filter());
    log::set_boxed_logger(Box::new(FanoutLogger { console, buffer }))?;

    Ok(())
}

/// `HH:MM:SS.mmm` for the current instant.
///
/// Windows has a system call that already resolves the local time zone and DST,
/// so use it. Everywhere else this is UTC: pulling in a time-zone database just
/// to label log lines is not worth the binary size, and the console sink
/// already prints an absolute UTC timestamp of its own.
#[cfg(windows)]
pub fn now_clock() -> String {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    let mut now: SYSTEMTIME = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut now) };

    format!(
        "{:02}:{:02}:{:02}.{:03}",
        now.wHour, now.wMinute, now.wSecond, now.wMilliseconds
    )
}

#[cfg(not(windows))]
pub fn now_clock() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds_today = now.as_secs() % 86_400;

    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds_today / 3600,
        (seconds_today % 3600) / 60,
        seconds_today % 60,
        now.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(message: &str) -> LogEntry {
        LogEntry {
            clock: "00:00:00.000".to_string(),
            level: log::Level::Info,
            target: "test".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn push_advances_revision() {
        let buffer = LogBuffer::new(8);
        let before = buffer.revision();

        buffer.push(entry("one"));

        assert_ne!(buffer.revision(), before);
        assert_eq!(buffer.snapshot().len(), 1);
    }

    #[test]
    fn ring_evicts_oldest_and_counts_drops() {
        let buffer = LogBuffer::new(2);

        buffer.push(entry("one"));
        buffer.push(entry("two"));
        buffer.push(entry("three"));

        let snapshot = buffer.snapshot();
        let messages: Vec<&str> = snapshot.iter().map(|e| e.message.as_str()).collect();

        assert_eq!(messages, vec!["two", "three"]);
        assert_eq!(buffer.dropped(), 1);
    }

    #[test]
    fn clear_advances_revision_so_readers_refresh() {
        let buffer = LogBuffer::new(4);
        buffer.push(entry("one"));
        let before = buffer.revision();

        buffer.clear();

        assert_ne!(buffer.revision(), before);
        assert!(buffer.snapshot().is_empty());
    }

    #[test]
    fn source_label_prefers_the_log_target() {
        let record = log::Record::builder()
            .target("rustclipsync::sync")
            .file(Some("src/sync.rs"))
            .args(format_args!("x"))
            .build();

        assert_eq!(source_label(&record), "rustclipsync::sync");
    }

    #[test]
    fn source_label_falls_back_to_the_file_name() {
        // What gpui's `log_err()` produces from a crates.io build.
        let record = log::Record::builder()
            .target("")
            .file(Some("C:/src/gpui/src/platform/windows/window.rs"))
            .args(format_args!("x"))
            .build();

        assert_eq!(source_label(&record), "window.rs");
    }

    #[test]
    fn source_label_handles_a_record_with_no_source_at_all() {
        let record = log::Record::builder()
            .target("")
            .args(format_args!("x"))
            .build();

        assert_eq!(source_label(&record), "unknown");
    }

    #[test]
    fn clock_has_millisecond_precision() {
        let clock = now_clock();

        assert_eq!(clock.len(), 12, "unexpected clock format: {clock}");
        assert_eq!(clock.as_bytes()[2], b':');
        assert_eq!(clock.as_bytes()[8], b'.');
    }
}
