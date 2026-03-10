# NetFile

NetFile 是一个高性能的局域网/跨NAT文件传输工具，支持设备发现、文件传输、断点续传、压缩传输、TLS 加密、NAT 穿透（iroh QUIC P2P + Relay）、信令服务器好友系统等功能。

## 核心特性

- **设备发现**: 基于 UDP 广播的自动设备发现，支持局域网内设备互相发现
- **文件传输**: 基于 TCP/QUIC 的可靠文件传输，支持分块传输和断点续传
- **文件夹传输**: 支持递归传输文件夹，保留完整目录结构
- **进度跟踪**: 实时显示传输进度、速度、限速来源和预计剩余时间
- **压缩传输**: 使用 zstd 算法智能压缩，自动判断压缩收益
- **安全机制**: 支持设备授权、密码保护和 TLS 加密
- **NAT 穿透**: STUN 获取公网地址 + iroh QUIC P2P 直连（NAT 打洞）+ iroh Relay 中继
- **信令服务器**: 跨 NAT 的好友系统、邀请码配对、消息中继、iroh 地址分发
- **TURN 中继**: 信令服务器可选开启文件传输 TCP 中继，双方均在 NAT 后仍可传输文件
- **文件共享**: 支持文件共享浏览，好友可浏览和下载共享文件
- **传输历史**: 持久化记录收发历史，支持按 MD5 同步共享状态
- **设备别名与收藏**: 为设备设置自定义别名，收藏常用设备置顶显示
- **局域网广播发送**: 一键向局域网内所有设备并发发送同一文件
- **多信令服务器**: 支持逗号分隔配置多个信令服务器地址，自动故障转移
- **传输完成通知**: 传输结束时弹出 toast 提示
- **版本更新检查**: 启动时检查 GitHub 最新 Release，支持跳过指定版本
- **诊断日志导出**: 一键将运行日志打包为 zip 文件
- **多模式单二进制**: GUI（默认）/ `--cli` / `--tui` 三种启动模式
- **跨平台**: 支持 Windows、Linux、macOS

## 架构设计

### 模块结构

```
netfile/
├── crates/
│   ├── netfile-core/          # 核心库
│   │   └── src/
│   │       ├── config.rs      # 配置管理
│   │       ├── discovery/     # 设备发现
│   │       ├── transfer/      # 文件传输
│   │       ├── protocol.rs    # 协议定义
│   │       ├── auth.rs        # 身份验证
│   │       ├── compression.rs # 压缩模块
│   │       ├── tls.rs         # TLS 加密
│   │       ├── stun.rs        # STUN 协议（并发批量查询）
│   │       ├── iroh_net.rs    # iroh QUIC endpoint 管理
│   │       ├── message_store.rs # 消息持久化
│   │       └── signal_client.rs # 信令客户端
│   ├── netfile-signal/        # 信令服务器（独立部署）
│   │   └── src/
│   │       ├── main.rs        # 服务入口
│   │       ├── protocol.rs    # 信令协议（JSON over TCP）
│   │       └── server.rs      # ServerState + handle_connection + rate limiting
│   ├── netfile-gui/           # Tauri GUI 客户端（兼含 CLI/TUI 入口）
│   └── netfile-cli/           # 独立 CLI 工具（已合并至 netfile-gui）
└── docs/                      # 文档目录
```

### 核心组件

#### 1. 设备发现 (Discovery)

使用 UDP 广播实现局域网设备自动发现：
- 定时广播设备信息（默认 5 秒间隔）
- 监听其他设备的广播消息
- 维护在线设备列表，支持心跳超时检测
- 通过 `set_public_transfer_addr()` 接受信令服务器回填公网地址

#### 2. 文件传输 (Transfer)

支持两条传输路径：

**LAN 直连（TCP）：**
- 分块传输（默认 1MB）
- 断点续传（记录已传输的块）
- 并发控制（默认最多 3 个）
- 完整性校验：SHA256 文件级、CRC32 块级

**iroh QUIC（NAT 穿透）：**
- 通过 iroh endpoint 建立 QUIC 连接
- 自动选择 P2P 直连或 iroh Relay 中继
- 传输条实时显示连接路径（LAN / P2P / Relay / NAT）
- 连接失败时指数退避重试（最多 3 次，200/400/800ms）

**文件传输回退链：**
```
局域网直连（LAN TCP）
  └─ 失败 → iroh QUIC P2P（NAT 打洞）
              └─ 失败 → iroh QUIC Relay（中继）
                          └─ 失败 → 信令服务器 TURN TCP 中继（--relay-port）
```

#### 3. 信令服务器 (Signal Server)

独立部署的信令服务器，提供跨 NAT 通信能力。协议为 JSON over TCP，每条消息使用 4 字节大端 length prefix 分帧。

**服务端状态：**
- `online`: 当前在线设备表（device_id → 连接信息含 iroh_addr）
- `friends`: 好友关系表（双向 HashSet，内存持久）
- `invite_codes`: 邀请码表（8 位大写字母，10 分钟 TTL）
- `offline_msgs`: 离线消息队列（每设备最多 200 条）
- `relay_addr`: 可选，非 None 时开启 TURN TCP 文件中继
- `total_connections` / `connections_per_ip`: 连接数控流计数器

**好友生命周期：**
1. 设备连接后发送 `Register`，服务端推送 `Registered{friends, observed_addr, stun_addr, iroh_relay_url}` 和离线消息
2. 设备上线/下线时服务端向其好友广播 `FriendOnline` / `FriendOffline`
3. 设备 iroh 地址变化时发送 `UpdateIrohAddr`，服务端向好友广播更新后的 `FriendOnline`

**邀请码配对流程：**
1. 设备 A 发送 `GenerateInvite`，服务端返回 8 位邀请码
2. 设备 B 发送 `AcceptInvite{code}`，服务端建立双向好友关系
3. 双方收到 `InviteResult`，如果对方在线同时收到 `FriendOnline`

**消息中继：**
- 目标在线时：服务端实时转发 `RelayedMessage`
- 目标离线时：消息入队 `offline_msgs`，对方上线后批量推送

**文件 TURN 中继（需 `--relay-port` 开启）：**
1. 发送方发送 `RequestRelay{to_device_id}`，服务端生成 session_key 并通知双方 `RelayReady{session_key, relay_addr}`
2. 双方各自 TCP 连接 relay 端口并发送 session_key
3. 服务端配对两个连接，启动双向管道 `copy_bidirectional`

#### 4. iroh QUIC (IrohManager)

`netfile-core/src/iroh_net.rs` 中的 `IrohManager`：
- 管理 iroh QUIC endpoint 的生命周期（密钥持久化到 `~/.netfile/iroh/secret_key`）
- `connect(addr)` / `accept()` 建立/接受 QUIC 连接
- `get_conn_type(conn)` 检测连接实际路径（P2P 直连 `iroh-p2p` 或 Relay 中继 `iroh-relay`）
- 支持自定义 relay URL（通过 `RelayMode::Custom`）

#### 5. 信令客户端 (SignalClient)

`netfile-core/src/signal_client.rs` 中的 `SignalClient`：
- 维护到信令服务器的单一 TCP 长连接，reader/writer 独立 task
- 启动时并发 STUN 查询公网地址（批量 3 个，每个 2s 超时）；全部失败才使用 `observed_addr` 回填
- 收到 `Registered` 后依次更新 `stun_addr`、`iroh_relay_url`、`transfer_addr`，最后设置状态为 `Connected`（避免竞态）
- `get_peer_iroh_addr(device_id)` 获取好友 iroh 地址，发送方最多等待 8s
- 收到 `RelayedMessage` / `OfflineMessages` 时写入 `MessageStore`

#### 6. 协议设计

信令协议使用 JSON，传输协议使用 bincode 二进制：

```json
// C2S
{"type":"register","device_id":"...","instance_name":"...","transfer_addr":"1.2.3.4:37050"}
{"type":"generate_invite"}
{"type":"accept_invite","code":"ABCD1234"}
{"type":"update_transfer_addr","transfer_addr":"1.2.3.4:37050"}
{"type":"update_iroh_addr","iroh_addr":"<json>"}
{"type":"request_relay","to_device_id":"..."}
{"type":"relay_message","to_device_id":"...","content":"...","timestamp":1234567890}
{"type":"heartbeat"}

// S2C
{"type":"registered","friends":[...],"observed_addr":"1.2.3.4","stun_addr":"1.2.3.4:37201","iroh_relay_url":"https://..."}
{"type":"invite_code","code":"ABCD1234"}
{"type":"invite_result","success":true,"friend":{...},"error":null}
{"type":"friend_online","device_id":"...","instance_name":"...","transfer_addr":"...","iroh_addr":"<json>"}
{"type":"friend_offline","device_id":"..."}
{"type":"relay_ready","session_key":"uuid","relay_addr":"host:port"}
{"type":"relayed_message","from_device_id":"...","from_instance_name":"...","content":"...","timestamp":...}
{"type":"offline_messages","messages":[...]}
{"type":"error","message":"..."}
```

## 配置文件

配置文件位于 `~/.netfile/config.toml`：

```toml
[instance]
instance_id = "uuid"
instance_name = "默认实例"
device_name = "hostname"

[network]
discovery_port = 0              # 0 表示自动分配
transfer_port = 0
broadcast_interval = 5          # 秒
signal_server_addr = ""         # 信令服务器地址，格式 host:port
iroh_relay_url = ""             # 自建 iroh-relay URL，空则使用信令服务器下发值
stun_servers = []               # 自定义 STUN 服务器列表

[transfer]
chunk_size = 1048576            # 1MB
max_concurrent = 3
enable_compression = false
download_dir = ""               # 空则使用系统下载目录
speed_limit_mbps = 0            # 0 为不限速
require_confirmation = true     # 接收文件前需确认
iroh_stream_count = 4           # iroh QUIC 并发流数
quic_stream_window_mb = 32      # QUIC 流接收窗口 MB
history_page_size = 20          # 历史记录每页条数
enable_sharing = false          # 开启文件共享功能
sharing_require_confirm = true  # 共享文件下载需确认

[security]
require_auth = true
password = ""
allowed_devices = []
enable_tls = false
```

## 信令服务器部署与使用

### 部署服务端

```bash
# 编译
cargo build --release --package netfile-signal

# 运行（内置 STUN 服务默认开启在 37201）
./target/release/netfile-signal

# 开启 TCP 文件中继（TURN）
./target/release/netfile-signal --relay-port 37202

# 配置自建 iroh-relay
./target/release/netfile-signal --iroh-relay-url https://your-relay-host:443

# 自定义所有参数
./target/release/netfile-signal \
  --host 0.0.0.0 \
  --port 37200 \
  --relay-port 37202 \
  --stun-port 37201 \
  --stun-public-ip 1.2.3.4 \
  --iroh-relay-url https://your-relay-host:443 \
  --max-connections 500 \
  --max-connections-per-ip 5 \
  --rate-limit-msgs-per-sec 20 \
  --max-msg-bytes 65536
```

**启动参数说明：**

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--host` | `0.0.0.0` | 监听地址 |
| `--port` | `37200` | 信令 TCP 端口 |
| `--relay-port` | 无 | TCP 文件中继端口（不设则不开启） |
| `--stun-enabled` | `true` | 是否开启内置 UDP STUN 服务 |
| `--stun-port` | `port + 1` | STUN 监听 UDP 端口（默认 37201） |
| `--stun-public-ip` | `--host` 值 | STUN 对外公告的公网 IP（host 为 0.0.0.0 时必须设置） |
| `--iroh-relay-url` | 无 | 自建 iroh-relay 的 HTTPS URL，广播给客户端 |
| `--max-connections` | `500` | 全局最大并发连接数 |
| `--max-connections-per-ip` | `5` | 单 IP 最大并发连接数 |
| `--rate-limit-msgs-per-sec` | `20` | 单连接每秒最大消息数 |
| `--max-msg-bytes` | `65536` | 单条消息最大字节数 |

服务端为纯内存状态，重启后好友关系、离线消息全部清空。

**防火墙：** 信令端口 TCP（默认 37200）、STUN 端口 UDP（默认 37201）必须开放入站；开启 TCP 文件中继时还需开放对应端口 TCP 入站。

### 自建 iroh-relay（可选，推荐国内部署）

iroh-relay 是 iroh 项目提供的 QUIC 中继服务，用于在 P2P 打洞失败时提供低延迟中继。默认客户端使用 iroh 官方海外节点，国内延迟高（200-500ms），建议自建。

```bash
# 安装（需要 Rust 工具链）
cargo install iroh-relay

# 运行（需要 HTTPS 证书，监听 443）
iroh-relay --hostname your-relay-host --certfile cert.pem --keyfile key.pem
```

配置后，客户端在连接信令服务器时会自动收到 URL（通过 `Registered.iroh_relay_url`），无需客户端手动配置。

### 客户端连接信令服务器

**方式一：通过 GUI 设置界面**

打开设置 → 「网络配置」→「信令服务器地址」输入 `host:37200` → 点击「连接」→ 状态变为「已连接」→ 点击「保存」

支持填写多个地址（逗号分隔），客户端自动选择第一个可连接的服务器：
```
host1:37200,host2:37200
```

**方式二：直接编辑配置文件**

```toml
[network]
signal_server_addr = "your-server-ip:37200"  # 或 "host1:37200,host2:37200"
```

### 添加好友（邀请码配对）

1. 一方点击「邀请好友」→「生成邀请码」tab，获得 8 位邀请码
2. 告知对方，对方切换到「输入邀请码」tab 输入并确认
3. 配对成功，双方设备列表「网络好友」区块出现对方（标注 `WAN` 徽标）
4. 邀请码有效期 10 分钟

### 跨 NAT 通信流程

**文字消息：**
- 优先尝试直连 `transfer_addr`
- 直连失败时自动回退到信令服务器中继

**文件传输（四层回退）：**
```
LAN 直连（UDP 发现的局域网 IP:port）
  └─ 失败 → iroh QUIC P2P（NAT 打洞，通过信令交换 iroh_addr）
              └─ 失败 → iroh QUIC Relay（iroh 内置中继，自动降级）
                          └─ 失败 → TURN TCP 中继（信令服务器 --relay-port）
```

## 数据结构

### FriendInfo

```rust
pub struct FriendInfo {
    pub device_id: String,                  // 对端 instance_id（持久唯一）
    pub instance_name: String,              // 显示名称
    pub online: bool,                       // 当前是否在线
    pub transfer_addr: Option<String>,      // 公网 transfer_addr（IP:port）
    pub iroh_addr: Option<String>,          // iroh EndpointAddr（JSON）
}
```

### TransferProgress（Tauri → UI）

```rust
pub struct TransferProgress {
    pub file_id: String,
    pub file_name: String,
    pub total_size: u64,
    pub transferred: u64,
    pub speed: u64,
    pub eta_secs: u64,
    pub elapsed_secs: u64,
    pub direction: String,          // "send" | "receive"
    pub status: String,             // "active" | "queued" | "pending_confirm" | "error"
    pub paused: bool,
    pub transfer_method: Option<String>,    // "lan" | "iroh-p2p" | "iroh-relay" | "iroh"
    pub speed_limit_source: Option<String>, // "sender" | "receiver"
}
```

## Tauri 命令（GUI ↔ Rust）

| 命令 | 说明 |
|------|------|
| `get_devices()` | 获取已发现设备列表 |
| `get_transfers()` | 获取当前传输队列 |
| `send_file(...)` | 发送文件（LAN → iroh P2P → iroh Relay → TURN 回退） |
| `cancel_transfer(file_id)` | 取消传输 |
| `pause_transfer(file_id)` | 暂停传输 |
| `resume_transfer(file_id)` | 继续传输 |
| `pause_all_transfers()` | 暂停全部 |
| `resume_all_transfers()` | 继续全部 |
| `confirm_transfer(file_id)` | 确认接收 |
| `confirm_transfer_save_as(file_id, save_path)` | 另存为确认接收 |
| `reject_transfer(file_id)` | 拒绝接收 |
| `get_my_public_addr()` | 获取本机公网传输地址 |
| `send_text_message(peer_id, target_addr, content)` | 发送文字消息 |
| `get_conversation(peer_id)` | 获取会话消息 |
| `get_conversation_delta(peer_id, after_ts)` | 增量获取消息 |
| `get_message_counts()` | 获取各会话未读数 |
| `get_transfer_history()` | 获取传输历史 |
| `clear_transfer_history()` | 清空传输历史 |
| `delete_transfer_record(id)` | 删除单条历史 |
| `delete_transfer_records(ids)` | 批量删除历史 |
| `get_share_entries()` | 获取共享文件列表 |
| `set_share_excluded(id, excluded)` | 设置共享排除状态 |
| `update_share_tags(id, tags)` | 更新共享标签 |
| `update_share_remark(id, remark)` | 更新共享备注 |
| `query_device_shares(device_id)` | 查询对端共享文件 |
| `query_all_shares()` | 查询所有好友共享 |
| `get_bookmarks()` | 获取书签列表 |
| `add_bookmark(entry)` | 添加书签 |
| `remove_bookmark(id)` | 删除书签 |
| `request_file_download(device_id, file_id)` | 请求下载共享文件 |
| `request_file_download_to(device_id, file_id, save_path)` | 下载到指定路径 |
| `add_local_file_to_share(path)` | 添加本地文件到共享 |
| `get_config()` | 获取配置 |
| `update_config(config)` | 更新配置 |
| `connect_signal_server(server_addr)` | 连接信令服务器（支持逗号分隔多地址，自动故障转移） |
| `disconnect_signal_server()` | 断开信令服务器 |
| `get_signal_status()` | 获取信令连接状态 |
| `generate_invite_code()` | 生成邀请码 |
| `accept_invite_code(code)` | 接受邀请码 |
| `get_signal_friends()` | 获取信令好友列表 |
| `send_relay_message(to_device_id, content)` | 发送中继消息 |
| `send_file_broadcast(file_path, compression)` | 向所有局域网设备广播发送文件 |
| `get_device_aliases()` | 获取设备别名与收藏状态 |
| `set_device_alias(device_id, alias)` | 设置设备别名 |
| `set_device_favorite(device_id, favorite)` | 设置设备收藏状态 |
| `export_diagnostics()` | 将运行日志打包为 zip，返回文件路径 |

## 数据目录

```
~/.netfile/
├── config.toml          # 配置文件
├── data/                # 传输数据（消息、历史、共享、书签）
├── logs/                # 运行日志（按日滚动，保留 14 天）
├── device_aliases.json  # 设备别名与收藏（JSON）
├── diagnostics.zip      # 诊断日志导出文件
└── iroh/
    └── secret_key       # iroh QUIC 节点密钥（持久化）
```

## 启动模式

```bash
# GUI 模式（默认）
./netfile-gui

# CLI 模式
./netfile-gui --cli

# TUI 终端界面模式
./netfile-gui --tui
```

## 技术栈

- **语言**: Rust 2021 Edition
- **异步运行时**: Tokio
- **序列化**: Serde, Bincode, TOML, JSON
- **网络**: Tokio TCP/UDP + iroh QUIC
- **加密**: SHA256, Rustls, RCGen
- **压缩**: Zstd
- **NAT 穿透**: STUN + iroh 0.96（QUIC P2P + Relay）
- **CLI**: Clap
- **TUI**: Ratatui + Crossterm
- **日志**: Tracing + tracing-appender（日滚动文件）
- **GUI**: Tauri v2 + React + TypeScript
- **打包**: zip（诊断导出）

## 许可证

MIT License
