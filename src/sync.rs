use crate::clipboard::{ClipboardBackend, ClipboardItem, create_backend};
use crate::config::ClientConfig;
use crate::file_transfer::{cleanup_old_received_files, save_received_file};
use crate::network::HttpRelayClient;
use crate::protocol::{PayloadKind, PushRequest, RelayMessage, RelayPayload};
use crate::security::calculate_bytes_hash;
use anyhow::Result;
use base64::Engine;
use base64::prelude::*;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const RECEIVE_FILE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const RECEIVE_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const INLINE_PAYLOAD_LIMIT_BYTES: usize = 10 * 1024 * 1024;
pub const R2_PAYLOAD_LIMIT_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutgoingPayloadRoute {
    Inline,
    R2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPayload {
    pub message_id: String,
    pub kind: PayloadKind,
    pub payload_hash: String,
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
    pub route: OutgoingPayloadRoute,
}

pub async fn run_client(config: ClientConfig) -> Result<()> {
    log::info!(
        "client starting: id={}, name={}, relay={}, receive_dir={}, local_poll_ms={}, remote_poll_ms={}, max_payload_bytes={}",
        config.client_id,
        config.client_name,
        config.server_url,
        config.receive_dir,
        config.poll_interval_ms,
        config.remote_poll_interval_ms,
        config.max_payload_bytes
    );

    let backend = Arc::new(Mutex::new(create_backend()?));
    log::info!(
        "clipboard backend selected: {}",
        backend.lock().unwrap().name()
    );

    let relay = Arc::new(HttpRelayClient::new(&config));
    let recent_ids = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(128)));
    let last_local_hash = Arc::new(Mutex::new(String::new()));
    let receive_cleanup_task =
        tokio::spawn(receive_cleanup_loop(PathBuf::from(&config.receive_dir)));

    let local_task = tokio::spawn(local_push_loop(
        config.clone(),
        backend.clone(),
        relay.clone(),
        last_local_hash.clone(),
    ));
    let remote_task = tokio::spawn(remote_pull_loop(
        config,
        backend,
        relay,
        recent_ids,
        last_local_hash,
    ));

    let _ = tokio::try_join!(local_task, remote_task)?;
    receive_cleanup_task.abort();
    Ok(())
}

async fn receive_cleanup_loop(receive_dir: PathBuf) {
    loop {
        match cleanup_old_received_files(&receive_dir, RECEIVE_FILE_RETENTION, SystemTime::now()) {
            Ok(removed) if removed > 0 => {
                log::info!(
                    "cleaned old received files: dir={}, removed={}",
                    receive_dir.display(),
                    removed
                );
            }
            Ok(_) => {}
            Err(err) => log::warn!("receive cleanup failed: {:?}", err),
        }

        tokio::time::sleep(RECEIVE_CLEANUP_INTERVAL).await;
    }
}

async fn local_push_loop(
    config: ClientConfig,
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    relay: Arc<HttpRelayClient>,
    last_local_hash: Arc<Mutex<String>>,
) -> Result<()> {
    loop {
        let item = {
            let mut backend = backend.lock().unwrap();
            backend.read_snapshot()?
        };

        if let Some(item) = item
            && let Some(request) = item_to_push_request(&config, item)?
        {
            let should_push = *last_local_hash.lock().unwrap() != request.payload_hash;

            if should_push {
                log::info!(
                    "pushing local clipboard update: id={}, kind={}, hash={}",
                    request.message_id,
                    request.kind.as_str(),
                    hash_prefix(&request.payload_hash)
                );
                match relay.push(&request).await {
                    Ok(_) => *last_local_hash.lock().unwrap() = request.payload_hash,
                    Err(err) => log::warn!("push failed: {:?}", err),
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)).await;
    }
}

async fn remote_pull_loop(
    config: ClientConfig,
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    relay: Arc<HttpRelayClient>,
    recent_ids: Arc<Mutex<VecDeque<String>>>,
    last_local_hash: Arc<Mutex<String>>,
) -> Result<()> {
    let mut last_seen_sequence = 0;
    let mut is_initial_pull = true;

    loop {
        match relay.pull(&config.client_id, last_seen_sequence).await {
            Ok(response) => {
                last_seen_sequence = response.latest_sequence;
                for message in messages_for_pull(is_initial_pull, response.messages) {
                    let mut backend = backend.lock().unwrap();
                    apply_remote_message(
                        &config,
                        &mut **backend,
                        message,
                        &recent_ids,
                        &last_local_hash,
                    )?;
                }
                is_initial_pull = false;
            }
            Err(err) => log::warn!("pull failed: {:?}", err),
        }

        tokio::time::sleep(Duration::from_millis(config.remote_poll_interval_ms)).await;
    }
}

fn item_to_push_request(config: &ClientConfig, item: ClipboardItem) -> Result<Option<PushRequest>> {
    let Some(payload) = local_payload_for_item(config, item)? else {
        return Ok(None);
    };
    if payload.route != OutgoingPayloadRoute::Inline {
        log::warn!("local payload requires R2 and cannot be sent through HTTP relay");
        return Ok(None);
    }

    Ok(Some(PushRequest {
        client_id: config.client_id.clone(),
        message_id: payload.message_id,
        kind: payload.kind,
        payload_hash: payload.payload_hash,
        filename: payload.filename,
        bytes_base64: BASE64_STANDARD.encode(payload.bytes),
    }))
}

pub fn local_payload_for_item(
    _config: &ClientConfig,
    item: ClipboardItem,
) -> Result<Option<LocalPayload>> {
    let (kind, filename, bytes) = match item {
        ClipboardItem::Text(text) => (PayloadKind::Text, None, text.into_bytes()),
        ClipboardItem::ImagePng(bytes) => (PayloadKind::ImagePng, None, bytes),
        ClipboardItem::FilePath(path) => {
            if !path.is_file() {
                return Ok(None);
            }
            let metadata = std::fs::metadata(&path)?;
            if metadata.len() > R2_PAYLOAD_LIMIT_BYTES as u64 {
                log::warn!("file ignored because it exceeds Cloudflare R2 payload limit");
                return Ok(None);
            }
            let bytes = std::fs::read(&path)?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string);
            (PayloadKind::File, filename, bytes)
        }
    };

    if bytes.len() > R2_PAYLOAD_LIMIT_BYTES {
        log::warn!("local payload ignored because it exceeds Cloudflare R2 payload limit");
        return Ok(None);
    }

    let route = if bytes.len() <= INLINE_PAYLOAD_LIMIT_BYTES {
        OutgoingPayloadRoute::Inline
    } else {
        OutgoingPayloadRoute::R2
    };
    let payload_hash = calculate_bytes_hash(&bytes);

    Ok(Some(LocalPayload {
        message_id: Uuid::new_v4().to_string(),
        kind,
        payload_hash,
        filename,
        bytes,
        route,
    }))
}

fn apply_remote_message(
    config: &ClientConfig,
    backend: &mut dyn ClipboardBackend,
    message: RelayMessage,
    recent_ids: &Arc<Mutex<VecDeque<String>>>,
    last_local_hash: &Arc<Mutex<String>>,
) -> Result<()> {
    if message.source == config.client_id {
        return Ok(());
    }

    {
        let mut ids = recent_ids.lock().unwrap();
        if ids.contains(&message.message_id) {
            return Ok(());
        }
        ids.push_back(message.message_id.clone());
        while ids.len() > 128 {
            ids.pop_front();
        }
    }

    let bytes = match &message.payload {
        RelayPayload::Inline { bytes_base64 } => BASE64_STANDARD.decode(bytes_base64)?,
        RelayPayload::R2 { .. } => {
            log::warn!("remote R2 payload cannot be applied through HTTP polling client");
            return Ok(());
        }
    };
    if bytes.len() > config.max_payload_bytes {
        log::warn!("remote payload exceeds configured limit");
        return Ok(());
    }
    let payload_hash = calculate_bytes_hash(&bytes);
    if payload_hash != message.payload_hash {
        log::warn!("remote payload hash mismatch");
        return Ok(());
    }

    match message.kind {
        PayloadKind::Text => {
            let text = String::from_utf8(bytes)?;
            let size = text.len();
            backend.write_item(ClipboardItem::Text(text))?;
            *last_local_hash.lock().unwrap() = payload_hash;
            log::info!(
                "applied remote text: source={}, size={}, hash={}",
                message.source,
                size,
                hash_prefix(&message.payload_hash)
            );
        }
        PayloadKind::ImagePng => {
            let size = bytes.len();
            backend.write_item(ClipboardItem::ImagePng(bytes))?;
            *last_local_hash.lock().unwrap() = payload_hash;
            log::info!(
                "applied remote image: source={}, size={}, hash={}",
                message.source,
                size,
                hash_prefix(&message.payload_hash)
            );
        }
        PayloadKind::File => {
            let filename = message
                .filename
                .ok_or_else(|| anyhow::anyhow!("missing filename"))?;
            let path = save_received_file(Path::new(&config.receive_dir), &filename, &bytes)?;
            log::info!(
                "saved remote file: source={}, path={}, size={}, hash={}",
                message.source,
                path.display(),
                bytes.len(),
                hash_prefix(&message.payload_hash)
            );
        }
    }

    Ok(())
}

fn messages_for_pull(is_initial_pull: bool, messages: Vec<RelayMessage>) -> Vec<RelayMessage> {
    if is_initial_pull {
        messages.into_iter().next_back().into_iter().collect()
    } else {
        messages
    }
}

fn hash_prefix(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_payload_uses_inline_route() {
        let config = test_config();
        let payload = local_payload_for_item(&config, ClipboardItem::Text("hello".to_string()))
            .unwrap()
            .unwrap();

        assert_eq!(payload.kind, PayloadKind::Text);
        assert_eq!(payload.bytes, b"hello");
        assert_eq!(payload.route, OutgoingPayloadRoute::Inline);
    }

    #[test]
    fn ten_mb_file_uses_inline_route() {
        let (_root, file_path) = sparse_test_file(INLINE_PAYLOAD_LIMIT_BYTES as u64);
        let config = test_config();
        let payload = local_payload_for_item(&config, ClipboardItem::FilePath(file_path))
            .unwrap()
            .unwrap();

        assert_eq!(payload.kind, PayloadKind::File);
        assert_eq!(payload.filename.as_deref(), Some("sample.bin"));
        assert_eq!(payload.bytes.len(), INLINE_PAYLOAD_LIMIT_BYTES);
        assert_eq!(payload.route, OutgoingPayloadRoute::Inline);
    }

    #[test]
    fn file_above_ten_mb_uses_r2_route() {
        let (_root, file_path) = sparse_test_file((INLINE_PAYLOAD_LIMIT_BYTES + 1) as u64);
        let config = test_config();
        let payload = local_payload_for_item(&config, ClipboardItem::FilePath(file_path))
            .unwrap()
            .unwrap();

        assert_eq!(payload.kind, PayloadKind::File);
        assert_eq!(payload.bytes.len(), INLINE_PAYLOAD_LIMIT_BYTES + 1);
        assert_eq!(payload.route, OutgoingPayloadRoute::R2);
    }

    #[test]
    fn file_above_one_hundred_mb_is_rejected() {
        let (_root, file_path) = sparse_test_file((R2_PAYLOAD_LIMIT_BYTES + 1) as u64);
        let config = test_config();
        let payload = local_payload_for_item(&config, ClipboardItem::FilePath(file_path)).unwrap();

        assert!(payload.is_none());
    }

    #[test]
    fn missing_file_path_returns_none() {
        let config = test_config();
        let missing_path =
            std::env::temp_dir().join(format!("rustclipsync-missing-{}", Uuid::new_v4()));
        let payload =
            local_payload_for_item(&config, ClipboardItem::FilePath(missing_path)).unwrap();

        assert!(payload.is_none());
    }

    #[test]
    fn initial_pull_applies_only_latest_remote_message() {
        let messages = vec![
            test_relay_message(1, "m1"),
            test_relay_message(2, "m2"),
            test_relay_message(3, "m3"),
        ];

        let selected = messages_for_pull(true, messages);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].sequence, 3);
        assert_eq!(selected[0].message_id, "m3");
    }

    #[test]
    fn incremental_pull_applies_all_remote_messages() {
        let messages = vec![test_relay_message(4, "m4"), test_relay_message(5, "m5")];

        let selected = messages_for_pull(false, messages);

        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].sequence, 4);
        assert_eq!(selected[1].sequence, 5);
    }

    fn test_config() -> ClientConfig {
        ClientConfig {
            server_url: "http://127.0.0.1:7878".to_string(),
            client_id: "client-a".to_string(),
            client_name: "Client A".to_string(),
            auth_token: "secret".to_string(),
            poll_interval_ms: 300,
            remote_poll_interval_ms: 500,
            receive_dir: "receive".to_string(),
            max_payload_bytes: 10 * 1024 * 1024,
        }
    }

    fn test_relay_message(sequence: u64, message_id: &str) -> RelayMessage {
        RelayMessage {
            sequence,
            source: "client-b".to_string(),
            message_id: message_id.to_string(),
            kind: PayloadKind::Text,
            payload_hash: calculate_bytes_hash(message_id.as_bytes()),
            filename: None,
            payload: RelayPayload::Inline {
                bytes_base64: BASE64_STANDARD.encode(message_id),
            },
        }
    }

    fn sparse_test_file(len: u64) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("rustclipsync-local-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("sample.bin");
        let file = std::fs::File::create(&file_path).unwrap();
        file.set_len(len).unwrap();
        (root, file_path)
    }
}
