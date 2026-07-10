import {
  INLINE_LIMIT_BYTES,
  type ClientMessage,
  type RelayMessage,
  type ServerMessage,
  isValidClientMessage,
} from "./protocol";
import { objectKeyForUpload } from "./r2";

// The relay is a transient hand-off, not an archive: only the latest message
// is retained (the clipboard is a last-write-wins single slot, so older
// entries can never matter), and it moves through two tiers. For the hot
// window after publish the payload sits in Durable Object storage so live
// peers get it instantly; after that an alarm hands inline payloads off to
// R2 (whose lifecycle rule deletes them after a day) and keeps only a small
// pointer here, so clipboard content itself never lingers in the DO.
export const INLINE_HOT_TTL_MS = 60 * 1000;
// When the pointer itself disappears and late clients get nothing. Aligned
// with the bucket's 1-day object expiry rule and the expires_at the client
// stamps on its own R2 uploads.
export const LATEST_RETENTION_MS = 24 * 60 * 60 * 1000;

// Exact strings for the runtime-level heartbeat auto-response. The client
// must send this byte-for-byte (see protocol.rs tests); the runtime answers
// without waking the Durable Object or accruing duration charges.
export const PING_TEXT = '{"type":"ping"}';
export const PONG_TEXT = '{"type":"pong"}';

const SEQUENCE_KEY = "nextSequence";
const LATEST_KEY = "latest";
const SCHEMA_KEY = "schemaVersion";
const SCHEMA_VERSION = 2;

type Attachment = { clientId: string };

export type StoredLatest = {
  message: RelayMessage;
  expiresAt: number;
  // Set while an inline payload is still in the hot window; its presence
  // tells the alarm to archive to R2 rather than purge.
  archiveAt?: number;
};

export function archivedMessage(
  message: RelayMessage,
  objectKey: string,
  size: number,
  expiresAtSecs: number,
): RelayMessage {
  if (message.payload.mode !== "inline") {
    return message;
  }
  return {
    ...message,
    payload: {
      mode: "r2",
      object_key: objectKey,
      size,
      expires_at: expiresAtSecs,
    },
  };
}

export function catchupMessage(
  latest: StoredLatest | undefined,
  lastSeenSequence: number,
  clientId: string,
  now: number,
): RelayMessage | null {
  if (!latest || now >= latest.expiresAt) {
    return null;
  }
  if (latest.message.sequence <= lastSeenSequence) {
    return null;
  }
  if (latest.message.source === clientId) {
    return null;
  }
  return latest.message;
}

type RoomEnv = { OBJECTS: R2Bucket };

export class SyncRoom implements DurableObject {
  private nextSequence = 1;

  constructor(
    private state: DurableObjectState,
    private env: RoomEnv,
  ) {
    // The object hibernates between messages, so the sequence counter must
    // be restored from storage before any event is delivered; otherwise
    // every wake would restart sequences at 1 and break catch-up.
    state.blockConcurrencyWhile(async () => {
      const stored = await state.storage.get<number>([
        SEQUENCE_KEY,
        SCHEMA_KEY,
      ]);
      this.nextSequence = stored.get(SEQUENCE_KEY) ?? 1;
      if ((stored.get(SCHEMA_KEY) ?? 1) < SCHEMA_VERSION) {
        // One-time cleanup of the pre-TTL recent-message window, which kept
        // clipboard content in storage indefinitely.
        const old = await state.storage.list({ prefix: "recent:" });
        if (old.size > 0) {
          await state.storage.delete([...old.keys()]);
        }
        await state.storage.put(SCHEMA_KEY, SCHEMA_VERSION);
      }
    });
    state.setWebSocketAutoResponse(
      new WebSocketRequestResponsePair(PING_TEXT, PONG_TEXT),
    );
  }

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    // Hibernation API: the socket stays connected at the edge while the
    // object is evicted from memory between messages, so an open connection
    // no longer bills duration around the clock.
    this.state.acceptWebSocket(server);
    console.log("websocket accepted");

    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(
    ws: WebSocket,
    data: ArrayBuffer | string,
  ): Promise<void> {
    try {
      const parsed = JSON.parse(String(data)) as unknown;
      if (!isValidClientMessage(parsed)) {
        send(ws, { type: "error", message: "invalid message" });
        return;
      }

      if (parsed.type === "ping") {
        // Normally intercepted by the auto-response pair; answered here too
        // so a ping that differs in formatting still gets a pong.
        send(ws, { type: "pong" });
        return;
      }

      if (parsed.type === "hello") {
        const attachment: Attachment = { clientId: parsed.client_id };
        ws.serializeAttachment(attachment);
        send(ws, {
          type: "hello_ack",
          latest_sequence: this.nextSequence - 1,
        });
        const latest = await this.state.storage.get<StoredLatest>(LATEST_KEY);
        const catchup = catchupMessage(
          latest,
          parsed.last_seen_sequence,
          parsed.client_id,
          Date.now(),
        );
        if (catchup) {
          send(ws, { type: "message", ...catchup });
        }
        console.log(
          `hello: client=${parsed.client_id}, last_seen=${parsed.last_seen_sequence}, catchup=${catchup ? 1 : 0}`,
        );
        return;
      }

      const clientId = attachmentClientId(ws);
      if (!clientId) {
        send(ws, { type: "error", message: "hello required" });
        return;
      }

      const message = this.toRelayMessage(clientId, parsed);
      await this.remember(message);
      this.broadcast(message);
      console.log(
        `publish: client=${clientId}, kind=${message.kind}, seq=${message.sequence}`,
      );
    } catch (err) {
      console.log(`message handling failed: ${err}`);
      send(ws, {
        type: "error",
        message: err instanceof Error ? err.message : "invalid message",
      });
    }
  }

  async webSocketClose(ws: WebSocket, code: number): Promise<void> {
    console.log(
      `websocket closed: client=${attachmentClientId(ws) ?? "unknown"}, code=${code}`,
    );
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    console.log(
      `websocket error: client=${attachmentClientId(ws) ?? "unknown"}, error=${error}`,
    );
  }

  private toRelayMessage(
    source: string,
    message: Exclude<ClientMessage, { type: "hello" } | { type: "ping" }>,
  ): RelayMessage {
    if (message.type === "publish_inline") {
      const decodedLength = Math.floor((message.bytes_base64.length * 3) / 4);
      if (decodedLength > INLINE_LIMIT_BYTES) {
        throw new Error("inline payload too large");
      }
      return {
        sequence: this.nextSequence++,
        source,
        message_id: message.message_id,
        kind: message.kind,
        payload_hash: message.payload_hash,
        filename: message.filename,
        payload: { mode: "inline", bytes_base64: message.bytes_base64 },
      };
    }

    return {
      sequence: this.nextSequence++,
      source,
      message_id: message.message_id,
      kind: message.kind,
      payload_hash: message.payload_hash,
      filename: message.filename,
      payload: {
        mode: "r2",
        object_key: message.object_key,
        size: message.size,
        expires_at: message.expires_at,
      },
    };
  }

  private async remember(message: RelayMessage): Promise<void> {
    const now = Date.now();
    const expiresAt = now + LATEST_RETENTION_MS;
    const latest: StoredLatest =
      message.payload.mode === "inline"
        ? { message, expiresAt, archiveAt: now + INLINE_HOT_TTL_MS }
        : { message, expiresAt };
    await this.state.storage.put(SEQUENCE_KEY, this.nextSequence);
    await this.state.storage.put(LATEST_KEY, latest);
    // Each publish overwrites the previous latest and re-arms the alarm,
    // which is exactly right since only the latest message is retained.
    await this.state.storage.setAlarm(latest.archiveAt ?? latest.expiresAt);
  }

  async alarm(): Promise<void> {
    const latest = await this.state.storage.get<StoredLatest>(LATEST_KEY);
    if (!latest) {
      return;
    }

    if (
      latest.archiveAt !== undefined &&
      latest.message.payload.mode === "inline"
    ) {
      // Hot window over: hand the payload off to R2, where the bucket's
      // lifecycle rule deletes it after a day, and keep only a pointer here
      // so clipboard content stops living in DO storage.
      const key = objectKeyForUpload(
        latest.message.message_id,
        latest.message.filename ?? null,
      );
      const bytes = bytesFromBase64(latest.message.payload.bytes_base64);
      await this.env.OBJECTS.put(key, bytes);

      // The R2 put reopens the input gate, so a publish may have replaced
      // the latest message meanwhile; archiving over it would resurrect
      // stale content.
      const current = await this.state.storage.get<StoredLatest>(LATEST_KEY);
      if (current?.message.sequence !== latest.message.sequence) {
        return;
      }
      const archived: StoredLatest = {
        message: archivedMessage(
          latest.message,
          key,
          bytes.byteLength,
          Math.floor(latest.expiresAt / 1000),
        ),
        expiresAt: latest.expiresAt,
      };
      await this.state.storage.put(LATEST_KEY, archived);
      await this.state.storage.setAlarm(latest.expiresAt);
      console.log(
        `latest inline payload archived to R2: seq=${latest.message.sequence}`,
      );
      return;
    }

    await this.state.storage.delete(LATEST_KEY);
    console.log("expired latest message purged");
  }

  private broadcast(message: RelayMessage): void {
    for (const ws of this.state.getWebSockets()) {
      const clientId = attachmentClientId(ws);
      // Sockets that have not completed hello yet have no attachment; they
      // will receive catch-up when their hello arrives.
      if (!clientId || clientId === message.source) {
        continue;
      }
      // A dead/closing socket throws on send; isolate it so one stale peer
      // can't abort delivery to the remaining live sessions.
      send(ws, { type: "message", ...message });
    }
  }
}

function bytesFromBase64(encoded: string): Uint8Array {
  const binary = atob(encoded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function attachmentClientId(ws: WebSocket): string | null {
  try {
    const attachment = ws.deserializeAttachment() as Attachment | null;
    return attachment?.clientId ?? null;
  } catch {
    return null;
  }
}

function send(socket: WebSocket, message: ServerMessage): boolean {
  try {
    socket.send(JSON.stringify(message));
    return true;
  } catch {
    return false;
  }
}
