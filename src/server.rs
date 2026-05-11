use crate::config::ServerConfig;
use crate::protocol::{
    PayloadKind, PullResponse, PushRequest, PushResponse, RelayMessage, RelayPayload,
};
use crate::security::calculate_bytes_hash;
use anyhow::Result;
use axum::extract::DefaultBodyLimit;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::MethodRouter,
    routing::{get, post},
};
use base64::prelude::*;
use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone)]
struct AppState {
    auth_token: String,
    max_payload_bytes: usize,
    queue: Arc<Mutex<MessageQueue>>,
}

struct IncomingMessage {
    source: String,
    message_id: String,
    kind: PayloadKind,
    payload_hash: String,
    filename: Option<String>,
    bytes: Vec<u8>,
}

struct MessageQueue {
    next_sequence: u64,
    max_messages: usize,
    max_bytes: usize,
    queued_bytes: usize,
    messages: VecDeque<RelayMessage>,
}

impl MessageQueue {
    fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            next_sequence: 1,
            max_messages,
            max_bytes,
            queued_bytes: 0,
            messages: VecDeque::new(),
        }
    }

    fn push(&mut self, msg: IncomingMessage) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        let bytes_base64 = BASE64_STANDARD.encode(msg.bytes);
        self.queued_bytes += bytes_base64.len();
        self.messages.push_back(RelayMessage {
            sequence,
            source: msg.source,
            message_id: msg.message_id,
            kind: msg.kind,
            payload_hash: msg.payload_hash,
            filename: msg.filename,
            payload: RelayPayload::Inline { bytes_base64 },
        });
        while self.messages.len() > 1
            && (self.messages.len() > self.max_messages || self.queued_bytes > self.max_bytes)
        {
            if let Some(removed) = self.messages.pop_front() {
                self.queued_bytes = self
                    .queued_bytes
                    .saturating_sub(inline_payload_size(&removed.payload));
            }
        }
        sequence
    }

    fn pull(&self, client_id: &str, after: u64) -> PullResponse {
        let latest_sequence = self.next_sequence.saturating_sub(1);
        let messages = self
            .messages
            .iter()
            .filter(|msg| msg.sequence > after && msg.source != client_id)
            .cloned()
            .collect();
        PullResponse {
            latest_sequence,
            messages,
        }
    }
}

#[derive(Deserialize)]
struct PullQuery {
    client_id: String,
    after: u64,
}

pub async fn run_server(config: ServerConfig) -> Result<()> {
    let state = AppState {
        auth_token: config.auth_token,
        max_payload_bytes: config.max_payload_bytes,
        queue: Arc::new(Mutex::new(MessageQueue::new(
            config.max_queue_messages,
            config.max_queue_bytes,
        ))),
    };
    let push_body_limit = json_body_limit(config.max_payload_bytes);
    let app = Router::new()
        .route("/health", get(health))
        .route("/push", push_route(push_body_limit))
        .route("/pull", get(pull))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    log::info!(
        "HTTP relay listening on {}, max_queue_messages={}, max_queue_bytes={}",
        config.bind_addr,
        config.max_queue_messages,
        config.max_queue_bytes
    );
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PushRequest>,
) -> Result<Json<PushResponse>, StatusCode> {
    authorize(&state, &headers)?;
    let bytes = BASE64_STANDARD
        .decode(&request.bytes_base64)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if bytes.len() > state.max_payload_bytes {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    if calculate_bytes_hash(&bytes) != request.payload_hash {
        return Err(StatusCode::BAD_REQUEST);
    }
    if request.kind == PayloadKind::File && request.filename.as_deref().unwrap_or("").is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut queue = state.queue.lock().await;
    let sequence = queue.push(IncomingMessage {
        source: request.client_id,
        message_id: request.message_id,
        kind: request.kind,
        payload_hash: request.payload_hash,
        filename: request.filename,
        bytes,
    });
    Ok(Json(PushResponse { sequence }))
}

async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PullQuery>,
) -> Result<Json<PullResponse>, StatusCode> {
    authorize(&state, &headers)?;
    let queue = state.queue.lock().await;
    Ok(Json(queue.pull(&query.client_id, query.after)))
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), StatusCode> {
    let expected = format!("Bearer {}", state.auth_token);
    let actual = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if actual == expected {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn push_route(body_limit: usize) -> MethodRouter<AppState> {
    post(push).layer(DefaultBodyLimit::max(body_limit))
}

fn json_body_limit(max_payload_bytes: usize) -> usize {
    (max_payload_bytes * 4).div_ceil(3) + 4096
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PayloadKind;

    #[test]
    fn queue_assigns_sequence_and_excludes_self() {
        let mut queue = MessageQueue::new(100, 1024);
        let msg = IncomingMessage {
            source: "client-a".to_string(),
            message_id: "m1".to_string(),
            kind: PayloadKind::Text,
            payload_hash: calculate_bytes_hash(b"hello"),
            filename: None,
            bytes: b"hello".to_vec(),
        };

        let sequence = queue.push(msg);
        let for_b = queue.pull("client-b", 0);
        let for_a = queue.pull("client-a", 0);

        assert_eq!(sequence, 1);
        assert_eq!(for_b.messages.len(), 1);
        assert_eq!(for_a.messages.len(), 0);
        assert_eq!(for_b.latest_sequence, 1);
    }

    #[test]
    fn queue_evicts_old_messages_by_message_count() {
        let mut queue = MessageQueue::new(2, 1024);

        queue.push(test_message("m1", b"one"));
        queue.push(test_message("m2", b"two"));
        queue.push(test_message("m3", b"three"));

        let response = queue.pull("client-b", 0);
        let ids: Vec<_> = response
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m2", "m3"]);
    }

    #[test]
    fn queue_evicts_old_messages_by_encoded_bytes() {
        let mut queue = MessageQueue::new(100, 8);

        queue.push(test_message("m1", b"abc"));
        queue.push(test_message("m2", b"def"));
        queue.push(test_message("m3", b"ghi"));

        let response = queue.pull("client-b", 0);
        let ids: Vec<_> = response
            .messages
            .iter()
            .map(|message| message.message_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m2", "m3"]);
    }

    #[test]
    fn json_body_limit_allows_base64_overhead() {
        let limit = json_body_limit(10 * 1024 * 1024);
        assert!(limit > 13 * 1024 * 1024);
    }

    fn test_message(message_id: &str, bytes: &[u8]) -> IncomingMessage {
        IncomingMessage {
            source: "client-a".to_string(),
            message_id: message_id.to_string(),
            kind: PayloadKind::Text,
            payload_hash: calculate_bytes_hash(bytes),
            filename: None,
            bytes: bytes.to_vec(),
        }
    }
}

fn inline_payload_size(payload: &RelayPayload) -> usize {
    match payload {
        RelayPayload::Inline { bytes_base64 } => bytes_base64.len(),
        RelayPayload::R2 { .. } => 0,
    }
}
