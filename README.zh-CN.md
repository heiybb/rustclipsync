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
src/             Rust 桌面客户端（库 + 命令行程序）
src/gui/         Windows 托盘 GUI：gpui 日志窗口和托盘图标（feature `gui`）
assets/          程序图标，以及把它嵌进可执行文件的资源脚本
build.rs         只给 GUI 程序挂上 assets/rustclipsync.rc
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

### 托盘 GUI（Windows）

GUI 前端放在 `gui` feature 后面，默认构建不受影响，也不会引入界面依赖：

```powershell
cargo build --release --features gui
```

输出：`target\release\rustclipsync-gui.exe`。

前置条件：Windows，以及 Windows 10/11 SDK 里的 `fxc.exe`——在 `PATH` 上、位于
SDK 默认路径，或用 `GPUI_FXC_PATH` 指定都可以。release 构建下 `gpui` 会提前编译
HLSL 着色器；debug 构建改为运行时编译，不需要 SDK。

#### 程序图标

`build.rs` 会把 `assets/rustclipsync.ico` 以资源 id 1 嵌进
`rustclipsync-gui.exe`，它同时充当可执行文件图标（资源管理器、任务栏、Alt-Tab）
和通知区域图标。要换图标，替换这一个文件即可。

请保持多分辨率。托盘会按当前 DPI 向 Windows 请求通知区域需要的精确尺寸——随着
缩放比例升高依次是 16、20、24、32 像素——图标里备好这些尺寸就能被原样取用，不必
重采样。有一个测试专门检查它们存在。

换图标前先把透明留白裁掉。图标生成器往往在图形周围留很宽的边，而这种留白躲得过
其他所有检查——文件能解析、尺寸齐全、资源能嵌入、托盘能加载——图标却仍然比通知
区域里的邻居明显小一圈，因为真正画出来的像素只占画布中间那一块。图形应该在较长
的那条边上顶到画布边缘；有一个测试会测量 alpha 通道，不达标就带着实际填充百分比
报错。

命令行程序有意不带图标：资源是按二进制单独挂载的，所以 `rustclipsync.exe` 不会
白背 64 KB 永远不会显示的数据。

## 部署 Cloudflare 中继

安装 Node.js 和 pnpm，然后部署 Worker：

```bash
cd cloudflare
pnpm install
pnpm wrangler r2 bucket create rustclipsync-objects
pnpm wrangler secret put AUTH_TOKEN
pnpm deploy --domain relay.example.com
```

`wrangler.toml` 关掉了 `*.workers.dev` 和预览地址，Worker 只能通过 `--domain` 传入的自定义域名访问。
这个域名必须是同一个 Cloudflare 账号下的 zone。想用 `workers.dev` 的话，把 `workers_dev` 改成 `true`，
部署时不加 `--domain`。

Worker 使用：

- Durable Object 绑定：`ROOM`
- R2 绑定：`OBJECTS`
- R2 bucket：`rustclipsync-objects`
- Secret：`AUTH_TOKEN`

健康检查：

```bash
curl https://relay.example.com/health
```

期望返回：

```json
{"status":"ok"}
```

## 配置客户端

第一次运行客户端时，如果当前目录没有 `config.toml`，RustClipSync 会生成配置模板并退出。

示例：

```toml
server_url = "https://relay.example.com"
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

### 托盘 GUI

`rustclipsync-gui.exe` 跑的是同一套同步客户端，只是没有控制台窗口，并在通知区域
放一个图标：

- **左键单击**图标：打开日志窗口；已经打开时则收起。
- **右键**：打开日志窗口、重载 `config.toml` 并重启同步、退出。
- 关闭日志窗口（或在窗口里按 `Esc`）只是隐藏它，同步照常运行；要退出请用托盘
  菜单。

窗口里显示的就是命令行版写到 stderr 的那些记录，同样遵循 `RUST_LOG`，保留最近
4000 条。工具栏可以设置最低级别、切换自动跟随、清空缓冲。彩色圆点和托盘悬浮提示
显示中继连接状态。

加 `--hidden` 可以启动时不弹窗口，放到启动文件夹的快捷方式应该这么写：

```powershell
.\rustclipsync-gui.exe --hidden
```

`config.toml` 的查找顺序是 `$RUSTCLIPSYNC_CONFIG`、当前工作目录、可执行文件所在
目录。最后一条是为快捷方式准备的——快捷方式的工作目录通常不是安装目录。

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
