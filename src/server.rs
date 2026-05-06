use crate::config::ServerConfig;
use crate::protocol::{PayloadKind, PullResponse, PushRequest, PushResponse, RelayMessage};
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
    max_len: usize,
    messages: VecDeque<RelayMessage>,
}

impl MessageQueue {
    fn new(max_len: usize) -> Self {
        Self {
            next_sequence: 1,
            max_len,
            messages: VecDeque::new(),
        }
    }

    fn push(&mut self, msg: IncomingMessage) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.messages.push_back(RelayMessage {
            sequence,
            source: msg.source,
            message_id: msg.message_id,
            kind: msg.kind,
            payload_hash: msg.payload_hash,
            filename: msg.filename,
            bytes_base64: BASE64_STANDARD.encode(msg.bytes),
        });
        while self.messages.len() > self.max_len {
            self.messages.pop_front();
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
        queue: Arc::new(Mutex::new(MessageQueue::new(1000))),
    };
    let push_body_limit = json_body_limit(config.max_payload_bytes);
    let app = Router::new()
        .route("/health", get(health))
        .route("/push", push_route(push_body_limit))
        .route("/pull", get(pull))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    log::info!("HTTP relay listening on {}", config.bind_addr);
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
        let mut queue = MessageQueue::new(100);
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
    fn json_body_limit_allows_base64_overhead() {
        let limit = json_body_limit(10 * 1024 * 1024);
        assert!(limit > 13 * 1024 * 1024);
    }
}
