export const INLINE_LIMIT_BYTES = 10 * 1024 * 1024;
export const R2_LIMIT_BYTES = 100 * 1024 * 1024;
export const DEFAULT_ROOM = "default";

export type PayloadKind = "text" | "image_png" | "file";

export type ClientMessage =
  | {
      type: "hello";
      client_id: string;
      client_name: string;
      last_seen_sequence: number;
    }
  | {
      type: "publish_inline";
      message_id: string;
      kind: PayloadKind;
      payload_hash: string;
      filename?: string;
      bytes_base64: string;
    }
  | {
      type: "publish_r2";
      message_id: string;
      kind: PayloadKind;
      payload_hash: string;
      filename?: string;
      object_key: string;
      size: number;
      expires_at: number;
    };

export type RelayPayload =
  | { mode: "inline"; bytes_base64: string }
  | { mode: "r2"; object_key: string; size: number; expires_at: number };

export type RelayMessage = {
  sequence: number;
  source: string;
  message_id: string;
  kind: PayloadKind;
  payload_hash: string;
  filename?: string;
  payload: RelayPayload;
};

export type ServerMessage =
  | { type: "hello_ack"; latest_sequence: number }
  | ({ type: "message" } & RelayMessage)
  | { type: "error"; message: string };

export function isValidClientMessage(value: unknown): value is ClientMessage {
  if (!value || typeof value !== "object") {
    return false;
  }

  const message = value as Record<string, unknown>;
  if (message.type === "hello") {
    return (
      typeof message.client_id === "string" &&
      typeof message.client_name === "string" &&
      typeof message.last_seen_sequence === "number"
    );
  }

  if (message.type === "publish_inline") {
    return (
      typeof message.message_id === "string" &&
      isPayloadKind(message.kind) &&
      typeof message.payload_hash === "string" &&
      optionalString(message.filename) &&
      typeof message.bytes_base64 === "string"
    );
  }

  if (message.type === "publish_r2") {
    return (
      typeof message.message_id === "string" &&
      isPayloadKind(message.kind) &&
      typeof message.payload_hash === "string" &&
      optionalString(message.filename) &&
      typeof message.object_key === "string" &&
      typeof message.size === "number" &&
      typeof message.expires_at === "number"
    );
  }

  return false;
}

export function isPayloadKind(value: unknown): value is PayloadKind {
  return value === "text" || value === "image_png" || value === "file";
}

export function objectKeyFor(
  room: string,
  messageId: string,
  filename: string,
): string {
  return `rooms/${safeSegment(room)}/messages/${safeSegment(
    messageId,
  )}/${safeSegment(filename)}`;
}

function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

function safeSegment(value: string): string {
  const safe = value.replace(/[^A-Za-z0-9._-]/g, "_");
  return safe.length > 0 ? safe : "unnamed";
}
