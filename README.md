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
src/             Rust desktop client (library + CLI binary)
src/gui/         Windows tray GUI: gpui log window and tray icon (feature `gui`)
assets/          Application icon and the resource script that embeds it
build.rs         Attaches assets/rustclipsync.rc to the GUI binary only
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

### Tray GUI (Windows)

The GUI front end sits behind the `gui` feature, so the default build stays
small and free of UI dependencies:

```powershell
cargo build --release --features gui
```

Output: `target\release\rustclipsync-gui.exe`.

Requirements: Windows, plus `fxc.exe` from the Windows 10/11 SDK either on
`PATH`, at the SDK's default location, or pointed at by `GPUI_FXC_PATH`.
Release builds of `gpui` compile their HLSL shaders ahead of time; debug builds
compile them at runtime and need no SDK.

#### Application icon

`build.rs` embeds `assets/rustclipsync.ico` into `rustclipsync-gui.exe` as
resource id 1, which serves as both the executable icon (Explorer, taskbar,
Alt-Tab) and the notification-area icon. To change it, replace that one file.

Keep it multi-resolution. The tray asks Windows for the exact size the
notification area wants at the current DPI — 16, 20, 24 or 32 px as the display
scaling factor rises — so an icon holding those entries is used verbatim
instead of being resampled. A test asserts they are present.

Crop the transparent padding out first. Icon generators tend to leave a wide
margin around the glyph, which is invisible to every other check — the file
parses, the sizes are all there, the resource embeds, the tray loads it — and
the icon still renders visibly smaller than its neighbours in the notification
area, because the drawn pixels only occupy the middle of each canvas. The glyph
should reach the canvas edge on its longer axis; a test measures the alpha
channel and fails with the actual fill percentages if it does not.

The CLI binary deliberately gets no icon: the resource is attached per-binary,
so `rustclipsync.exe` does not carry 64 KB it would never display.

## Deploy The Cloudflare Relay

Install Node.js and pnpm, then deploy the Worker:

```bash
cd cloudflare
pnpm install
pnpm wrangler r2 bucket create rustclipsync-objects
pnpm wrangler secret put AUTH_TOKEN
pnpm deploy --domain relay.example.com
```

`wrangler.toml` turns off the `*.workers.dev` and preview URLs, so the Worker is
only reachable through the custom domain passed with `--domain`. The domain must
be a zone on the same Cloudflare account. To use `workers.dev` instead, set
`workers_dev = true` and deploy without `--domain`.

The Worker uses:

- Durable Object binding: `ROOM`
- R2 binding: `OBJECTS`
- R2 bucket: `rustclipsync-objects`
- Secret: `AUTH_TOKEN`

Health check:

```bash
curl https://relay.example.com/health
```

Expected response:

```json
{"status":"ok"}
```

## Configure The Client

Run the client once. If `config.toml` does not exist, RustClipSync creates a template and exits.

Example:

```toml
server_url = "https://relay.example.com"
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

### Tray GUI

`rustclipsync-gui.exe` runs the same sync client with no console window and
puts an icon in the notification area:

- **Left click** the icon: show the log window, or hide it if it is already up.
- **Right click**: show the log window, reload `config.toml` and restart sync,
  or quit.
- Closing the log window, or pressing `Esc` in it, only hides it. Sync keeps
  running; quit from the tray menu.

The window shows the same records the CLI writes to stderr, honours `RUST_LOG`,
and retains the last 4000 of them. Its toolbar sets a minimum level, toggles
follow-tail, and clears the buffer. The coloured dot and the tray tooltip show
the relay connection state.

Pass `--hidden` to start without opening the window, which is what a
startup-folder shortcut wants:

```powershell
.\rustclipsync-gui.exe --hidden
```

`config.toml` is looked up in `$RUSTCLIPSYNC_CONFIG`, then the working
directory, then next to the executable. The last one matters for shortcuts,
whose working directory is usually not the install directory.

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
