# RustClipSync

[English](README.md) | 简体中文

RustClipSync 是一个面向可信设备的剪贴板和文件同步工具。桌面客户端通过 WebSocket 连接到 Cloudflare Worker 中继；Durable Objects 负责协调在线会话，R2 用来保存较大的文件负载。

## 当前架构

RustClipSync 现在不再包含自建 HTTP 轮询中继服务。新的中继是 Cloudflare serverless 架构：

```text
Windows 客户端 ─┐
Linux X11 客户端├─ WebSocket ─ Cloudflare Worker ─ Durable Object 房间
其他客户端     ─┘                              └─ R2 存储大文件
```

本地剪贴板变化会尽量通过平台事件监听触发。小负载直接通过 WebSocket 广播。大文件会先上传到 R2，再通过 Durable Object 房间把元数据广播给其他客户端。

## 功能

- 同步文本、PNG 图片和文件
- Windows 事件驱动剪贴板监听
- Linux X11 通过 `xclip` 支持剪贴板
- Cloudflare Worker 中继
- Durable Objects 协调 WebSocket 会话
- R2 保存大文件负载
- 共享 bearer token 鉴权
- 10 MB 以内负载直接走 WebSocket
- 10 MB 到 100 MB 文件走 R2
- 接收文件保存到 `receive/`
- 使用本地 `config.toml` 配置

## 仓库结构

```text
cloudflare/      Cloudflare Worker、Durable Object、R2 接口和测试
src/             Rust 桌面客户端
config.toml      客户端配置示例
```

## 构建客户端

安装 Rust 后执行：

```bash
cargo build --release
```

输出位置：

```text
target/release/rustclipsync
target\release\rustclipsync.exe
```

## 部署 Cloudflare 中继

安装 Node.js 和 pnpm，然后部署 Worker：

```bash
cd cloudflare
pnpm install
pnpm wrangler r2 bucket create rustclipsync-objects
pnpm wrangler secret put AUTH_TOKEN
pnpm deploy
```

Worker 使用：

- Durable Object 绑定：`ROOM`
- R2 绑定：`OBJECTS`
- R2 bucket：`rustclipsync-objects`
- Secret：`AUTH_TOKEN`

健康检查：

```bash
curl https://YOUR_WORKER.workers.dev/health
```

期望返回：

```json
{"status":"ok"}
```

## 配置客户端

第一次运行客户端时，如果当前目录没有 `config.toml`，RustClipSync 会生成配置模板并退出。

示例：

```toml
server_url = "https://YOUR_WORKER.workers.dev"
auth_token = "YOUR_TOKEN"

# 可选：设备显示名称。默认使用主机名。
# client_name = "my-desktop"

# 可选：本地剪贴板轮询兜底间隔，单位毫秒。
# poll_interval_ms = 300

# 可选：接收文件保存目录。
# receive_dir = "receive"
```

`auth_token` 必须和 Cloudflare Worker 里的 `AUTH_TOKEN` secret 一致。

## 运行客户端

Windows：

```powershell
.\rustclipsync.exe
```

Linux X11：

```bash
sudo apt update
sudo apt install -y xclip
./rustclipsync
```

每台可信设备应使用不同主机名，或者在 `config.toml` 中设置不同的 `client_name`。

## 负载规则

- `<= 10 MB`：直接通过 WebSocket 发送
- `> 10 MB` 且 `<= 100 MB`：上传到 R2，再通过 WebSocket 广播元数据
- `> 100 MB`：本地拒绝发送

下载的文件会写入 `receive/`。旧接收文件会在 24 小时后清理。

## 日志

默认日志级别是 `info`。

PowerShell：

```powershell
$env:RUST_LOG="rustclipsync=debug,info"
.\rustclipsync.exe
```

Bash：

```bash
RUST_LOG=rustclipsync=debug,info ./rustclipsync
```

## 安全说明

RustClipSync 面向可信设备使用。

- 使用强随机 token。
- token 只应保存在 `config.toml` 和 Cloudflare Worker secret 中。
- 剪贴板内容可能包含敏感信息。
- R2 对象只是中继负载，不是备份。
- 不要把 Worker URL 和 token 分享给不可信用户。

## 开发

Rust 检查：

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Cloudflare Worker 检查：

```bash
pnpm --dir cloudflare test
pnpm --dir cloudflare typecheck
```
