use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    ImagePng,
    File,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientWsMessage {
    Hello {
        client_id: String,
        client_name: String,
        last_seen_sequence: u64,
    },
    PublishInline {
        message_id: String,
        kind: PayloadKind,
        payload_hash: String,
        filename: Option<String>,
        bytes_base64: String,
    },
    PublishR2 {
        message_id: String,
        kind: PayloadKind,
        payload_hash: String,
        filename: Option<String>,
        object_key: String,
        size: usize,
        expires_at: i64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerWsMessage {
    HelloAck { latest_sequence: u64 },
    Message(RelayMessage),
    Error { message: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RelayMessage {
    pub sequence: u64,
    pub source: String,
    pub message_id: String,
    pub kind: PayloadKind,
    pub payload_hash: String,
    pub filename: Option<String>,
    pub payload: RelayPayload,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum RelayPayload {
    Inline {
        bytes_base64: String,
    },
    R2 {
        object_key: String,
        size: usize,
        expires_at: i64,
    },
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

    #[test]
    fn client_ws_message_uses_snake_case_type_tag() {
        let message = ClientWsMessage::Hello {
            client_id: "client-a".to_string(),
            client_name: "Client A".to_string(),
            last_seen_sequence: 7,
        };

        let json = serde_json::to_value(message).unwrap();

        assert_eq!(json["type"], "hello");
        assert_eq!(json["client_id"], "client-a");
        assert_eq!(json["last_seen_sequence"], 7);
    }

    #[test]
    fn relay_payload_serializes_with_inline_mode() {
        let payload = RelayPayload::Inline {
            bytes_base64: "aGVsbG8=".to_string(),
        };

        let json = serde_json::to_value(payload).unwrap();

        assert_eq!(json["mode"], "inline");
        assert_eq!(json["bytes_base64"], "aGVsbG8=");
    }

    #[test]
    fn relay_message_serializes_payload_without_legacy_bytes_field() {
        let message = RelayMessage {
            sequence: 1,
            source: "client-a".to_string(),
            message_id: "message-1".to_string(),
            kind: PayloadKind::Text,
            payload_hash: "hash".to_string(),
            filename: None,
            payload: RelayPayload::Inline {
                bytes_base64: "aGVsbG8=".to_string(),
            },
        };

        let json = serde_json::to_value(message).unwrap();

        assert!(json.get("bytes_base64").is_none());
        assert_eq!(json["payload"]["mode"], "inline");
        assert_eq!(json["payload"]["bytes_base64"], "aGVsbG8=");
    }
}
