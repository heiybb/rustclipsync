use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    ImagePng,
    File,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PushRequest {
    pub client_id: String,
    pub message_id: String,
    pub kind: PayloadKind,
    pub payload_hash: String,
    pub filename: Option<String>,
    pub bytes_base64: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PushResponse {
    pub sequence: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PullResponse {
    pub latest_sequence: u64,
    pub messages: Vec<RelayMessage>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RelayMessage {
    pub sequence: u64,
    pub source: String,
    pub message_id: String,
    pub kind: PayloadKind,
    pub payload_hash: String,
    pub filename: Option<String>,
    pub bytes_base64: String,
}

impl PayloadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PayloadKind::Text => "text",
            PayloadKind::ImagePng => "image_png",
            PayloadKind::File => "file",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn text_push_round_trips_through_json() {
        let request = PushRequest {
            client_id: "client-a".to_string(),
            message_id: "message-1".to_string(),
            kind: PayloadKind::Text,
            payload_hash: "hash".to_string(),
            filename: None,
            bytes_base64: base64::prelude::BASE64_STANDARD.encode("hello"),
        };

        let json = serde_json::to_string(&request).unwrap();
        let decoded: PushRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded.client_id, "client-a");
        assert_eq!(decoded.kind, PayloadKind::Text);
        assert_eq!(decoded.filename, None);
    }

    #[test]
    fn pull_response_tracks_latest_sequence() {
        let response = PullResponse {
            latest_sequence: 42,
            messages: vec![RelayMessage {
                sequence: 42,
                source: "client-a".to_string(),
                message_id: "message-1".to_string(),
                kind: PayloadKind::ImagePng,
                payload_hash: "hash".to_string(),
                filename: None,
                bytes_base64: "abc".to_string(),
            }],
        };

        assert_eq!(response.latest_sequence, 42);
        assert_eq!(response.messages[0].kind, PayloadKind::ImagePng);
    }
}
