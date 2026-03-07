# 使用 webrtc-rs 改造 NAT 打洞逻辑方案

## 1. 目的

本文整理 NetFile 当前跨 NAT 建链逻辑，并给出如果改用 `webrtc-rs` 进行重构时，需要新增、替换和保留的内容。

目标不是单纯“把 UDP 打洞改个库”，而是回答三个更关键的问题：

1. `webrtc-rs` 在这个项目里应该替换哪一层。
2. 需要改动哪些模块、协议和部署组件。
3. 应该按什么顺序迁移，风险最低。

## 2. 当前实现现状

当前跨 NAT 主流程并不是直接使用 `netfile-core/src/hole_punch.rs`，而是：

1. `TransferService` 在启动时对 `transfer_port` 做 STUN，拿到 `public_addr` 和 `nat_type`。
2. GUI 发送文件时先尝试局域网直连。
3. 直连失败后，如果本端 NAT 被判断为可打洞，则通过 `SignalClient` 发起 `RequestPunch`。
4. 信令服务端协调 `PunchCoordinate` / `PunchRequest` / `PunchReady` / `PunchStart`。
5. 客户端收到协调消息后，调用 `TransferService::punch_hole(peer_addr)`。
6. `punch_hole()` 的实际动作不是“发 PUNCH 包”，而是直接对对端公网 `transfer_addr` 发起 QUIC `connect()` 预热。
7. 建链成功后，后续文本/文件传输复用 QUIC 连接；失败则再回退到当前自定义 TCP relay。

和当前流程直接相关的代码：

- `crates/netfile-core/src/stun.rs`
- `crates/netfile-core/src/signal_client.rs`
- `crates/netfile-core/src/transfer/service.rs`
- `crates/netfile-signal/src/protocol.rs`
- `crates/netfile-signal/src/server.rs`
- `crates/netfile-gui/src/lib.rs`

当前方案的特点：

- 优点：实现简单，和现有 QUIC 文件传输耦合紧密。
- 缺点：NAT 判定、打洞时序、重试、回退、会话同步都由业务层自己维护。
- 限制：当前 relay 是“自定义 TCP 管道”，并不是真正的 TURN。
- 限制：当前“是否进入打洞分支”主要看本端 NAT 类型，而不是 ICE 那种双端候选对和连通性检查。

## 3. 先给结论

### 3.1 推荐路线

如果要用 `webrtc-rs` 改造 NAT 打洞，**推荐的方向不是“继续保留现在的 QUIC 打洞，只把 STUN/打洞协调换成 WebRTC”**，而是：

**把“跨 NAT P2P 建链”整体替换成 WebRTC 的 ICE + STUN/TURN + DataChannel。**

即：

- 保留当前信令服务器，但把它改成 WebRTC SDP/ICE 转发器。
- 用 `RTCPeerConnection` 负责候选收集、连通性检查、NAT 穿透、TURN 回退。
- 用 `RTCDataChannel` 承载控制消息和文件数据。
- 当前自定义 TCP relay 先作为过渡兜底保留，后续再收敛到标准 TURN。

### 3.2 不推荐路线

不建议把 `webrtc-rs` 只当成“新的打洞器”，然后最终仍然把流量切回当前 Quinn/QUIC 连接。原因是：

- `webrtc-rs` 的价值主要在完整 ICE 状态机，而不是单个 STUN 查询。
- `RTCPeerConnection` 建链成功后默认承接的是 DTLS/SCTP/DataChannel，而不是把一个已经打好的 UDP 五元组回交给外部 QUIC。
- 这样会同时维护两套跨 NAT 连接体系：WebRTC 一套，QUIC 一套，复杂度会上升而不是下降。

结论上，更合适的改造方式是：

- **局域网内直连**：可以继续保留现有快速路径。
- **跨 NAT 建链**：切到 WebRTC。
- **离线消息**：继续走现有信令服务器。
- **文件/文本实时直传**：逐步迁移到 DataChannel。

## 4. 为什么 webrtc-rs 适合做这件事

截至 **2026-03-06**，`webrtc` crate 在 docs.rs 的最新稳定版本是 `0.17.1`。官方仓库 README 也明确说明：

- `v0.17.x` 适合基于 Tokio 的生产使用。
- `master / v0.20.0+` 正在做新的 Sans-I/O 架构，暂时不适合作为当前项目的直接落地目标。

`webrtc-rs` 对这个项目真正有价值的能力包括：

- `RTCIceServer`：直接声明 STUN / TURN 地址、用户名、密码。
- `RTCConfiguration`：通过 `ice_servers` 配置候选收集与 relay。
- `RTCPeerConnection`：提供 `create_offer`、`create_answer`、`set_local_description`、`set_remote_description`、`on_ice_candidate`、`add_ice_candidate`、`restart_ice`、`get_stats` 等完整建链能力。
- `SettingEngine`：可以设置 ICE 超时、网络类型过滤、网卡/IP 过滤、1:1 NAT IP、DataChannel detach 等。
- `RTCDataChannel`：可以承载任意二进制数据。

对 NetFile 来说，最关键的一点是：

**当前自己写的“打洞协调状态机”，本质上可以交给 ICE 去做。**

## 5. 需要做的东西

### 5.1 依赖与版本策略

需要新增的核心依赖：

```toml
[workspace.dependencies]
webrtc = "0.17.1"
```

建议：

- 先固定在 `0.17.1`，不要直接跟 `master` 或未来的 `0.20` alpha。
- 当前项目已经是 Tokio 运行时，和 `0.17.x` 的使用方式匹配。
- 文档和 PoC 稳定后，再评估是否迁移到新架构分支。

### 5.2 新的连接分层

建议把现有“传输层”和“建链层”拆开。

### 当前状态

- `TransferService` 同时负责：
  - 本地监听
  - STUN
  - NAT 类型判断
  - QUIC endpoint 管理
  - 连接缓存
  - 文件/文本协议

### 改造后建议

新增一个独立的 WebRTC 建链层，例如：

```text
netfile-core/src/
├── webrtc/
│   ├── mod.rs
│   ├── manager.rs          # WebRTCManager，管理 peer session 生命周期
│   ├── session.rs          # 单个对端的 RTCPeerConnection + DataChannel
│   ├── signaling.rs        # SDP/ICE 信令编解码
│   ├── transport.rs        # DataChannel 包装成统一传输接口
│   └── stats.rs            # 连接状态、candidate pair、RTT、relay 命中统计
```

同时抽出统一的传输抽象：

```text
trait PeerTransport {
    async fn send_frame(&self, frame: &[u8]) -> anyhow::Result<()>;
    async fn recv_frame(&self) -> anyhow::Result<Vec<u8>>;
    async fn close(&self) -> anyhow::Result<()>;
}
```

这样可以同时兼容：

- `QuicTransport`：旧实现，迁移期保留。
- `WebRtcDataChannelTransport`：新实现，逐步切换。

### 5.3 信令协议需要重写

当前 `RequestPunch/PunchReady/PunchStart` 这一整套协议，会被 WebRTC 的 SDP + ICE candidate 交换替代。

建议新增以下消息：

```json
{"type":"webrtc_offer","target_device_id":"...","session_id":"...","sdp":"..."}
{"type":"webrtc_answer","target_device_id":"...","session_id":"...","sdp":"..."}
{"type":"webrtc_ice_candidate","target_device_id":"...","session_id":"...","candidate":"...","sdp_mid":"0","sdp_mline_index":0}
{"type":"webrtc_ice_done","target_device_id":"...","session_id":"..."}
{"type":"webrtc_close","target_device_id":"...","session_id":"...","reason":"..."}
{"type":"webrtc_restart","target_device_id":"...","session_id":"..."}
```

服务端需要新增会话状态：

```text
session_id ->
  initiator_id
  target_id
  offer / answer
  pending_local_candidates
  pending_remote_candidates
  state
  created_at
```

需要特别处理的点：

- 对端还没 `set_remote_description()` 前，先缓存早到的 candidate。
- session 要有 TTL，避免信令断流后长期残留。
- 重连或 `restart_ice()` 时需要 session 级状态更新，而不是沿用旧 `punch_session`。

### 5.4 当前 signal server 还要保留，但职责会变

改造后信令服务端仍然需要存在，但职责变化如下：

### 保留

- 注册/在线状态
- 好友关系
- 邀请码
- 离线消息

### 删除或降级

- `punch_sessions`
- `RequestPunch` / `PunchReady` / `PunchStart` 协调逻辑

### 新增

- SDP 转发
- trickle ICE candidate 转发
- WebRTC session 生命周期管理
- 可选 TURN 服务信息下发

也就是说：

- 现在的 signal server 是“手写 NAT 打洞协调器”。
- 改造后它更像“WebRTC signaling server”。

### 5.5 TransferService 需要从“依赖 QUIC”改成“依赖传输抽象”

这是整个改造里工作量最大的部分。

当前文件传输核心强依赖：

- `quinn::Connection`
- `open_bi()`
- 自定义消息读写
- QUIC 连接缓存

如果切到 WebRTC，需要把这些 Quinn 细节从业务协议里抽出来。

建议拆成两层：

### 业务协议层

继续保留这些业务概念：

- `TransferRequest`
- `TransferResponse`
- `TransferComplete`
- `TransferError`
- 文本消息
- 断点续传
- 压缩
- 进度统计

### 传输承载层

把“底下到底是 QUIC stream 还是 DataChannel”做成可替换实现。

### 5.6 文件传输不要直接走默认 on_message，建议用 detached DataChannel

这是一个关键点。

官方文档里，`RTCDataChannel::on_message()` 当前说明：

- 默认回调模式下消息接收尺寸有限。
- 如果要更大的消息，应该使用 `detach()`。

`SettingEngine::detach_data_channels()` 也明确要求：

- 先开启 detach 模式。
- 在 `OnOpen` 里对 DataChannel 执行 `detach()`。

因此，**如果要把现在的文件传输迁移到 DataChannel，推荐方案是“detached DataChannel + 自己的分帧协议”**，而不是直接把现在 1MB 级 chunk 原样塞给 `on_message()`。

### 推荐做法

引入两个尺寸概念，不要再混成一个：

1. **业务块大小**
   - 用于断点续传、哈希校验、进度统计。
   - 可以继续保留现在的 `chunk_size` 语义。

2. **传输帧大小**
   - 用于 DataChannel 的真实发送粒度。
   - 建议比业务块更小，比如 64 KiB 到 256 KiB 范围内调优。

这样做的好处：

- 断点续传语义不用大改。
- 发送缓存和背压更容易控制。
- 不会因为单帧过大导致 DataChannel 缓冲暴涨。

### 5.7 控制面和数据面建议拆成两个 DataChannel

建议至少拆成两个通道：

- `control`：可靠、有序，用于 `TransferRequest`、`TransferResponse`、`Ack`、文本消息、状态同步。
- `bulk`：可靠、有序，专门传输文件数据帧。

这样做的原因：

- 控制消息不会被大文件数据淹没。
- 更容易做错误恢复和调试。
- 后续如果要实验“非严格有序”或不同重传策略，控制通道不用动。

### 5.8 NAT 类型检测不再作为主决策条件

当前逻辑里，`stun.rs` 的 `NatType::is_punchable()` 会直接影响是否发起 `request_punch()`。

切到 WebRTC 后，这种“业务层先验 NAT 分类”应当降级为：

- 日志与诊断信息
- UI 展示
- 连接统计

而不应该继续作为主要决策条件。

因为 WebRTC/ICE 的主路径应该是：

1. 收集 host / srflx / relay candidate。
2. 双端交换 candidate。
3. 进行连通性检查。
4. 自动选择能打通的 candidate pair。

这时“本端是 cone 还是 symmetric”不再需要由业务逻辑先分支决定。

### 5.9 TURN 需要真正引入，而不是沿用当前 TCP relay 伪装

当前项目的 relay：

- 本质是服务端把两条 TCP 连接 `copy_bidirectional()` 起来。
- 它可以做兜底，但不是 WebRTC 里的 TURN。

如果跨 NAT 部分改成 WebRTC，那么要么：

1. 先保留当前 TCP relay，作为迁移期最后兜底。
2. 正式接入标准 TURN 服务，例如 `coturn`。

从工程性和上线速度看，推荐：

- **第一阶段直接部署 coturn**。
- 不建议第一阶段自己写 TURN 服务器。

原因：

- `webrtc-rs` 的 `RTCIceServer` 已经支持 STUN/TURN URL、用户名、密码。
- 标准 TURN 部署成熟，验证成本低。
- 当前 netfile-signal 更适合做 signaling，而不是顺手再实现完整 TURN。

### 5.10 配置项需要扩展

建议在配置里增加独立的 WebRTC 配置，而不是继续复用当前 `signal_server_addr` 语义：

```toml
[webrtc]
enabled = true
prefer_webrtc = true

[[webrtc.ice_servers]]
urls = ["stun:stun.cloudflare.com:3478"]

[[webrtc.ice_servers]]
urls = ["turn:turn.example.com:3478?transport=udp"]
username = "netfile"
credential = "secret"

[webrtc.tuning]
transport_frame_size = 65536
ice_disconnected_timeout_ms = 5000
ice_failed_timeout_ms = 25000
ice_keepalive_interval_ms = 2000
```

如果是 native-only 设备间通信，还要补几个策略决定：

- 是否关闭 mDNS candidate。
- 是否只允许 UDP network type。
- 是否配置网卡/IP 过滤。

这些都可以通过 `SettingEngine` 做。

### 5.11 安全模型要重新厘清

当前 QUIC 传输已经有一套自己的证书与 TLS 逻辑。

切到 WebRTC 后，传输层会变成：

- ICE
- DTLS
- SCTP
- DataChannel

这里有一个很重要的边界：

- **DTLS 证书只负责传输层安全，不应直接等同于业务身份。**
- 业务身份仍然应该继续使用当前的 `device_id`、好友关系、授权逻辑。

因此需要保留：

- 当前信令注册身份
- 当前好友校验
- 当前授权/密码逻辑

不要把“WebRTC 建起来了”误当成“业务身份认证已经完成”。

### 5.12 连接观测和调试能力要补齐

如果不用手写打洞，调试面必须转向 WebRTC 状态和统计。

建议至少补这些日志/指标：

- `peer_connection_state`
- `ice_connection_state`
- `ice_gathering_state`
- 当前 candidate 类型：`host` / `srflx` / `relay`
- 是否命中 TURN
- 建链耗时
- RTT / 吞吐 / 重传情况
- DataChannel buffered amount

可以利用：

- `on_ice_candidate`
- `on_ice_connection_state_change`
- `on_peer_connection_state_change`
- `get_stats()`

这样 UI 也能展示出“当前连接走的是局域网 / 公网直连 / TURN 中继”。

## 6. 推荐迁移步骤

为了不把现有传输一次性打碎，建议按阶段推进。

### 阶段 0：准备期

- 新增 `webrtc = "0.17.1"` 依赖。
- 新建 `netfile-core::webrtc` 模块。
- 设计新的 signaling message。
- 在 signal server 上实现 SDP/ICE 转发最小闭环。

交付物：

- 两端能通过信令交换 offer / answer / candidate。
- 能打印出完整 ICE 状态变化。

### 阶段 1：最小可用 PoC

- 建立 `RTCPeerConnection`。
- 创建 `control` DataChannel。
- 收发纯文本 ping/pong。
- 在双 NAT Docker 环境里验证能否打通。

交付物：

- 不传文件，只验证建链成功率和状态机稳定性。

### 阶段 2：文本消息迁移

- 把当前点对点在线文本消息优先改走 WebRTC `control` channel。
- 离线消息仍然留在 signal server。

交付物：

- 在线消息 direct path 走 WebRTC。
- 离线消息继续可用。

### 阶段 3：文件协议迁移

- 引入 detached `bulk` DataChannel。
- 把当前文件协议从 Quinn `open_bi()` 迁到统一 `PeerTransport`。
- 调整 chunk/frame 分层。
- 保留断点续传、压缩、哈希校验、进度逻辑。

交付物：

- WebRTC DataChannel 上的单文件和文件夹传输可用。

### 阶段 4：TURN 正式接入

- 部署 coturn。
- 配置 TURN 凭据下发。
- 验证 symmetric NAT 下 relay 自动生效。

交付物：

- 不再依赖当前自定义 TCP relay 也能完成跨 NAT 传输。

### 阶段 5：旧逻辑收口

- 下线 `RequestPunch/PunchReady/PunchStart` 主路径。
- `stun.rs` 降级为诊断用途，或彻底删除。
- `hole_punch.rs` 标记为历史实验代码或移除。
- 当前自定义 TCP relay 改为可选 legacy fallback，后续再删除。

## 7. 需要明确的技术决策

在正式开工前，建议先定以下决策。

### 决策 A：WebRTC 只管跨 NAT，还是统一取代所有 P2P

推荐：

- 局域网发现后仍可保留当前直连快捷路径。
- 但跨 NAT 建链统一使用 WebRTC。

这样能减少一次性重构范围。

### 决策 B：文件是否直接迁到 DataChannel

推荐：

- 是。
- 并且采用 detached DataChannel。

不建议继续保留“WebRTC 建链 + 外部 QUIC 传文件”双栈方案。

### 决策 C：TURN 是自己写还是直接上 coturn

推荐：

- 第一阶段直接上 coturn。

### 决策 D：webrtc-rs 版本选型

推荐：

- 当前项目落地选 `0.17.1`。
- 不直接跟 `master` / `0.20.0+` 开发分支。

## 8. 主要风险

### 8.1 API 风格风险

`webrtc-rs 0.17.x` 还是 callback 风格较重，内部如果直接把回调散落到 GUI、signal、transfer 各处，代码会变乱。

应对方式：

- 在 `netfile-core::webrtc` 内做一层 actor/manager 封装。
- GUI 和 `TransferService` 只和自己的内部事件接口交互。

### 8.2 吞吐与内存背压风险

大文件传输时，如果不处理好：

- 发送帧大小
- buffered amount
- 压缩后瞬时峰值

很容易出现内存上涨或通道阻塞。

应对方式：

- transport frame 做小。
- 控制 `buffered_amount_low_threshold`。
- 发送端做节流和窗口控制。

### 8.3 断点续传语义风险

当前断点续传建立在“可靠流 + chunk index”之上。

如果迁移时把 chunk/frame 混在一起，恢复逻辑会变复杂。

应对方式：

- 保留业务 chunk 语义。
- 单独引入 transport frame，不影响 resume 索引。

### 8.4 部署复杂度风险

项目现在只需要 signal server 和可选 relay port。

切 WebRTC 后，正式上线最好增加：

- STUN
- TURN

应对方式：

- 先 PoC。
- 后接 coturn。
- 部署文档独立维护。

## 9. 验收标准

完成迁移后，至少要满足以下验收条件：

- 双端位于不同 NAT 后，可通过 WebRTC 建链成功。
- 本端不再依赖 `NatType::is_punchable()` 决定是否尝试跨 NAT P2P。
- 文本消息可通过 DataChannel 直传。
- 文件传输可通过 DataChannel 完成，且支持断点续传。
- symmetric NAT 场景下可自动落到 TURN。
- UI 可展示当前链路类型：host / srflx / relay。
- 旧的 QUIC 手工打洞路径可以被关闭，功能不回退。

## 10. 建议的落地清单

- [ ] 新增 `webrtc` 依赖并固定版本
- [ ] 新建 `netfile-core::webrtc` 模块
- [ ] 扩展 signal protocol，支持 offer/answer/candidate
- [ ] signal server 新增 WebRTC session 管理
- [ ] 封装 `WebRTCManager` / `WebRTCSession`
- [ ] 建立 `control` DataChannel PoC
- [ ] 引入 detached `bulk` DataChannel
- [ ] 抽象 `PeerTransport`
- [ ] 把文件协议从 Quinn 细节中解耦
- [ ] 补齐连接状态日志和统计
- [ ] 接入 coturn
- [ ] 双 NAT / symmetric NAT / relay 回退测试
- [ ] 收口旧 `RequestPunch` 主路径

## 11. 一句话总结

如果使用 `webrtc-rs` 改造 NetFile 的 NAT 打洞逻辑，真正要做的不是“换一个 STUN/打洞库”，而是把**跨 NAT 建链层**从“手写 STUN + 手写打洞协调 + QUIC 预热”切换为“标准 ICE + STUN/TURN + DataChannel”，同时把当前 `TransferService` 从 Quinn 细节中解耦出来。

这件事可做，但它是一次**连接层与传输层边界重构**，不是一个小补丁。

## 12. 参考资料

- webrtc crate 0.17.1 docs: [https://docs.rs/webrtc/latest/webrtc/](https://docs.rs/webrtc/latest/webrtc/)
- webrtc-rs 官方仓库 README: [https://github.com/webrtc-rs/webrtc](https://github.com/webrtc-rs/webrtc)
- `RTCPeerConnection` API: [https://docs.rs/webrtc/latest/webrtc/peer_connection/struct.RTCPeerConnection.html](https://docs.rs/webrtc/latest/webrtc/peer_connection/struct.RTCPeerConnection.html)
- `RTCConfiguration`: [https://docs.rs/webrtc/latest/webrtc/peer_connection/configuration/struct.RTCConfiguration.html](https://docs.rs/webrtc/latest/webrtc/peer_connection/configuration/struct.RTCConfiguration.html)
- `RTCIceServer`: [https://docs.rs/webrtc/latest/webrtc/ice_transport/ice_server/struct.RTCIceServer.html](https://docs.rs/webrtc/latest/webrtc/ice_transport/ice_server/struct.RTCIceServer.html)
- `SettingEngine`: [https://docs.rs/webrtc/latest/webrtc/api/setting_engine/struct.SettingEngine.html](https://docs.rs/webrtc/latest/webrtc/api/setting_engine/struct.SettingEngine.html)
- `RTCDataChannel`: [https://docs.rs/webrtc/latest/webrtc/data_channel/struct.RTCDataChannel.html](https://docs.rs/webrtc/latest/webrtc/data_channel/struct.RTCDataChannel.html)
