# Cloudflare Durable Objects WebSocket + R2 Relay Design

Date: 2026-05-11

## Context

RustClipSync currently uses a small in-memory HTTP polling relay. Clients push clipboard payloads to `/push` and poll `/pull?client_id=&after=` for new messages. This works, but it requires running a reachable server and it adds polling latency.

The new relay should replace the polling relay with Cloudflare serverless infrastructure.

Relevant Cloudflare limits checked during design:

- Durable Object received WebSocket message size: 32 MiB.
- Worker request body size: 100 MB on Free and Pro plans.
- Worker isolate memory: 128 MB.
- R2 object size: up to 5 TiB, with single-request uploads up to about 5 GiB, subject to Worker request limits when uploaded through a Worker.

## Goals

- Add a Cloudflare relay using Workers, Durable Objects, WebSocket, and R2.
- Replace the existing HTTP polling relay and CLI server mode.
- Use WebSocket for real-time clipboard sync.
- Send payloads up to 10 MB directly over WebSocket.
- Store files larger than 10 MB and up to 100 MB in R2.
- Reject files larger than 100 MB with a clear client log message.
- Avoid applying a long historical backlog on client startup.
- Keep authentication simple and compatible with the current shared bearer token model.

## Non-Goals

- Supporting files larger than 100 MB in the first Cloudflare version.
- Implementing multipart or presigned direct-to-R2 uploads in the first version.
- Building a web dashboard or account management system.
- Supporting untrusted public rooms or multi-user authorization.
- Keeping polling transport compatibility.

## Architecture

The Cloudflare relay has three parts:

```text
Rust client
  WebSocket /ws
    -> Worker
      -> Durable Object room
        -> connected clients
        -> sequence counter
        -> recent message index

Rust client
  HTTP PUT /objects/:message_id
    -> Worker
      -> R2 bucket

Rust client
  HTTP GET /objects/:message_id
    -> Worker
      -> R2 bucket
```

The Durable Object is the authoritative coordinator for one sync room. The first implementation can use a single default room. The object assigns sequence numbers, tracks connected clients, broadcasts live messages, and stores a small recent message index for reconnection.

R2 stores large payload bytes. The Durable Object stores only metadata for R2-backed messages.

## Transport

The client uses Cloudflare WebSocket transport directly. The existing polling transport and local server command are removed from the supported design.

Example:

```text
rustclipsync client --server-url https://clipsync.example.com --auth-token TOKEN --client-id RYZEN
```

## Message Model

Existing fields remain:

- `sequence`
- `source`
- `message_id`
- `kind`
- `payload_hash`
- `filename`

Cloudflare messages add a payload mode:

```text
inline:
  bytes_base64 or binary payload

r2:
  object_key
  size
  expires_at
```

The preferred implementation should avoid base64 for WebSocket payloads when practical, because base64 adds about one third overhead. If binary frames add too much complexity for the first pass, JSON with base64 is acceptable because the 10 MB threshold still fits within the 32 MiB Durable Object received WebSocket message limit.

## Payload Routing

Payload routing is based on raw payload byte size:

```text
size <= 10 MB:
  Send over WebSocket.

10 MB < size <= 100 MB:
  Upload to R2 through Worker HTTP PUT.
  Then announce metadata over WebSocket.

size > 100 MB:
  Reject locally before upload or send.
```

For R2-backed payloads, the sender flow is:

1. Client detects clipboard file payload.
2. Client calculates hash and message id.
3. Client uploads bytes to `PUT /objects/:message_id`.
4. Worker validates auth, size, and request path.
5. Worker writes to R2 under a deterministic key.
6. Client sends WebSocket metadata message with object key, size, filename, and hash.
7. Durable Object assigns sequence and broadcasts metadata.

The receiver flow is:

1. Client receives metadata message.
2. Client downloads bytes from `GET /objects/:message_id`.
3. Client validates hash and size.
4. Client writes the file into the local receive directory.

## Authentication

Use the current shared bearer token model:

- WebSocket upgrade requires `Authorization: Bearer TOKEN`, or a token query parameter only if header support is not reliable in the selected Rust WebSocket library.
- R2 upload and download endpoints require the same bearer token.
- The Worker rejects unauthenticated requests with `401`.

The token remains a deployment secret in Cloudflare Worker configuration.

## Startup And Reconnect Behavior

Clients maintain `last_seen_sequence` while connected.

On WebSocket connect, the client sends:

```text
hello:
  client_id
  last_seen_sequence
```

The Durable Object responds with one of:

- Messages after `last_seen_sequence`, if available in the recent index.
- Only the latest applicable state if the client is new or its sequence is too old.

This preserves the recent fix: startup should not apply a long historical backlog when only the latest clipboard state matters.

## Durable Object State

The Durable Object keeps:

- Connected WebSocket sessions keyed by client id and socket id.
- `next_sequence`.
- Recent message index.
- Queued byte accounting for inline payloads.
- Expiry metadata for R2-backed objects.

The recent index should be bounded by:

- Maximum message count.
- Maximum inline byte count.
- Message expiry age.

The initial default should match current behavior where practical:

- Recent message count: 100.
- Expiry: 24 hours.
- Inline payload ceiling per message: 10 MB.
- R2 payload ceiling per message: 100 MB.

## R2 Object Layout

Use deterministic keys:

```text
rooms/default/messages/{message_id}/{filename}
```

If a filename is missing or unsafe, use a sanitized fallback name. The local receiver still sanitizes filenames before writing files.

## Cleanup

The Durable Object uses alarms for cleanup:

- Remove expired messages from the recent index.
- Delete expired R2 objects.

Default R2 retention is 24 hours. This mirrors the current local received-file cleanup policy and keeps storage bounded.

R2 bucket lifecycle rules can be added later, but application-level cleanup is required in the first version so behavior is explicit and testable.

## Error Handling

Client behavior:

- WebSocket disconnects trigger reconnect with exponential backoff.
- Upload failure prevents metadata broadcast.
- Download failure logs a warning and skips apply.
- Hash mismatch logs a warning and skips apply.
- Payloads larger than 100 MB are rejected locally.

Worker and Durable Object behavior:

- Invalid auth returns `401`.
- Invalid JSON or message schema returns an error frame or `400`.
- Payload too large returns `413`.
- Missing R2 object returns `404`.
- The Durable Object does not broadcast messages back to their source client.

## Repository Changes

Expected implementation areas:

- Add a `cloudflare/` Worker project with TypeScript and Wrangler config.
- Replace the Rust polling client network loop with Cloudflare WebSocket transport.
- Add R2 upload/download client code.
- Add protocol types for inline and R2-backed WebSocket messages.
- Remove Rust HTTP relay server code and polling HTTP client code that are no longer used.
- Simplify CLI configuration by removing server mode and polling-specific settings.
- Update README with Cloudflare deployment instructions.

## Testing

Rust tests:

- Payload routing threshold: inline at 10 MB or below, R2 above 10 MB, reject above 100 MB.
- WebSocket reconnect state selection.
- R2 metadata validation and hash verification.
- CLI tests cover the Cloudflare-only client configuration.
- Removed polling/server code has no compatibility test requirement.

Cloudflare Worker tests:

- Auth rejection.
- WebSocket hello and broadcast behavior.
- R2 upload/download endpoints.
- 100 MB limit enforcement.
- Expiry cleanup behavior with alarms where feasible.

Manual verification:

- Run two local clients against a deployed or local Wrangler Worker.
- Sync text.
- Sync a small image inline.
- Sync a file between 10 MB and 100 MB through R2.
- Confirm a file larger than 100 MB is rejected before upload.
- Restart a client and verify it applies only the latest state.

## Rollout

1. Implement Cloudflare Worker and Durable Object.
2. Replace the Rust client transport with WebSocket/R2.
3. Remove the local Rust server mode and polling relay code.
4. Deploy to a test Worker URL.
5. Validate with two trusted devices.
6. Document production deployment.

Rollback is a git-level rollback to the last polling-relay commit if the new transport is not acceptable.
