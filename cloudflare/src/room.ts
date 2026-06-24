import {
  INLINE_LIMIT_BYTES,
  type ClientMessage,
  type RelayMessage,
  type ServerMessage,
  isValidClientMessage,
} from "./protocol";

const MAX_RECENT_MESSAGES = 100;

type Session = {
  socket: WebSocket;
  clientId: string;
};

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
  private sessions = new Set<Session>();
  private recent: RelayMessage[] = [];
  private nextSequence = 1;

  constructor(
    private state: DurableObjectState,
    private env: unknown,
  ) {}

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    let session: Session | null = null;

    server.addEventListener("message", (event) => {
      try {
        const parsed = JSON.parse(String(event.data)) as unknown;
        if (!isValidClientMessage(parsed)) {
          send(server, { type: "error", message: "invalid message" });
          return;
        }

        if (parsed.type === "hello") {
          session = { socket: server, clientId: parsed.client_id };
          this.sessions.add(session);
          send(server, {
            type: "hello_ack",
            latest_sequence: this.nextSequence - 1,
          });
          const catchup = messagesForClient(
            chooseCatchupMessages(this.recent, parsed.last_seen_sequence),
            parsed.client_id,
          );
          for (const message of catchup) {
            send(server, { type: "message", ...message });
          }
          return;
        }

        if (!session) {
          send(server, { type: "error", message: "hello required" });
          return;
        }

        const message = this.toRelayMessage(session.clientId, parsed);
        this.remember(message);
        this.broadcast(message);
      } catch {
        send(server, { type: "error", message: "invalid json" });
      }
    });

    server.addEventListener("close", () => {
      if (session) {
        this.sessions.delete(session);
      }
    });
    server.addEventListener("error", () => {
      if (session) {
        this.sessions.delete(session);
      }
    });

    return new Response(null, { status: 101, webSocket: client });
  }

  private toRelayMessage(
    source: string,
    message: Exclude<ClientMessage, { type: "hello" }>,
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

  private remember(message: RelayMessage): void {
    this.recent.push(message);
    while (this.recent.length > MAX_RECENT_MESSAGES) {
      this.recent.shift();
    }
  }

  private broadcast(message: RelayMessage): void {
    for (const session of this.sessions) {
      if (session.clientId !== message.source) {
        // A dead/closing socket throws on send; isolate it so one stale peer
        // can't abort delivery to the remaining live sessions, and drop it.
        if (!send(session.socket, { type: "message", ...message })) {
          this.sessions.delete(session);
        }
      }
    }
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
