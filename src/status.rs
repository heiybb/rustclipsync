//! Connection state published by the sync loop for a UI to read.
//!
//! The CLI ignores this entirely; it exists so the tray tooltip and the log
//! window can show whether the relay is actually connected without having to
//! scrape log lines for it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Process is up but the sync loop has not been started (or was stopped).
    Idle,
    Connecting,
    Connected,
    /// Connection dropped or refused; a retry is scheduled.
    Retrying,
    /// The sync loop gave up. Needs a restart to recover.
    Failed,
}

impl SyncState {
    pub fn label(self) -> &'static str {
        match self {
            SyncState::Idle => "未启动",
            SyncState::Connecting => "连接中",
            SyncState::Connected => "已连接",
            SyncState::Retrying => "重连中",
            SyncState::Failed => "已停止",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusSnapshot {
    pub state: SyncState,
    /// Free-form context for the state, e.g. the retry delay or an error.
    pub detail: String,
}

pub struct SyncStatus {
    snapshot: Mutex<StatusSnapshot>,
    revision: AtomicU64,
}

impl SyncStatus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(StatusSnapshot {
                state: SyncState::Idle,
                detail: String::new(),
            }),
            revision: AtomicU64::new(0),
        })
    }

    pub fn set(&self, state: SyncState, detail: impl Into<String>) {
        {
            let mut snapshot = self.snapshot.lock().unwrap_or_else(|err| err.into_inner());
            snapshot.state = state;
            snapshot.detail = detail.into();
        }
        self.revision.fetch_add(1, Ordering::Release);
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_idle() {
        let status = SyncStatus::new();

        assert_eq!(status.snapshot().state, SyncState::Idle);
    }

    #[test]
    fn set_advances_revision() {
        let status = SyncStatus::new();
        let before = status.revision();

        status.set(SyncState::Connected, "relay");

        assert_ne!(status.revision(), before);
        assert_eq!(status.snapshot().state, SyncState::Connected);
        assert_eq!(status.snapshot().detail, "relay");
    }
}
