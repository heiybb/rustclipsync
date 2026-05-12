# RustClipSync

English | [简体中文](README.zh-CN.md)

RustClipSync is a clipboard and file synchronization tool for trusted devices. The desktop client connects to a Cloudflare Worker relay over WebSocket. Durable Objects coordinate live sessions, and R2 stores large transferred files.

## Current Architecture

RustClipSync no longer ships a self-hosted HTTP polling relay. The relay is now serverless:

```text
Windows client  ─┐
Linux X11 client ├─ WebSocket ─ Cloudflare Worker ─ Durable Object room
Other clients   ─┘                              └─ R2 for large files
```

Local clipboard changes are detected by platform-specific watchers where available. Small payloads are broadcast over WebSocket. Large files are uploaded to R2 first, then announced to other clients through the Durable Object room.

## Features

- Text, PNG image, and file synchronization
- Event-driven clipboard watching on Windows
- X11 clipboard support on Linux through `xclip`
- Cloudflare Worker relay with Durable Objects
- R2 storage for large file payloads
- Shared bearer-token authentication
- Inline WebSocket payloads up to 10 MB
- R2-backed file payloads up to 100 MB
- Received files are saved into `receive/`
- Local `config.toml` configuration

## Repository Layout

```text
cloudflare/      Cloudflare Worker, Durable Object, R2 endpoints, tests
src/             Rust desktop client
config.toml      Example client configuration
```

## Build The Client

Install Rust, then build:

```bash
cargo build --release
```

Output:

```text
target/release/rustclipsync
target\release\rustclipsync.exe
```

## Deploy The Cloudflare Relay

Install Node.js and pnpm, then deploy the Worker:

```bash
cd cloudflare
pnpm install
pnpm wrangler r2 bucket create rustclipsync-objects
pnpm wrangler secret put AUTH_TOKEN
pnpm deploy
```

The Worker uses:

- Durable Object binding: `ROOM`
- R2 binding: `OBJECTS`
- R2 bucket: `rustclipsync-objects`
- Secret: `AUTH_TOKEN`

Health check:

```bash
curl https://YOUR_WORKER.workers.dev/health
```

Expected response:

```json
{"status":"ok"}
```

## Configure The Client

Run the client once. If `config.toml` does not exist, RustClipSync creates a template and exits.

Example:

```toml
server_url = "https://YOUR_WORKER.workers.dev"
auth_token = "YOUR_TOKEN"

# Optional: Custom name for this device. Defaults to hostname.
# client_name = "my-desktop"

# Optional: Local clipboard poll fallback interval in ms.
# poll_interval_ms = 300

# Optional: Where received files are saved.
# receive_dir = "receive"
```

`auth_token` must match the Cloudflare Worker `AUTH_TOKEN` secret.

## Run The Client

Windows:

```powershell
.\rustclipsync.exe
```

Linux X11:

```bash
sudo apt update
sudo apt install -y xclip
./rustclipsync
```

Use a different hostname or `client_name` for each trusted device.

## Payload Rules

- `<= 10 MB`: sent inline over WebSocket
- `> 10 MB` and `<= 100 MB`: uploaded to R2, then announced over WebSocket
- `> 100 MB`: rejected locally

Downloaded files are written to `receive/`. Old received files are cleaned up after 24 hours.

## Logging

Default log level is `info`.

PowerShell:

```powershell
$env:RUST_LOG="rustclipsync=debug,info"
.\rustclipsync.exe
```

Bash:

```bash
RUST_LOG=rustclipsync=debug,info ./rustclipsync
```

## Security Notes

RustClipSync is intended for trusted devices.

- Use a strong random token.
- Store the token only in `config.toml` and Cloudflare Worker secrets.
- Clipboard contents may contain sensitive data.
- R2 objects are relay payloads, not backups.
- Do not share the Worker URL and token with untrusted users.

## Development

Rust checks:

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Cloudflare Worker checks:

```bash
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```
