# RustClipSync

RustClipSync is a lightweight clipboard and file sync tool for trusted devices. It uses a small HTTP relay server, so clients behind NAT or firewalls can synchronize through a VPS or any reachable host.

## Features

- HTTP polling relay, no inbound client port required
- Text, PNG image, and file synchronization
- Files are saved into a local `receive/` directory
- Shared bearer token authentication
- 10 MB payload limit
- Windows clipboard support, including file paths and PNG images
- Linux X11 clipboard support through `xclip`
- Command-line configuration only, no config file required

## Architecture

Run one relay server on a reachable machine. Every desktop runs as a client and polls the relay.

```text
Windows client  ─┐
Ubuntu X11      ─┼─ HTTP relay server ─ broadcasts to other online clients
Other clients   ─┘
```

When one client detects a local clipboard change, it pushes the payload to the relay. Other clients pull new messages every 500 ms and apply them locally.

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

## Server

Run the relay server on a VPS or a reachable host:

```bash
rustclipsync server --auth-token YOUR_TOKEN --bind-addr 0.0.0.0:7878
```

Health check:

```bash
curl http://127.0.0.1:7878/health
```

Expected response:

```json
{"status":"ok"}
```

## Client

Windows:

```powershell
.\rustclipsync.exe client --server-url http://SERVER_IP:7878 --auth-token YOUR_TOKEN --client-id windows-client
```

Ubuntu X11:

```bash
sudo apt update
sudo apt install -y xclip
./rustclipsync client --server-url http://SERVER_IP:7878 --auth-token YOUR_TOKEN --client-id ubuntu-x11
```

Use a unique `--client-id` for each machine.

## Logging

The default log level is `info`.

PowerShell debug run:

```powershell
$env:RUST_LOG="rustclipsync=debug,info"
.\rustclipsync.exe client --server-url http://SERVER_IP:7878 --auth-token YOUR_TOKEN --client-id windows-client
```

Bash debug run:

```bash
RUST_LOG=rustclipsync=debug,info ./rustclipsync client --server-url http://SERVER_IP:7878 --auth-token YOUR_TOKEN --client-id ubuntu-x11
```

## Security Notes

RustClipSync is intended for trusted devices and trusted networks.

- Use a strong random `--auth-token`.
- Do not expose the relay without firewall rules or a reverse proxy unless you understand the risk.
- Plain HTTP is supported directly. For internet deployment, prefer a TLS reverse proxy, VPN, or private network overlay.
- Clipboard contents may contain sensitive data. Only run clients on machines you trust.

## Development

Run tests and lint checks:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```
