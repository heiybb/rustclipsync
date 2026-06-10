use crate::clipboard::{ClipboardBackend, ClipboardItem, ClipboardWatcher, create_backend};
use crate::cloudflare::CloudflareRelay;
use crate::config::ClientConfig;
use crate::file_transfer::{cleanup_old_received_files, save_received_file};
use crate::protocol::{ClientWsMessage, PayloadKind, RelayMessage, RelayPayload, ServerWsMessage};
use crate::security::calculate_bytes_hash;
use anyhow::Result;
use base64::Engine;
use base64::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use uuid::Uuid;

const RECEIVE_FILE_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const RECEIVE_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const INLINE_PAYLOAD_LIMIT_BYTES: usize = 10 * 1024;
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
        "client starting: id={}, name={}, relay={}, receive_dir={}, local_poll_ms={}, max_inline_bytes={}, max_r2_bytes={}",
        config.client_id,
        config.client_name,
        config.server_url,
        config.receive_dir,
        config.poll_interval_ms,
        INLINE_PAYLOAD_LIMIT_BYTES,
        config.max_payload_bytes
    );

    let (backend, watcher) = create_backend()?;
    let backend = Arc::new(Mutex::new(backend));
    let watcher = Arc::new(Mutex::new(watcher));
    log::info!(
        "clipboard backend selected: {}",
        backend.lock().unwrap().name()
    );

    let last_local_hash = Arc::new(Mutex::new(String::new()));
    let _receive_cleanup_task =
        tokio::spawn(receive_cleanup_loop(PathBuf::from(&config.receive_dir)));
    let relay = Arc::new(CloudflareRelay::new(config.clone()));
    let mut last_seen_sequence = 0;

    let (local_tx, mut local_rx) = mpsc::channel::<LocalPayload>(1);
    let mut local_send_task = tokio::spawn(local_send_loop(
        config.clone(),
        backend.clone(),
        watcher.clone(),
        local_tx,
        last_local_hash.clone(),
    ));

    loop {
        match relay.connect(last_seen_sequence).await {
            Ok((outgoing, incoming)) => {
                let forwarder_fut = async {
                    while let Some(payload) = local_rx.recv().await {
                        send_payload(&relay, &outgoing, payload).await?;
                    }
                    Ok::<(), anyhow::Error>(())
                };

                let receiver_fut = remote_receive_loop(
                    config.clone(),
                    backend.clone(),
                    relay.clone(),
                    incoming,
                    last_local_hash.clone(),
                    &mut last_seen_sequence,
                );

                tokio::select! {
                    res = forwarder_fut => {
                        if let Err(err) = res {
                            log::warn!("local forwarder failed: {:?}", err);
                        }
                    }
                    res = receiver_fut => {
                        if let Err(err) = res {
                            log::warn!("remote receiver failed: {:?}", err);
                        }
                    }
                    res = &mut local_send_task => {
                        match res {
                            Ok(Ok(())) => log::info!("local send task exited normally"),
                            Ok(Err(err)) => log::error!("local send task failed: {:?}", err),
                            Err(err) => log::error!("local send task panicked: {:?}", err),
                        }
                        break;
                    }
                }
            }
            Err(err) => log::warn!("websocket connect failed: {:?}", err),
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    local_send_task.abort();

    #[allow(unreachable_code)]
    {
        Ok(())
    }
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

async fn local_send_loop(
    config: ClientConfig,
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    watcher: Arc<Mutex<Box<dyn ClipboardWatcher>>>,
    local_tx: mpsc::Sender<LocalPayload>,
    last_local_hash: Arc<Mutex<String>>,
) -> Result<()> {
    loop {
        let item = {
            let mut backend = backend.lock().unwrap();
            backend.read_snapshot()?
        };

        if let Some(item) = item
            && let Some(payload) = local_payload_for_item(&config, item)?
        {
            let should_push = *last_local_hash.lock().unwrap() != payload.payload_hash;

            if should_push {
                log::info!(
                    "pushing local clipboard update: id={}, kind={}, hash={}",
                    payload.message_id,
                    payload.kind.as_str(),
                    hash_prefix(&payload.payload_hash)
                );
                let payload_hash = payload.payload_hash.clone();
                if local_tx.send(payload).await.is_err() {
                    break;
                }
                *last_local_hash.lock().unwrap() = payload_hash;
            }
        }

        let watcher = watcher.clone();
        tokio::task::spawn_blocking(move || watcher.lock().unwrap().wait_for_change()).await??;
    }
    Ok(())
}

async fn send_payload(
    relay: &CloudflareRelay,
    outgoing: &mpsc::Sender<ClientWsMessage>,
    payload: LocalPayload,
) -> Result<()> {
    match payload.route {
        OutgoingPayloadRoute::Inline => {
            outgoing
                .send(ClientWsMessage::PublishInline {
                    message_id: payload.message_id,
                    kind: payload.kind,
                    payload_hash: payload.payload_hash,
                    filename: payload.filename,
                    bytes_base64: BASE64_STANDARD.encode(payload.bytes),
                })
                .await?;
        }
        OutgoingPayloadRoute::R2 => {
            let size = payload.bytes.len();
            let object_key = relay
                .upload_object(
                    &payload.message_id,
                    payload.filename.as_deref(),
                    payload.bytes,
                )
                .await?;
            outgoing
                .send(ClientWsMessage::PublishR2 {
                    message_id: payload.message_id,
                    kind: payload.kind,
                    payload_hash: payload.payload_hash,
                    filename: payload.filename,
                    object_key,
                    size,
                    expires_at: expires_at_24h(),
                })
                .await?;
        }
    }

    Ok(())
}

async fn remote_receive_loop(
    config: ClientConfig,
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    relay: Arc<CloudflareRelay>,
    mut incoming: mpsc::Receiver<ServerWsMessage>,
    last_local_hash: Arc<Mutex<String>>,
    last_seen_sequence: &mut u64,
) -> Result<()> {
    while let Some(message) = incoming.recv().await {
        match message {
            ServerWsMessage::HelloAck { latest_sequence } => {
                *last_seen_sequence = latest_sequence;
            }
            ServerWsMessage::Error { message } => {
                log::warn!("relay error: {}", message);
            }
            ServerWsMessage::Message(message) => {
                *last_seen_sequence = message.sequence;
                apply_remote_message(&config, &relay, backend.clone(), message, &last_local_hash)
                    .await?;
            }
        }
    }

    Ok(())
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

async fn apply_remote_message(
    config: &ClientConfig,
    relay: &CloudflareRelay,
    backend: Arc<Mutex<Box<dyn ClipboardBackend>>>,
    message: RelayMessage,
    last_local_hash: &Arc<Mutex<String>>,
) -> Result<()> {
    if message.source == config.client_id {
        return Ok(());
    }

    let bytes = bytes_from_message(relay, &message).await?;
    let Some(bytes) = validate_remote_bytes(&message, bytes) else {
        return Ok(());
    };
    let payload_hash = message.payload_hash.clone();

    let mut backend = backend.lock().unwrap();
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

async fn bytes_from_message(relay: &CloudflareRelay, message: &RelayMessage) -> Result<Vec<u8>> {
    match &message.payload {
        RelayPayload::Inline { bytes_base64 } => Ok(BASE64_STANDARD.decode(bytes_base64)?),
        RelayPayload::R2 { .. } => {
            relay
                .download_object(&message.message_id, message.filename.as_deref())
                .await
        }
    }
}

#[cfg(test)]
fn bytes_from_inline_message(message: &RelayMessage) -> Result<Option<Vec<u8>>> {
    match &message.payload {
        RelayPayload::Inline { bytes_base64 } => {
            let bytes = BASE64_STANDARD.decode(bytes_base64)?;
            Ok(validate_remote_bytes(message, bytes))
        }
        RelayPayload::R2 { .. } => Ok(None),
    }
}

fn validate_remote_bytes(message: &RelayMessage, bytes: Vec<u8>) -> Option<Vec<u8>> {
    let payload_hash = calculate_bytes_hash(&bytes);
    if payload_hash != message.payload_hash {
        log::warn!("remote payload hash mismatch");
        return None;
    }
    Some(bytes)
}

fn expires_at_24h() -> i64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    now + 24 * 60 * 60
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
    fn hundred_kb_file_uses_inline_route() {
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
    fn file_above_hundred_kb_uses_r2_route() {
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
    fn inline_text_message_decodes_to_bytes() {
        let message = RelayMessage {
            sequence: 1,
            source: "client-b".to_string(),
            message_id: "m1".to_string(),
            kind: PayloadKind::Text,
            payload_hash: calculate_bytes_hash(b"hello"),
            filename: None,
            payload: RelayPayload::Inline {
                bytes_base64: BASE64_STANDARD.encode(b"hello"),
            },
        };

        let bytes = bytes_from_inline_message(&message).unwrap().unwrap();

        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn hash_mismatch_is_rejected_before_apply() {
        let message = RelayMessage {
            sequence: 1,
            source: "client-b".to_string(),
            message_id: "m1".to_string(),
            kind: PayloadKind::Text,
            payload_hash: "bad".to_string(),
            filename: None,
            payload: RelayPayload::Inline {
                bytes_base64: BASE64_STANDARD.encode(b"hello"),
            },
        };

        assert!(bytes_from_inline_message(&message).unwrap().is_none());
    }

    fn test_config() -> ClientConfig {
        ClientConfig {
            server_url: "http://127.0.0.1:7878".to_string(),
            client_id: "client-a".to_string(),
            client_name: "Client A".to_string(),
            auth_token: "secret".to_string(),
            poll_interval_ms: 300,
            receive_dir: "receive".to_string(),
            max_payload_bytes: R2_PAYLOAD_LIMIT_BYTES,
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
