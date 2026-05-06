use crate::clipboard::{ClipboardBackend, ClipboardItem, create_backend};
use crate::config::ClientConfig;
use crate::file_transfer::{cleanup_old_received_files, save_received_file};
use crate::network::HttpRelayClient;
use crate::protocol::{PayloadKind, PushRequest, RelayMessage};
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

    loop {
        match relay.pull(&config.client_id, last_seen_sequence).await {
            Ok(response) => {
                last_seen_sequence = response.latest_sequence;
                for message in response.messages {
                    let mut backend = backend.lock().unwrap();
                    apply_remote_message(
                        &config,
                        &mut **backend,
                        message,
                        &recent_ids,
                        &last_local_hash,
                    )?;
                }
            }
            Err(err) => log::warn!("pull failed: {:?}", err),
        }

        tokio::time::sleep(Duration::from_millis(config.remote_poll_interval_ms)).await;
    }
}

fn item_to_push_request(config: &ClientConfig, item: ClipboardItem) -> Result<Option<PushRequest>> {
    let (kind, filename, bytes) = match item {
        ClipboardItem::Text(text) => (PayloadKind::Text, None, text.into_bytes()),
        ClipboardItem::ImagePng(bytes) => (PayloadKind::ImagePng, None, bytes),
        ClipboardItem::FilePath(path) => {
            if !path.is_file() {
                return Ok(None);
            }
            let metadata = std::fs::metadata(&path)?;
            if metadata.len() as usize > config.max_payload_bytes {
                log::warn!("file ignored because it exceeds configured limit");
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

    if bytes.len() > config.max_payload_bytes {
        log::warn!("local payload ignored because it exceeds configured limit");
        return Ok(None);
    }

    let payload_hash = calculate_bytes_hash(&bytes);
    Ok(Some(PushRequest {
        client_id: config.client_id.clone(),
        message_id: Uuid::new_v4().to_string(),
        kind,
        payload_hash,
        filename,
        bytes_base64: BASE64_STANDARD.encode(bytes),
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

    let bytes = BASE64_STANDARD.decode(&message.bytes_base64)?;
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

fn hash_prefix(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_item_becomes_push_request() {
        let config = test_config();
        let request = item_to_push_request(&config, ClipboardItem::Text("hello".to_string()))
            .unwrap()
            .unwrap();

        assert_eq!(request.client_id, "client-a");
        assert_eq!(request.kind, PayloadKind::Text);
        assert_eq!(
            BASE64_STANDARD.decode(request.bytes_base64).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn file_item_becomes_push_request() {
        let root = std::env::temp_dir().join(format!("rustclipsync-local-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let file_path = root.join("sample.txt");
        std::fs::write(&file_path, b"hello").unwrap();

        let config = test_config();
        let request = item_to_push_request(&config, ClipboardItem::FilePath(file_path))
            .unwrap()
            .unwrap();

        assert_eq!(request.kind, PayloadKind::File);
        assert_eq!(request.filename.as_deref(), Some("sample.txt"));
        assert_eq!(
            BASE64_STANDARD.decode(request.bytes_base64).unwrap(),
            b"hello"
        );

        std::fs::remove_dir_all(root).unwrap();
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
}
