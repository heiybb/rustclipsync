import {
  INLINE_LIMIT_BYTES,
  type ClientMessage,
  type RelayMessage,
  type ServerMessage,
  isValidClientMessage,
} from "./protocol";

const MAX_RECENT_MESSAGES = 100;

// Exact strings for the runtime-level heartbeat auto-response. The client
// must send this byte-for-byte (see protocol.rs tests); the runtime answers
// without waking the Durable Object or accruing duration charges.
export const PING_TEXT = '{"type":"ping"}';
export const PONG_TEXT = '{"type":"pong"}';

const SEQUENCE_KEY = "nextSequence";
const RECENT_PREFIX = "recent:";

function recentKey(sequence: number): string {
  return `${RECENT_PREFIX}${String(sequence).padStart(12, "0")}`;
}

type Attachment = { clientId: string };

export function chooseCatchupMessages(
  messages: RelayMessage[],
  lastSeenSequence: number,
): RelayMessage[] {
  if (messages.length === 0) {
    return [];
  }

  const oldest = messages[0].sequence;
  if (lastSeenSequence > 0 && lastSeenSequence >= oldest - 1) {
    return messages.filter((message) => message.sequence > lastSeenSequence);
  }

  return [messages[messages.length - 1]];
}

export function messagesForClient(
  messages: RelayMessage[],
  clientId: string,
): RelayMessage[] {
  return messages.filter((message) => message.source !== clientId);
}

export class SyncRoom implements DurableObject {
  private nextSequence = 1;

  constructor(
    private state: DurableObjectState,
    private env: unknown,
  ) {
    // The object hibernates between messages, so the sequence counter must
    // be restored from storage before any event is delivered; otherwise
    // every wake would restart sequences at 1 and break catch-up.
    state.blockConcurrencyWhile(async () => {
      this.nextSequence = (await state.storage.get<number>(SEQUENCE_KEY)) ?? 1;
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
        const catchup = messagesForClient(
          chooseCatchupMessages(
            await this.loadRecent(),
            parsed.last_seen_sequence,
          ),
          parsed.client_id,
        );
        for (const message of catchup) {
          send(ws, { type: "message", ...message });
        }
        console.log(
          `hello: client=${parsed.client_id}, last_seen=${parsed.last_seen_sequence}, catchup=${catchup.length}`,
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
    await this.state.storage.put(SEQUENCE_KEY, this.nextSequence);
    await this.state.storage.put(recentKey(message.sequence), message);
    // Sequences are dense, so deleting the single key that just fell out of
    // the window keeps storage bounded without a list scan.
    await this.state.storage.delete(
      recentKey(message.sequence - MAX_RECENT_MESSAGES),
    );
  }

  private async loadRecent(): Promise<RelayMessage[]> {
    const entries = await this.state.storage.list<RelayMessage>({
      prefix: RECENT_PREFIX,
    });
    return [...entries.values()];
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
