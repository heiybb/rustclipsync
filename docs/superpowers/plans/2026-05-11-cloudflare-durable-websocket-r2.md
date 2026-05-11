# Cloudflare Durable Objects WebSocket + R2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the local HTTP polling relay with a Cloudflare Worker relay using Durable Objects, WebSocket, and R2.

**Architecture:** The Rust binary becomes a Cloudflare-only clipboard client. A TypeScript Worker accepts WebSocket connections, routes them to a Durable Object room, stores large payloads in R2, and broadcasts message metadata or inline payloads to connected clients. Payloads up to 10 MB are sent inline over WebSocket; payloads above 10 MB and up to 100 MB are uploaded to R2; larger payloads are rejected locally.

**Tech Stack:** Rust 2024, Tokio, reqwest, tokio-tungstenite, serde, TypeScript, Cloudflare Workers, Durable Objects, R2, Wrangler/Vitest.

---

## File Structure

- `Cargo.toml`: remove server-only dependencies after migration; add WebSocket dependency.
- `src/main.rs`: remove server dispatch; run the client directly.
- `src/config.rs`: remove `AppConfig::Server`, `ServerConfig`, polling intervals, and server CLI usage.
- `src/protocol.rs`: replace HTTP relay request/response structs with Cloudflare WebSocket protocol structs.
- `src/cloudflare.rs`: new Rust transport module for WebSocket connection, reconnect, R2 upload/download, and message send/receive.
- `src/sync.rs`: keep clipboard polling and apply logic, but replace HTTP push/pull loops with WebSocket sender/receiver tasks.
- `src/network.rs`: delete after the Rust client no longer uses HTTP polling.
- `src/server.rs`: delete after the Cloudflare Worker becomes the only relay.
- `README.md`: replace server instructions with Cloudflare deployment and client usage.
- `cloudflare/package.json`: Worker project scripts and dev dependencies.
- `cloudflare/wrangler.toml`: Worker bindings for Durable Object and R2.
- `cloudflare/src/index.ts`: Worker entrypoint and HTTP routing.
- `cloudflare/src/room.ts`: Durable Object room state, WebSocket lifecycle, sequence assignment, broadcast, and cleanup alarms.
- `cloudflare/src/protocol.ts`: Worker-side protocol types and validation helpers.
- `cloudflare/src/r2.ts`: R2 key construction, object upload/download, and cleanup helpers.
- `cloudflare/test/*.test.ts`: Worker unit tests.

---

### Task 1: Rust Protocol And Payload Routing

**Files:**
- Modify: `src/protocol.rs`
- Modify: `src/sync.rs`

- [ ] **Step 1: Write failing tests for payload routing**

Add these tests to `src/sync.rs` test module:

```rust
#[test]
fn text_payload_uses_inline_route() {
    let config = test_config();
    let payload = local_payload_for_item(&config, ClipboardItem::Text("hello".to_string()))
        .unwrap()
        .unwrap();

    assert_eq!(payload.route, OutgoingPayloadRoute::Inline);
    assert_eq!(payload.kind, PayloadKind::Text);
    assert_eq!(payload.bytes, b"hello");
}

#[test]
fn ten_mb_file_uses_inline_route() {
    let root = std::env::temp_dir().join(format!("rustclipsync-route-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("ten.bin");
    std::fs::write(&path, vec![b'a'; INLINE_PAYLOAD_LIMIT_BYTES]).unwrap();

    let config = test_config();
    let payload = local_payload_for_item(&config, ClipboardItem::FilePath(path)).unwrap().unwrap();

    assert_eq!(payload.route, OutgoingPayloadRoute::Inline);
    assert_eq!(payload.bytes.len(), INLINE_PAYLOAD_LIMIT_BYTES);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn file_above_ten_mb_uses_r2_route() {
    let root = std::env::temp_dir().join(format!("rustclipsync-route-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("large.bin");
    std::fs::write(&path, vec![b'a'; INLINE_PAYLOAD_LIMIT_BYTES + 1]).unwrap();

    let config = test_config();
    let payload = local_payload_for_item(&config, ClipboardItem::FilePath(path)).unwrap().unwrap();

    assert_eq!(payload.route, OutgoingPayloadRoute::R2);
    assert_eq!(payload.bytes.len(), INLINE_PAYLOAD_LIMIT_BYTES + 1);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn file_above_one_hundred_mb_is_rejected() {
    let root = std::env::temp_dir().join(format!("rustclipsync-route-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("too-large.bin");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len((R2_PAYLOAD_LIMIT_BYTES + 1) as u64).unwrap();

    let config = test_config();
    let payload = local_payload_for_item(&config, ClipboardItem::FilePath(path)).unwrap();

    assert!(payload.is_none());

    std::fs::remove_dir_all(root).unwrap();
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```powershell
cargo test payload_uses route_is rejected
```

If the command filter is too narrow, run:

```powershell
cargo test sync::tests
```

Expected: compilation fails because `local_payload_for_item`, `OutgoingPayloadRoute`, `INLINE_PAYLOAD_LIMIT_BYTES`, and `R2_PAYLOAD_LIMIT_BYTES` do not exist.

- [ ] **Step 3: Implement protocol types**

Replace `src/protocol.rs` contents with protocol models for the Cloudflare transport:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadKind {
    Text,
    ImagePng,
    File,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PayloadMode {
    Inline,
    R2,
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
    Inline { bytes_base64: String },
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
```

- [ ] **Step 4: Implement payload routing in `src/sync.rs`**

Add constants and the local payload model near the top of `src/sync.rs`:

```rust
pub const INLINE_PAYLOAD_LIMIT_BYTES: usize = 10 * 1024 * 1024;
pub const R2_PAYLOAD_LIMIT_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutgoingPayloadRoute {
    Inline,
    R2,
}

#[derive(Debug)]
struct LocalPayload {
    message_id: String,
    kind: PayloadKind,
    payload_hash: String,
    filename: Option<String>,
    bytes: Vec<u8>,
    route: OutgoingPayloadRoute,
}
```

Replace `item_to_push_request` with `local_payload_for_item`:

```rust
fn local_payload_for_item(config: &ClientConfig, item: ClipboardItem) -> Result<Option<LocalPayload>> {
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
            let filename = path.file_name().and_then(|name| name.to_str()).map(str::to_string);
            (PayloadKind::File, filename, bytes)
        }
    };

    let route = if bytes.len() <= INLINE_PAYLOAD_LIMIT_BYTES {
        OutgoingPayloadRoute::Inline
    } else if bytes.len() <= R2_PAYLOAD_LIMIT_BYTES {
        OutgoingPayloadRoute::R2
    } else {
        log::warn!("local payload ignored because it exceeds 100 MB Cloudflare relay limit");
        return Ok(None);
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
```

Set `ClientConfig.max_payload_bytes` to `R2_PAYLOAD_LIMIT_BYTES` in tests after Task 2 updates config, or keep the existing 10 MB config check removed in this function.

- [ ] **Step 5: Run routing tests**

Run:

```powershell
cargo test sync::tests
```

Expected: routing tests pass; older tests that reference `PushRequest` fail until later tasks remove or rewrite them.

- [ ] **Step 6: Commit**

```powershell
git add -- src\protocol.rs src\sync.rs
git commit -m "refactor: add cloudflare payload routing protocol"
```

---

### Task 2: Cloudflare Worker Project Skeleton

**Files:**
- Create: `cloudflare/package.json`
- Create: `cloudflare/tsconfig.json`
- Create: `cloudflare/wrangler.toml`
- Create: `cloudflare/src/protocol.ts`
- Create: `cloudflare/src/r2.ts`
- Create: `cloudflare/src/index.ts`
- Create: `cloudflare/test/protocol.test.ts`

- [ ] **Step 1: Write Worker protocol tests**

Create `cloudflare/test/protocol.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { INLINE_LIMIT_BYTES, R2_LIMIT_BYTES, isValidClientMessage, objectKeyFor } from "../src/protocol";

describe("protocol limits", () => {
  it("uses the confirmed Cloudflare payload limits", () => {
    expect(INLINE_LIMIT_BYTES).toBe(10 * 1024 * 1024);
    expect(R2_LIMIT_BYTES).toBe(100 * 1024 * 1024);
  });

  it("accepts hello messages with client identity", () => {
    expect(isValidClientMessage({
      type: "hello",
      client_id: "RYZEN",
      client_name: "RYZEN",
      last_seen_sequence: 0,
    })).toBe(true);
  });

  it("builds deterministic R2 keys", () => {
    expect(objectKeyFor("default", "message-1", "sample.txt"))
      .toBe("rooms/default/messages/message-1/sample.txt");
  });
});
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```powershell
pnpm --dir cloudflare test
```

Expected: fails because the Worker project does not exist.

- [ ] **Step 3: Create package and TypeScript config**

Create `cloudflare/package.json`:

```json
{
  "name": "rustclipsync-cloudflare-relay",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "wrangler dev",
    "deploy": "wrangler deploy",
    "test": "vitest run",
    "typecheck": "tsc --noEmit"
  },
  "devDependencies": {
    "@cloudflare/workers-types": "^4.20260509.0",
    "typescript": "^5.9.0",
    "vitest": "^3.1.0",
    "wrangler": "^4.14.0"
  }
}
```

Create `cloudflare/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ES2022",
    "moduleResolution": "Bundler",
    "strict": true,
    "types": ["@cloudflare/workers-types", "vitest/globals"],
    "noEmit": true
  },
  "include": ["src/**/*.ts", "test/**/*.ts"]
}
```

- [ ] **Step 4: Create Wrangler config**

Create `cloudflare/wrangler.toml`:

```toml
name = "rustclipsync-relay"
main = "src/index.ts"
compatibility_date = "2026-05-11"

[[durable_objects.bindings]]
name = "ROOM"
class_name = "SyncRoom"

[[migrations]]
tag = "v1"
new_sqlite_classes = ["SyncRoom"]

[[r2_buckets]]
binding = "OBJECTS"
bucket_name = "rustclipsync-objects"
```

- [ ] **Step 5: Create Worker protocol helpers**

Create `cloudflare/src/protocol.ts`:

```ts
export const INLINE_LIMIT_BYTES = 10 * 1024 * 1024;
export const R2_LIMIT_BYTES = 100 * 1024 * 1024;
export const DEFAULT_ROOM = "default";

export type PayloadKind = "text" | "image_png" | "file";

export type ClientMessage =
  | { type: "hello"; client_id: string; client_name: string; last_seen_sequence: number }
  | { type: "publish_inline"; message_id: string; kind: PayloadKind; payload_hash: string; filename?: string; bytes_base64: string }
  | { type: "publish_r2"; message_id: string; kind: PayloadKind; payload_hash: string; filename?: string; object_key: string; size: number; expires_at: number };

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
  | { type: "message"; sequence: number; source: string; message_id: string; kind: PayloadKind; payload_hash: string; filename?: string; payload: RelayPayload }
  | { type: "error"; message: string };

export function isValidClientMessage(value: unknown): value is ClientMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Record<string, unknown>;
  if (message.type === "hello") {
    return typeof message.client_id === "string"
      && typeof message.client_name === "string"
      && typeof message.last_seen_sequence === "number";
  }
  if (message.type === "publish_inline") {
    return typeof message.message_id === "string"
      && isPayloadKind(message.kind)
      && typeof message.payload_hash === "string"
      && typeof message.bytes_base64 === "string";
  }
  if (message.type === "publish_r2") {
    return typeof message.message_id === "string"
      && isPayloadKind(message.kind)
      && typeof message.payload_hash === "string"
      && typeof message.object_key === "string"
      && typeof message.size === "number"
      && typeof message.expires_at === "number";
  }
  return false;
}

export function isPayloadKind(value: unknown): value is PayloadKind {
  return value === "text" || value === "image_png" || value === "file";
}

export function objectKeyFor(room: string, messageId: string, filename: string): string {
  return `rooms/${safeSegment(room)}/messages/${safeSegment(messageId)}/${safeSegment(filename)}`;
}

function safeSegment(value: string): string {
  const safe = value.replace(/[^A-Za-z0-9._-]/g, "_");
  return safe.length > 0 ? safe : "unnamed";
}
```

- [ ] **Step 6: Create minimal Worker entry and R2 helper**

Create `cloudflare/src/r2.ts`:

```ts
export type EnvWithObjects = {
  OBJECTS: R2Bucket;
};
```

Create `cloudflare/src/index.ts`:

```ts
export interface Env {
  ROOM: DurableObjectNamespace;
  OBJECTS: R2Bucket;
  AUTH_TOKEN: string;
}

export { SyncRoom } from "./room";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }
    return new Response("not found", { status: 404 });
  },
};
```

Create `cloudflare/src/room.ts`:

```ts
export class SyncRoom implements DurableObject {
  constructor(private state: DurableObjectState, private env: unknown) {}

  async fetch(): Promise<Response> {
    return new Response("not implemented", { status: 501 });
  }
}
```

- [ ] **Step 7: Install dependencies and run tests**

Run:

```powershell
pnpm --dir cloudflare install
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```

Expected: tests and typecheck pass.

- [ ] **Step 8: Commit**

```powershell
git add -- cloudflare
git commit -m "feat: scaffold cloudflare worker relay"
```

---

### Task 3: Worker Auth, R2 Upload, And Download

**Files:**
- Modify: `cloudflare/src/index.ts`
- Modify: `cloudflare/src/r2.ts`
- Test: `cloudflare/test/r2.test.ts`

- [ ] **Step 1: Write R2 endpoint tests**

Create `cloudflare/test/r2.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { isAuthorized, parseObjectPath } from "../src/r2";

describe("r2 endpoints", () => {
  it("accepts matching bearer token", () => {
    const request = new Request("https://example.com/objects/m1", {
      headers: { authorization: "Bearer secret" },
    });
    expect(isAuthorized(request, "secret")).toBe(true);
  });

  it("rejects missing bearer token", () => {
    const request = new Request("https://example.com/objects/m1");
    expect(isAuthorized(request, "secret")).toBe(false);
  });

  it("parses object message ids", () => {
    expect(parseObjectPath("/objects/message-1")).toEqual({ messageId: "message-1" });
  });

  it("rejects non-object paths", () => {
    expect(parseObjectPath("/other/message-1")).toBeNull();
  });
});
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```powershell
pnpm --dir cloudflare test r2
```

Expected: fails because `isAuthorized` and `parseObjectPath` are missing.

- [ ] **Step 3: Implement R2 helpers**

Replace `cloudflare/src/r2.ts` with:

```ts
import { DEFAULT_ROOM, R2_LIMIT_BYTES, objectKeyFor } from "./protocol";

export type ParsedObjectPath = { messageId: string };

export function isAuthorized(request: Request, token: string): boolean {
  return request.headers.get("authorization") === `Bearer ${token}`;
}

export function parseObjectPath(pathname: string): ParsedObjectPath | null {
  const match = pathname.match(/^\/objects\/([^/]+)$/);
  if (!match) return null;
  return { messageId: decodeURIComponent(match[1]) };
}

export function objectKeyForUpload(messageId: string, filename: string | null): string {
  return objectKeyFor(DEFAULT_ROOM, messageId, filename || "payload.bin");
}

export async function putObject(bucket: R2Bucket, key: string, request: Request): Promise<Response> {
  const length = Number(request.headers.get("content-length") || "0");
  if (!Number.isFinite(length) || length <= 0) {
    return new Response("missing content-length", { status: 411 });
  }
  if (length > R2_LIMIT_BYTES) {
    return new Response("payload too large", { status: 413 });
  }
  await bucket.put(key, request.body, {
    httpMetadata: { contentType: request.headers.get("content-type") || "application/octet-stream" },
  });
  return Response.json({ object_key: key, size: length });
}

export async function getObject(bucket: R2Bucket, key: string): Promise<Response> {
  const object = await bucket.get(key);
  if (!object) {
    return new Response("not found", { status: 404 });
  }
  return new Response(object.body, {
    headers: {
      "content-type": object.httpMetadata?.contentType || "application/octet-stream",
      "content-length": String(object.size),
    },
  });
}
```

- [ ] **Step 4: Wire HTTP endpoints**

Update `cloudflare/src/index.ts`:

```ts
import { getObject, isAuthorized, objectKeyForUpload, parseObjectPath, putObject } from "./r2";

export interface Env {
  ROOM: DurableObjectNamespace;
  OBJECTS: R2Bucket;
  AUTH_TOKEN: string;
}

export { SyncRoom } from "./room";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }

    const objectPath = parseObjectPath(url.pathname);
    if (objectPath) {
      if (!isAuthorized(request, env.AUTH_TOKEN)) {
        return new Response("unauthorized", { status: 401 });
      }
      const filename = url.searchParams.get("filename");
      const key = objectKeyForUpload(objectPath.messageId, filename);
      if (request.method === "PUT") {
        return putObject(env.OBJECTS, key, request);
      }
      if (request.method === "GET") {
        return getObject(env.OBJECTS, key);
      }
      return new Response("method not allowed", { status: 405 });
    }

    return new Response("not found", { status: 404 });
  },
};
```

- [ ] **Step 5: Run Worker tests**

Run:

```powershell
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```

Expected: pass.

- [ ] **Step 6: Commit**

```powershell
git add -- cloudflare/src cloudflare/test
git commit -m "feat: add worker r2 object endpoints"
```

---

### Task 4: Durable Object WebSocket Room

**Files:**
- Modify: `cloudflare/src/index.ts`
- Modify: `cloudflare/src/room.ts`
- Test: `cloudflare/test/room.test.ts`

- [ ] **Step 1: Write room unit tests for pure state helpers**

Create `cloudflare/test/room.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { chooseCatchupMessages, messagesForClient } from "../src/room";
import type { RelayMessage } from "../src/protocol";

function msg(sequence: number, source = "a"): RelayMessage {
  return {
    sequence,
    source,
    message_id: `m${sequence}`,
    kind: "text",
    payload_hash: `h${sequence}`,
    payload: { mode: "inline", bytes_base64: "aGVsbG8=" },
  };
}

describe("room catchup", () => {
  it("returns messages after last seen when available", () => {
    expect(chooseCatchupMessages([msg(1), msg(2), msg(3)], 1).map(m => m.sequence)).toEqual([2, 3]);
  });

  it("returns only latest state when client is too old", () => {
    expect(chooseCatchupMessages([msg(10), msg(11)], 1).map(m => m.sequence)).toEqual([11]);
  });

  it("does not send messages back to their source", () => {
    expect(messagesForClient([msg(1, "a"), msg(2, "b")], "a").map(m => m.sequence)).toEqual([2]);
  });
});
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```powershell
pnpm --dir cloudflare test room
```

Expected: fails because `chooseCatchupMessages` and `messagesForClient` are missing.

- [ ] **Step 3: Implement room helpers and WebSocket lifecycle**

Replace `cloudflare/src/room.ts` with:

```ts
import { INLINE_LIMIT_BYTES, type ClientMessage, type RelayMessage, type ServerMessage, isValidClientMessage } from "./protocol";

const MAX_RECENT_MESSAGES = 100;

type Session = {
  socket: WebSocket;
  clientId: string;
};

export function chooseCatchupMessages(messages: RelayMessage[], lastSeenSequence: number): RelayMessage[] {
  if (messages.length === 0) return [];
  const oldest = messages[0].sequence;
  if (lastSeenSequence > 0 && lastSeenSequence >= oldest - 1) {
    return messages.filter(message => message.sequence > lastSeenSequence);
  }
  return [messages[messages.length - 1]];
}

export function messagesForClient(messages: RelayMessage[], clientId: string): RelayMessage[] {
  return messages.filter(message => message.source !== clientId);
}

export class SyncRoom implements DurableObject {
  private sessions = new Set<Session>();
  private recent: RelayMessage[] = [];
  private nextSequence = 1;

  constructor(private state: DurableObjectState, private env: unknown) {}

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("upgrade") !== "websocket") {
      return new Response("expected websocket", { status: 426 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();

    let session: Session | null = null;

    server.addEventListener("message", event => {
      try {
        const parsed = JSON.parse(String(event.data)) as unknown;
        if (!isValidClientMessage(parsed)) {
          send(server, { type: "error", message: "invalid message" });
          return;
        }
        if (parsed.type === "hello") {
          session = { socket: server, clientId: parsed.client_id };
          this.sessions.add(session);
          send(server, { type: "hello_ack", latest_sequence: this.nextSequence - 1 });
          for (const message of messagesForClient(chooseCatchupMessages(this.recent, parsed.last_seen_sequence), parsed.client_id)) {
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
      if (session) this.sessions.delete(session);
    });
    server.addEventListener("error", () => {
      if (session) this.sessions.delete(session);
    });

    return new Response(null, { status: 101, webSocket: client });
  }

  private toRelayMessage(source: string, message: Exclude<ClientMessage, { type: "hello" }>): RelayMessage {
    if (message.type === "publish_inline") {
      const decodedLength = Math.floor(message.bytes_base64.length * 3 / 4);
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
        send(session.socket, { type: "message", ...message });
      }
    }
  }
}

function send(socket: WebSocket, message: ServerMessage): void {
  socket.send(JSON.stringify(message));
}
```

- [ ] **Step 4: Route `/ws` to Durable Object and enforce auth**

Update `cloudflare/src/index.ts` so `/ws` authenticates and forwards:

```ts
import { DEFAULT_ROOM } from "./protocol";
import { getObject, isAuthorized, objectKeyForUpload, parseObjectPath, putObject } from "./r2";

export interface Env {
  ROOM: DurableObjectNamespace;
  OBJECTS: R2Bucket;
  AUTH_TOKEN: string;
}

export { SyncRoom } from "./room";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health") {
      return Response.json({ status: "ok" });
    }

    if (url.pathname === "/ws") {
      if (!isAuthorized(request, env.AUTH_TOKEN)) {
        return new Response("unauthorized", { status: 401 });
      }
      const id = env.ROOM.idFromName(DEFAULT_ROOM);
      return env.ROOM.get(id).fetch(request);
    }

    const objectPath = parseObjectPath(url.pathname);
    if (objectPath) {
      if (!isAuthorized(request, env.AUTH_TOKEN)) {
        return new Response("unauthorized", { status: 401 });
      }
      const filename = url.searchParams.get("filename");
      const key = objectKeyForUpload(objectPath.messageId, filename);
      if (request.method === "PUT") {
        return putObject(env.OBJECTS, key, request);
      }
      if (request.method === "GET") {
        return getObject(env.OBJECTS, key);
      }
      return new Response("method not allowed", { status: 405 });
    }

    return new Response("not found", { status: 404 });
  },
};
```

- [ ] **Step 5: Run Worker tests and typecheck**

Run:

```powershell
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```

Expected: pass.

- [ ] **Step 6: Commit**

```powershell
git add -- cloudflare/src cloudflare/test
git commit -m "feat: add durable object websocket room"
```

---

### Task 5: Rust Cloudflare Transport

**Files:**
- Modify: `Cargo.toml`
- Create: `src/cloudflare.rs`
- Modify: `src/main.rs`
- Modify: `src/sync.rs`

- [ ] **Step 1: Add transport unit tests**

Create a test module in `src/cloudflare.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_ws_endpoint_from_https_server_url() {
        assert_eq!(
            ws_endpoint("https://clipsync.example.com").unwrap(),
            "wss://clipsync.example.com/ws"
        );
    }

    #[test]
    fn builds_object_endpoint_with_filename() {
        assert_eq!(
            object_endpoint("https://clipsync.example.com/", "m1", Some("sample.txt")).unwrap(),
            "https://clipsync.example.com/objects/m1?filename=sample.txt"
        );
    }
}
```

- [ ] **Step 2: Run tests and verify they fail**

Run:

```powershell
cargo test cloudflare::tests
```

Expected: fails because `src/cloudflare.rs` is not wired and helper functions do not exist.

- [ ] **Step 3: Add dependencies**

Update `Cargo.toml`:

```toml
tokio-tungstenite = { version = "0.26", features = ["rustls-tls-webpki-roots"] }
futures-util = "0.3"
urlencoding = "2"
```

Keep `reqwest` for R2 upload/download. Remove `axum` after Task 7 removes the local server.

- [ ] **Step 4: Create `src/cloudflare.rs`**

Create `src/cloudflare.rs`:

```rust
use crate::config::ClientConfig;
use crate::protocol::{ClientWsMessage, ServerWsMessage};
use anyhow::{Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;

pub struct CloudflareRelay {
    config: ClientConfig,
    http: Client,
}

impl CloudflareRelay {
    pub fn new(config: ClientConfig) -> Self {
        Self {
            config,
            http: Client::new(),
        }
    }

    pub async fn connect(
        &self,
        last_seen_sequence: u64,
    ) -> Result<(mpsc::Sender<ClientWsMessage>, mpsc::Receiver<ServerWsMessage>)> {
        let mut request = ws_endpoint(&self.config.server_url)?.into_client_request()?;
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {}", self.config.auth_token).parse()?,
        );
        let (socket, _) = connect_async(request).await?;
        let (mut write, mut read) = socket.split();
        let (out_tx, mut out_rx) = mpsc::channel::<ClientWsMessage>(64);
        let (in_tx, in_rx) = mpsc::channel::<ServerWsMessage>(64);

        let hello = ClientWsMessage::Hello {
            client_id: self.config.client_id.clone(),
            client_name: self.config.client_name.clone(),
            last_seen_sequence,
        };
        write.send(Message::Text(serde_json::to_string(&hello)?.into())).await?;

        tokio::spawn(async move {
            while let Some(message) = out_rx.recv().await {
                match serde_json::to_string(&message) {
                    Ok(json) => {
                        if write.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => log::warn!("failed to encode websocket message: {:?}", err),
                }
            }
        });

        tokio::spawn(async move {
            while let Some(message) = read.next().await {
                match message {
                    Ok(Message::Text(text)) => match serde_json::from_str::<ServerWsMessage>(&text) {
                        Ok(decoded) => {
                            if in_tx.send(decoded).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => log::warn!("failed to decode websocket message: {:?}", err),
                    },
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {}
                    Err(err) => {
                        log::warn!("websocket read failed: {:?}", err);
                        break;
                    }
                }
            }
        });

        Ok((out_tx, in_rx))
    }

    pub async fn upload_object(&self, message_id: &str, filename: Option<&str>, bytes: Vec<u8>) -> Result<String> {
        let url = object_endpoint(&self.config.server_url, message_id, filename)?;
        let response = self.http
            .put(url)
            .bearer_auth(&self.config.auth_token)
            .body(bytes)
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;
        response
            .get("object_key")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("missing object_key in upload response"))
    }

    pub async fn download_object(&self, message_id: &str, filename: Option<&str>) -> Result<Vec<u8>> {
        let url = object_endpoint(&self.config.server_url, message_id, filename)?;
        let bytes = self.http
            .get(url)
            .bearer_auth(&self.config.auth_token)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        Ok(bytes.to_vec())
    }
}

fn ws_endpoint(server_url: &str) -> Result<String> {
    let trimmed = server_url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        Ok(format!("wss://{rest}/ws"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        Ok(format!("ws://{rest}/ws"))
    } else {
        Err(anyhow!("server url must start with http:// or https://"))
    }
}

fn object_endpoint(server_url: &str, message_id: &str, filename: Option<&str>) -> Result<String> {
    let mut url = format!(
        "{}/objects/{}",
        server_url.trim_end_matches('/'),
        urlencoding::encode(message_id)
    );
    if let Some(filename) = filename {
        url.push_str("?filename=");
        url.push_str(&urlencoding::encode(filename));
    }
    Ok(url)
}
```

- [ ] **Step 5: Wire module**

Add to `src/main.rs`:

```rust
mod cloudflare;
```

- [ ] **Step 6: Run transport tests**

Run:

```powershell
cargo test cloudflare::tests
```

Expected: pass.

- [ ] **Step 7: Commit**

```powershell
git add -- Cargo.toml Cargo.lock src\cloudflare.rs src\main.rs
git commit -m "feat: add cloudflare websocket transport"
```

---

### Task 6: Replace Sync Loop With WebSocket/R2

**Files:**
- Modify: `src/sync.rs`
- Modify: `src/protocol.rs`
- Test: `src/sync.rs`

- [ ] **Step 1: Rewrite sync tests around Cloudflare message application**

Update tests in `src/sync.rs` to remove `PushRequest` assertions and add tests for applying inline and R2 relay payloads:

```rust
#[test]
fn inline_text_message_decodes_to_text_item() {
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
```

- [ ] **Step 2: Run sync tests and verify failures**

Run:

```powershell
cargo test sync::tests
```

Expected: fails because WebSocket apply helpers and new `RelayPayload` usage are not integrated.

- [ ] **Step 3: Replace `run_client` network orchestration**

Refactor `run_client` in `src/sync.rs`:

- Keep `receive_cleanup_loop`.
- Keep local clipboard polling.
- Replace `HttpRelayClient` with `CloudflareRelay`.
- Create one WebSocket send task and one receive task.
- Reconnect outer loop on WebSocket channel close.

The shape should be:

```rust
pub async fn run_client(config: ClientConfig) -> Result<()> {
    log::info!(
        "client starting: id={}, name={}, relay={}, receive_dir={}, max_inline_bytes={}, max_r2_bytes={}",
        config.client_id,
        config.client_name,
        config.server_url,
        config.receive_dir,
        INLINE_PAYLOAD_LIMIT_BYTES,
        R2_PAYLOAD_LIMIT_BYTES
    );

    let backend = Arc::new(Mutex::new(create_backend()?));
    log::info!("clipboard backend selected: {}", backend.lock().unwrap().name());

    let last_local_hash = Arc::new(Mutex::new(String::new()));
    let receive_cleanup_task = tokio::spawn(receive_cleanup_loop(PathBuf::from(&config.receive_dir)));
    let relay = Arc::new(CloudflareRelay::new(config.clone()));
    let mut last_seen_sequence = 0;

    loop {
        match relay.connect(last_seen_sequence).await {
            Ok((outgoing, incoming)) => {
                let local_task = tokio::spawn(local_send_loop(
                    config.clone(),
                    backend.clone(),
                    relay.clone(),
                    outgoing,
                    last_local_hash.clone(),
                ));
                let remote_result = remote_receive_loop(
                    config.clone(),
                    backend.clone(),
                    relay.clone(),
                    incoming,
                    last_local_hash.clone(),
                    &mut last_seen_sequence,
                ).await;
                local_task.abort();
                if let Err(err) = remote_result {
                    log::warn!("remote receive failed: {:?}", err);
                }
            }
            Err(err) => log::warn!("websocket connect failed: {:?}", err),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    #[allow(unreachable_code)]
    {
        receive_cleanup_task.abort();
        Ok(())
    }
}
```

- [ ] **Step 4: Implement local send loop**

`local_send_loop` should:

- Read clipboard snapshot.
- Convert to `LocalPayload`.
- Skip duplicates using `last_local_hash`.
- For inline payloads, send `ClientWsMessage::PublishInline`.
- For R2 payloads, upload bytes first and then send `ClientWsMessage::PublishR2`.

Use this expiration helper:

```rust
fn expires_at_24h() -> i64 {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    now + 24 * 60 * 60
}
```

- [ ] **Step 5: Implement remote receive loop**

`remote_receive_loop` should:

- Handle `ServerWsMessage::HelloAck`.
- Handle `ServerWsMessage::Error` by logging a warning.
- Handle `ServerWsMessage::Message(message)`.
- Update `last_seen_sequence`.
- Ignore self messages.
- Decode inline bytes or download R2 bytes.
- Validate hash and size.
- Apply text/image/file using existing clipboard and file transfer behavior.

- [ ] **Step 6: Remove old polling helpers**

Delete from `src/sync.rs`:

- `remote_pull_loop`.
- `messages_for_pull`.
- `item_to_push_request`.
- HTTP-specific tests.

- [ ] **Step 7: Run sync tests**

Run:

```powershell
cargo test sync::tests
```

Expected: pass.

- [ ] **Step 8: Commit**

```powershell
git add -- src\sync.rs src\protocol.rs
git commit -m "feat: sync clipboard over cloudflare websocket"
```

---

### Task 7: Remove Local Server And Polling Client Code

**Files:**
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Delete: `src/network.rs`
- Delete: `src/server.rs`
- Modify: `Cargo.toml`
- Modify: `README.md`

- [ ] **Step 1: Write CLI tests for Cloudflare-only mode**

In `src/config.rs`, replace server tests with:

```rust
#[test]
fn parses_client_required_args_without_mode() {
    let config = parse_config([
        "--server-url".to_string(),
        "https://clipsync.example.com".to_string(),
        "--auth-token".to_string(),
        "secret".to_string(),
        "--client-id".to_string(),
        "RYZEN".to_string(),
    ]).unwrap();

    assert_eq!(config.server_url, "https://clipsync.example.com");
    assert_eq!(config.client_id, "RYZEN");
    assert_eq!(config.auth_token, "secret");
    assert_eq!(config.max_payload_bytes, 100 * 1024 * 1024);
}

#[test]
fn rejects_server_mode() {
    let result = parse_config([
        "server".to_string(),
        "--auth-token".to_string(),
        "secret".to_string(),
    ]);
    assert!(result.is_err());
}
```

- [ ] **Step 2: Run CLI tests and verify they fail**

Run:

```powershell
cargo test config::tests
```

Expected: fails because `parse_config` still expects `client` or `server` modes.

- [ ] **Step 3: Simplify config**

Change `src/config.rs`:

- Remove `AppConfig`.
- Remove `ServerConfig`.
- Remove `parse_server`.
- Remove polling interval fields from `ClientConfig`.
- Make `parse_config_from_env() -> Result<ClientConfig>`.
- Allow optional legacy `client` prefix for a smoother CLI, or reject all modes. The spec says server mode is removed; accepting `client` as a no-op is acceptable if tests cover it.

The resulting usage:

```rust
fn usage() -> &'static str {
    "Usage:\n  rustclipsync --server-url https://WORKER_URL --auth-token TOKEN [--client-id ID] [--client-name NAME]"
}
```

- [ ] **Step 4: Simplify main**

Update `src/main.rs`:

```rust
mod clipboard;
mod cloudflare;
mod config;
mod file_transfer;
mod protocol;
mod security;
mod sync;

use crate::config::parse_config_from_env;
use crate::sync::run_client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default_filter_or("info")).init();
    run_client(parse_config_from_env()?).await?;
    Ok(())
}
```

- [ ] **Step 5: Delete unused modules and dependencies**

Delete:

```text
src/network.rs
src/server.rs
```

Remove from `Cargo.toml` if unused:

```toml
axum = "0.7"
```

Keep `reqwest` for R2.

- [ ] **Step 6: Update README**

Replace server/polling sections with:

```markdown
## Cloudflare Relay

Deploy the Worker in `cloudflare/` with Wrangler. The Worker uses:

- Durable Object binding `ROOM`
- R2 bucket binding `OBJECTS`
- Secret `AUTH_TOKEN`

```bash
cd cloudflare
pnpm install
pnpm wrangler r2 bucket create rustclipsync-objects
pnpm wrangler secret put AUTH_TOKEN
pnpm deploy
```

## Client

```powershell
.\rustclipsync.exe --server-url https://YOUR_WORKER.workers.dev --auth-token YOUR_TOKEN --client-id windows-client
```
```

- [ ] **Step 7: Run full Rust tests**

Run:

```powershell
cargo test
```

Expected: pass.

- [ ] **Step 8: Commit**

```powershell
git add -- Cargo.toml Cargo.lock README.md src
git commit -m "refactor: remove polling relay server"
```

---

### Task 8: End-To-End Verification And Build

**Files:**
- Modify only if verification finds defects.

- [ ] **Step 1: Run Rust formatting and tests**

Run:

```powershell
cargo fmt --check
cargo test
```

Expected: all pass.

- [ ] **Step 2: Run Worker checks**

Run:

```powershell
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```

Expected: all pass.

- [ ] **Step 3: Build release binary**

Run:

```powershell
cargo build --release
```

Expected: release binary builds successfully.

- [ ] **Step 4: Optional local Worker smoke test**

Run:

```powershell
pnpm --dir cloudflare dev
```

In another shell, verify health:

```powershell
Invoke-RestMethod http://127.0.0.1:8787/health
```

Expected:

```json
{"status":"ok"}
```

- [ ] **Step 5: Commit verification fixes if needed**

Only if defects were found:

```powershell
git add -- <changed-files>
git commit -m "fix: stabilize cloudflare relay migration"
```

---

## Self-Review

- Spec coverage: Cloudflare Worker, Durable Object WebSocket, R2 upload/download, 10 MB inline limit, 100 MB R2 limit, startup latest-state behavior, auth, cleanup direction, docs, and removal of polling/server compatibility all have tasks.
- Placeholder scan: no placeholder tokens or unspecified implementation steps remain.
- Type consistency: Rust uses `ClientWsMessage`, `ServerWsMessage`, `RelayMessage`, and `RelayPayload`; Worker uses matching snake_case protocol names.
