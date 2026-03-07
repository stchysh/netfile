# 使用 webrtc crate 改造整个项目 NAT 打洞与传输方案

## 1. 目标

本文从整个项目的角度，整理如果使用 Rust 的 `webrtc` crate 来改造 NetFile 的 NAT 打洞流程，需要做哪些事情。

这里说的不是“把现有 STUN/打洞代码换个库”，而是评估一次完整的连接层重构：

1. 如何用 WebRTC 接管跨 NAT 建链。
2. 现有 QUIC 文件传输如何迁移。
3. 信令服务端需要改到什么程度。
4. 哪些模块可以保留，哪些模块应该下线。
5. 应该按什么顺序做，风险最低。

## 2. 当前项目现状

当前 NetFile 的跨 NAT 主流程是：

1. `TransferService` 启动时对 `transfer_port` 做 STUN，拿到 `public_addr` 和 `nat_type`。
2. GUI 发文件时先尝试局域网地址直连。
3. 直连失败后，如果本端 NAT 被判断为可打洞，则通过 `SignalClient` 发 `RequestPunch`。
4. `netfile-signal` 协调 `PunchCoordinate` / `PunchRequest` / `PunchReady` / `PunchStart`。
5. 客户端收到协调消息后，调用 `TransferService::punch_hole(peer_addr)`。
6. `punch_hole()` 的实际动作不是简单 UDP PUNCH，而是直接对对端公网 `transfer_addr` 发起 Quinn/QUIC `connect()` 预热。
7. 连接成功后缓存 `quinn::Connection`，文本消息和文件传输都复用它。
8. 失败时再回退到当前信令服务端提供的自定义 TCP relay。

当前涉及的核心代码：

- `crates/netfile-core/src/stun.rs`
- `crates/netfile-core/src/hole_punch.rs`
- `crates/netfile-core/src/signal_client.rs`
- `crates/netfile-core/src/transfer/service.rs`
- `crates/netfile-signal/src/protocol.rs`
- `crates/netfile-signal/src/server.rs`
- `crates/netfile-gui/src/lib.rs`

当前方案的问题：

- NAT 分类、打洞时序、会话状态机都由业务层自己维护。
- `punch_hole()` 本质是“尝试 QUIC 直连”，不是真正的 ICE。
- relay 是自定义 TCP 中继，不是标准 TURN。
- 传输层强依赖 `quinn::Connection` 和 `open_bi()`，网络层和业务层耦合很深。

## 3. 先给结论

### 3.1 总结论

如果用 `webrtc` crate 来改造整个项目的 NAT 打洞流程，合理的方向不是：

- 继续保留当前 Quinn/QUIC 主链路；
- 只把 STUN 和手写打洞状态机替换成 WebRTC。

更合理的方向是：

**把“跨 NAT P2P 传输层”整体切换成 WebRTC 的 `RTCPeerConnection + ICE + STUN/TURN + DataChannel`。**

换句话说：

- WebRTC 不只负责打洞。
- WebRTC 同时会接管跨 NAT 的连接承载层。
- 当前基于 Quinn stream 的文件和文本协议，需要迁移到 DataChannel 或新的传输抽象之上。

### 3.2 为什么这是一次大改，不是小改

当前项目的传输建立在：

- UDP socket
- Quinn endpoint
- QUIC connection
- QUIC bi stream

而 WebRTC crate 建立在：

- ICE candidate gathering/checking
- STUN/TURN
- DTLS
- SCTP
- DataChannel

这意味着：

- 现在的“连接对象”不再是 `quinn::Connection`。
- 现在的“数据承载”不再天然是 QUIC bi stream。
- 当前文件协议虽然能保留大部分业务语义，但传输承载需要重写一层。

## 4. 官方 crate 现状

截至 **2026-03-06**，官方 `webrtc` crate 在 docs.rs 的最新稳定版本是 `0.17.1`。仓库 README 也明确说明：

- `v0.17.x` 是 Tokio 绑定实现的最终 feature release。
- `v0.17.x` 只收 bug fix，适合当前 Tokio 生产应用。
- `master` 正在开发 `v0.20.0+` 的新 Sans-I/O 架构。
- 对生产使用，官方建议当前阶段继续使用 `v0.17.x`。

因此当前项目如果落地，版本策略建议是：

- **先固定 `webrtc = "0.17.1"`**
- 不直接跟 `master`
- 不直接押注未来 `v0.20.0+` 的未稳定 API

官方文档里和本项目最相关的能力包括：

- `RTCPeerConnection`
  - `create_offer`
  - `create_answer`
  - `set_local_description`
  - `set_remote_description`
  - `add_ice_candidate`
  - `restart_ice`
  - `get_stats`
  - `on_ice_candidate`
  - `on_ice_connection_state_change`
  - `on_peer_connection_state_change`
  - `create_data_channel`
- `RTCIceServer`
  - 配置 STUN/TURN
- `RTCConfiguration`
  - 定义 ICE server 和连接策略
- `SettingEngine`
  - `detach_data_channels`
  - `set_ice_timeouts`
  - `set_network_types`
  - `set_interface_filter`
  - `set_ip_filter`
  - `set_nat_1to1_ips`
- `RTCDataChannel`
  - `on_message`
  - `send`
  - `send_text`
  - `detach`
  - `buffered_amount`
  - `set_buffered_amount_low_threshold`

## 5. 改造后的总体架构建议

### 5.1 建议的目标架构

建议把项目拆成四层：

1. **发现层**
   - 局域网内继续使用当前 UDP discovery。
2. **信令层**
   - signal server 继续负责好友、邀请码、在线状态、离线消息。
   - 同时新增 WebRTC SDP/ICE 交换。
3. **连接层**
   - 跨 NAT 建链由 `RTCPeerConnection` 负责。
   - TURN 回退由标准 TURN 服务负责。
4. **业务传输层**
   - 继续保留当前文本消息、文件传输、断点续传、压缩、进度、历史记录语义。
   - 但承载从 QUIC bi stream 改到 WebRTC DataChannel。

### 5.2 两个可选目标

#### 目标 A：混合架构

- 局域网内仍可走当前直连快路径。
- 跨 NAT 走 WebRTC。
- 这是最现实的一阶段方案。

#### 目标 B：统一架构

- 无论 LAN 还是 WAN，只要是在线 P2P，都统一走 WebRTC。
- 长期维护成本更低，但一次性改动更大。

建议：

- **先做 A**
- 稳定后再决定是否收口到 B

## 6. 需要改的核心模块

### 6.1 新增 `netfile-core::webrtc`

建议新增独立模块，例如：

```text
crates/netfile-core/src/
├── webrtc/
│   ├── mod.rs
│   ├── manager.rs       # WebRtcManager：创建和管理 PeerConnection
│   ├── session.rs       # WebRtcSession：单个对端会话
│   ├── signaling.rs     # SDP / ICE candidate 编解码
│   ├── transport.rs     # DataChannel 封装为统一传输接口
│   ├── stats.rs         # stats / candidate pair / relay 命中统计
│   └── config.rs        # WebRTC 运行参数和 ICE/TURN 配置
```

### 6.2 `TransferService` 解耦

当前 `TransferService` 自己负责：

- 创建 UDP socket
- 创建 Quinn endpoint
- STUN
- NAT 类型
- `get_or_connect()`
- `punch_hole()`
- connection cache

改造后建议：

- `TransferService` 不再直接管理 Quinn endpoint。
- `TransferService` 只依赖一个统一的传输抽象。

例如：

```text
trait PeerTransport {
    async fn open_channel(&self, kind: ChannelKind) -> anyhow::Result<Channel>;
    async fn accept_channel(&self, kind: ChannelKind) -> anyhow::Result<Channel>;
    async fn close(&self) -> anyhow::Result<()>;
}
```

这样 `TransferService` 只管：

- 文件协议
- 文本消息协议
- chunk 管理
- 压缩
- 断点续传
- 进度统计

而不再关心底下是 QUIC stream 还是 DataChannel。

### 6.3 `stun.rs` 和 `hole_punch.rs`

改造后：

- `stun.rs` 不应再作为主流程依赖。
- `hole_punch.rs` 基本可以退出主路径。

它们的去向建议：

- `stun.rs`
  - 第一阶段降级为诊断模块或日志辅助。
  - 稳定后删除。
- `hole_punch.rs`
  - 明确标记为实验代码。
  - 稳定后删除。

### 6.4 `signal_client.rs`

当前它承担：

- 好友系统
- 邀请码
- 离线消息
- punch
- relay 请求

改造后建议：

- 保留好友、邀请码、离线消息。
- 删除当前 `request_punch()` / `send_punch_ready()` / `request_relay()` 主路径语义。
- 新增 WebRTC signaling 能力：
  - `send_offer`
  - `send_answer`
  - `send_ice_candidate`
  - `send_webrtc_close`
  - `send_webrtc_restart`

同时需要能把下行消息投递给 `WebRtcManager`。

### 6.5 `netfile-signal`

当前服务端要维护：

- `online`
- `friends`
- `invite_codes`
- `offline_msgs`
- `relay_sessions`
- `punch_sessions`

改造后建议：

#### 保留

- `online`
- `friends`
- `invite_codes`
- `offline_msgs`

#### 删除或降级

- `punch_sessions`
- 当前自定义文件 relay 主路径

#### 新增

- WebRTC session 元信息
- offer/answer 转发
- trickle ICE candidate 转发
- candidate 缓存
- session TTL 和回收

## 7. 信令协议应该怎么改

### 7.1 删除的旧消息

这些消息不应再是主路径：

- `request_punch`
- `punch_ready`
- `punch_coordinate`
- `punch_request`
- `punch_start`
- `request_relay`
- `relay_session`
- `incoming_relay`
- `relay_unavailable`

### 7.2 新增的 WebRTC 信令消息

建议新增：

```json
{"type":"webrtc_offer","target_device_id":"...","session_id":"...","sdp":"..."}
{"type":"webrtc_answer","target_device_id":"...","session_id":"...","sdp":"..."}
{"type":"webrtc_ice_candidate","target_device_id":"...","session_id":"...","candidate":"...","sdp_mid":"0","sdp_mline_index":0}
{"type":"webrtc_ice_done","target_device_id":"...","session_id":"..."}
{"type":"webrtc_close","target_device_id":"...","session_id":"...","reason":"..."}
{"type":"webrtc_restart","target_device_id":"...","session_id":"..."}
```

### 7.3 服务端 session 状态

建议服务端增加会话状态：

```text
session_id ->
  initiator_id
  target_id
  offer
  answer
  pending_candidates_for_a
  pending_candidates_for_b
  state
  created_at
```

### 7.4 关键处理点

- 对端还没 `set_remote_description()` 前，candidate 可能先到，必须缓存。
- `on_ice_candidate` 在 gathering 结束时会给一个空值，需要有对应结束语义。
- `restart_ice()` 不是单纯本地重连，通常会触发新的 negotiation。
- 所有 session 必须有 TTL。

## 8. TURN/中继策略

### 8.1 当前 relay 的问题

当前 relay 是：

- 设备两端各连服务端一个 TCP socket
- 服务端 `copy_bidirectional()`

它能兜底，但对 WebRTC 来说不是 TURN。

### 8.2 WebRTC 路线的推荐做法

如果要走 `webrtc` crate，推荐：

- **直接部署标准 TURN**
- 推荐用 `coturn`
- signal server 不自己实现 TURN

原因：

- `RTCIceServer` 已经可以直接配置 STUN/TURN URL、用户名、密码。
- 标准 TURN 的行为和 ICE 状态机是天然配套的。
- 自己写 TURN 成本高，测试面大。

### 8.3 部署建议

一阶段部署形态建议：

- `netfile-signal`
  - 负责 signaling 和好友系统
- `coturn`
  - 负责 TURN relay

不要让 `netfile-signal` 继续承担“业务 signaling + 自定义 relay + 未来 TURN”三种职责。

## 9. 传输层怎么迁移

### 9.1 一个关键事实

一旦选择 `webrtc` crate，跨 NAT 的承载就不再是 Quinn 的 bi stream，而是 DataChannel。

因此不能再假设：

- `conn.open_bi()`
- `conn.accept_bi()`

就是最终业务收发接口。

### 9.2 推荐方案：两个 DataChannel

建议至少拆成：

- `control`
  - 可靠、有序
  - 用于文本消息、`TransferRequest`、`TransferResponse`、Ack、状态同步
- `bulk`
  - 可靠、有序
  - 用于文件数据帧

### 9.3 为什么不建议大文件直接走 `on_message`

官方 `RTCDataChannel::on_message()` 文档明确写了：

- 当前回调模式下接收消息最多 **16384 bytes**
- 如果要更大的消息，应使用 `detach()`

同时 `SettingEngine::detach_data_channels()` 文档明确要求：

- 开启 detach 模式后，要在 `OnOpen` 中调用 `detach()`

因此对于文件传输：

- **不要直接依赖 `on_message` 处理大块文件**
- 推荐方案是：
  - `detach_data_channels()`
  - `bulk` channel `detach()`
  - 自己做分帧、背压和缓冲管理

### 9.4 chunk 和 frame 分层

建议把两个概念分开：

1. **业务 chunk**
   - 当前已有语义
   - 用于断点续传、哈希、进度
2. **传输 frame**
   - DataChannel 实际发送粒度
   - 比如 16KiB、32KiB、64KiB 级别调优

这样能保留：

- 当前 resume 逻辑
- 当前文件协议语义

同时避免：

- DataChannel 单帧过大
- 缓冲暴涨
- 回调模式限制

### 9.5 背压与吞吐控制

WebRTC 路线上必须补这一层：

- `buffered_amount`
- `set_buffered_amount_low_threshold`
- `on_buffered_amount_low`

否则大文件发送时：

- 内存会快速上涨
- 队列会失控

## 10. NAT 相关策略变化

### 10.1 NAT 类型不再是主决策条件

当前代码中，`NatType::is_punchable()` 会直接决定是否进入打洞分支。

改成 WebRTC 后，这个判断应该降级为：

- 诊断信息
- UI 展示
- 日志参考

不应继续主导连接策略。

主路径应改成：

1. 创建 `RTCPeerConnection`
2. 收集 host / srflx / relay candidate
3. 交换 candidate
4. 做 connectivity checks
5. 自动选择 candidate pair

### 10.2 `SettingEngine` 需要显式配置

当前项目是 native app，不是浏览器环境，通常需要更主动地配置：

- `set_ice_timeouts`
- `set_network_types`
- `set_interface_filter`
- `set_ip_filter`
- `set_nat_1to1_ips`

尤其对桌面端和多网卡机器，这些设置很重要。

## 11. 安全模型

### 11.1 传输安全

WebRTC 路径的传输层会变成：

- ICE
- DTLS
- SCTP
- DataChannel

### 11.2 业务身份不能靠 DTLS 指纹代替

需要明确：

- DTLS 证书保证链路安全
- 但不等于业务身份

项目里原有这些仍然需要保留：

- `device_id`
- 好友关系
- 授权列表
- 可选密码

推荐：

- 继续用当前信令注册身份和好友系统做业务鉴权
- 不把 WebRTC transport 建立成功误认为“身份验证完成”

## 12. 配置项建议

建议新增独立配置：

```toml
[webrtc]
enabled = true
prefer_webrtc = true
detach_data_channels = true

[[webrtc.ice_servers]]
urls = ["stun:stun.cloudflare.com:3478"]

[[webrtc.ice_servers]]
urls = ["turn:turn.example.com:3478?transport=udp"]
username = "netfile"
credential = "secret"

[webrtc.transport]
bulk_frame_size = 16384
control_channel_label = "control"
bulk_channel_label = "bulk"

[webrtc.tuning]
ice_disconnected_timeout_ms = 5000
ice_failed_timeout_ms = 25000
ice_keepalive_interval_ms = 2000
only_udp = true
disable_mdns = true
```

## 13. 推荐迁移步骤

### 阶段 0：准备

- 加入 `webrtc = "0.17.1"`
- 新建 `netfile-core::webrtc`
- 设计 signal 协议
- 抽象 `PeerTransport`

交付物：

- 代码结构不再把 Quinn 细节散落在业务层

### 阶段 1：最小 WebRTC PoC

- 建立 `RTCPeerConnection`
- 走 signal server 交换 offer/answer/candidate
- 建立一个 `control` DataChannel
- 做 ping/pong 和文本消息验证

交付物：

- 双 NAT 下可稳定建链

### 阶段 2：文本消息切换

- 在线文本消息改走 `control` channel
- 离线消息继续保留在 signal server

交付物：

- 聊天链路先于文件链路稳定

### 阶段 3：文件协议迁移

- 接入 detached `bulk` channel
- 将当前文件 chunk 协议映射到 DataChannel frame
- 保留 resume/压缩/校验/进度逻辑

交付物：

- 小文件、大文件、文件夹传输可用

### 阶段 4：服务端收口

- 删除 `punch_sessions`
- 删除旧 punch 信令
- 自定义 TCP relay 改成 legacy fallback
- TURN 成为正式回退路径

交付物：

- 业务层不再显式管理 NAT 打洞状态机

### 阶段 5：可选统一化

- 评估是否把 LAN 直连也统一到 WebRTC
- 评估是否彻底删除旧 Quinn 路径

## 14. 主要风险

### 14.1 API 风格风险

`v0.17.x` 仍然是 callback 风格较重。

如果回调直接散落到 GUI、signal、transfer 各处，会出现：

- Arc 层层嵌套
- 状态同步难
- 调试困难

解决建议：

- `WebRtcManager` 用 actor/事件模型包一层
- 业务层只消费内部事件

### 14.2 传输语义风险

当前业务建立在 QUIC bi stream 上。

切到 WebRTC 后，如果不先抽象传输层，就会把所有文件协议逻辑都改乱。

解决建议：

- 先抽象 `PeerTransport`
- 再迁移承载

### 14.3 吞吐与内存风险

DataChannel 的缓冲和消息粒度需要精细控制。

解决建议：

- detached mode
- 小 frame
- buffered amount 阈值控制

### 14.4 部署风险

WebRTC 路线通常要新增：

- STUN
- TURN

解决建议：

- 第一阶段直接配 `coturn`
- 不在 signal server 里自己实现 TURN

### 14.5 版本演进风险

官方 README 明确说明：

- 当前稳定生产建议用 `v0.17.x`
- `v0.20.0+` 正在重构

解决建议：

- 锁死版本
- 用适配层隔离 API 变化

## 15. 验收标准

- 双端位于不同 NAT 后，可通过 WebRTC 建链。
- 本端不再依赖 `NatType::is_punchable()` 决定是否进入 P2P 分支。
- 文本消息可通过 `control` DataChannel 稳定直传。
- 文件传输可通过 detached `bulk` DataChannel 完成。
- 对称 NAT 或无法直连时，可自动落到 TURN。
- signal server 仍能维持好友、邀请码、在线状态、离线消息。
- 当前 punch 相关状态机可以关闭，功能不回退。

## 16. 一句话总结

如果使用 `webrtc` crate 改造整个项目的 NAT 打洞流程，本质上不是“换个打洞库”，而是把当前 **手写 STUN + 手写打洞协调 + Quinn/QUIC 传输** 的跨 NAT 主路径，重构为 **WebRTC 的 ICE + STUN/TURN + DataChannel**。

这条路线可行，但它是一次完整的连接层和承载层重构，改造深度明显大于 `iroh` 路线。

## 17. 参考资料

- webrtc crate docs: [https://docs.rs/webrtc/latest/webrtc/](https://docs.rs/webrtc/latest/webrtc/)
- `RTCPeerConnection`: [https://docs.rs/webrtc/latest/webrtc/peer_connection/struct.RTCPeerConnection.html](https://docs.rs/webrtc/latest/webrtc/peer_connection/struct.RTCPeerConnection.html)
- `RTCDataChannel`: [https://docs.rs/webrtc/latest/webrtc/data_channel/struct.RTCDataChannel.html](https://docs.rs/webrtc/latest/webrtc/data_channel/struct.RTCDataChannel.html)
- `SettingEngine`: [https://docs.rs/webrtc/latest/webrtc/api/setting_engine/struct.SettingEngine.html](https://docs.rs/webrtc/latest/webrtc/api/setting_engine/struct.SettingEngine.html)
- `RTCIceServer`: [https://docs.rs/webrtc/latest/webrtc/ice_transport/ice_server/struct.RTCIceServer.html](https://docs.rs/webrtc/latest/webrtc/ice_transport/ice_server/struct.RTCIceServer.html)
- webrtc-rs 仓库 README: [https://github.com/webrtc-rs/webrtc](https://github.com/webrtc-rs/webrtc)
