# RustClipSync

RustClipSync is a lightweight clipboard and file sync tool for trusted devices. It uses a Cloudflare Worker relay with Durable Objects, WebSocket, and R2, so clients behind NAT or firewalls do not need inbound ports.

## Features

- WebSocket relay through Cloudflare Workers and Durable Objects
- Text, PNG image, and file synchronization
- Files are saved into a local `receive/` directory
- Shared bearer token authentication
- Payloads up to 10 MB are sent inline over WebSocket
- Files larger than 10 MB and up to 100 MB are stored in R2
- Windows clipboard support, including file paths and PNG images
- Linux X11 clipboard support through `xclip`
- Command-line configuration only, no config file required

## Architecture

Deploy one Cloudflare Worker relay. Every desktop connects to the Worker over WebSocket.

```text
Windows client  ─┐
Ubuntu X11      ─┼─ Cloudflare Worker ─ Durable Object room
Other clients   ─┘                       └─ R2 for large files
```

When one client detects a local clipboard change, it publishes the payload to the Worker. Other connected clients receive the update immediately. Large files are uploaded to R2 first, then announced over WebSocket.

## Build

Install Rust, then build:

```bash
cargo build --release
```

The binary is created at:

```text
target/release/rustclipsync
```

On Windows:

```text
target\release\rustclipsync.exe
```

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

Health check:

```bash
curl https://YOUR_WORKER.workers.dev/health
```

Expected response:

```json
{"status":"ok"}
```

## Client

Windows:

```powershell
.\rustclipsync.exe --server-url https://YOUR_WORKER.workers.dev --auth-token YOUR_TOKEN --client-id windows-client
```

Ubuntu X11:

```bash
sudo apt update
sudo apt install -y xclip
./rustclipsync --server-url https://YOUR_WORKER.workers.dev --auth-token YOUR_TOKEN --client-id ubuntu-x11
```

Use a unique `--client-id` for each machine.

## Logging

The default log level is `info`.

PowerShell debug run:

```powershell
$env:RUST_LOG="rustclipsync=debug,info"
.\rustclipsync.exe --server-url https://YOUR_WORKER.workers.dev --auth-token YOUR_TOKEN --client-id windows-client
```

Bash debug run:

```bash
RUST_LOG=rustclipsync=debug,info ./rustclipsync --server-url https://YOUR_WORKER.workers.dev --auth-token YOUR_TOKEN --client-id ubuntu-x11
```

## Security Notes

RustClipSync is intended for trusted devices.

- Use a strong random `--auth-token`.
- Store the same token as the Cloudflare Worker `AUTH_TOKEN` secret.
- Clipboard contents may contain sensitive data. Only run clients on machines you trust.
- R2-backed payloads are intended to be short-lived relay objects, not durable backups.

## Development

Run Rust checks:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

Run Cloudflare Worker checks:

```bash
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```
