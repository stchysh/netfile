# 使用 iroh 改造 NAT 打洞与传输方案

## 1. 目的

本文整理 NetFile 当前跨 NAT 建链流程，并说明如果改用 `iroh` crate 来改造整个 NAT 打洞流程，需要做哪些事情。

这份方案重点回答四个问题：

1. `iroh` 适不适合这个项目。
2. 它应该替换当前哪几层。
3. 哪些现有模块可以保留，哪些可以删掉。
4. 应该按什么顺序迁移，风险最低。

## 2. 当前项目的跨 NAT 流程

当前主流程不是直接使用 `hole_punch.rs` 的简单 UDP PUNCH/PUNCH_ACK，而是：

1. `TransferService` 启动时对同一个 `transfer_port` 做 STUN，得到 `public_addr` 和 `nat_type`。
2. GUI 发文件时先尝试局域网直连。
3. 直连失败后，如果本端 NAT 被判定为可打洞，则通过 `SignalClient` 发 `RequestPunch`。
4. 信令服务端协调 `PunchCoordinate` / `PunchRequest` / `PunchReady` / `PunchStart`。
5. 客户端收到打洞协调消息后，调用 `TransferService::punch_hole(peer_addr)`。
6. `punch_hole()` 的实际动作是对目标公网 `transfer_addr` 直接发起 Quinn/QUIC `connect()` 预热。
7. 成功后把连接写入缓存，后续文本和文件传输复用 QUIC 连接；失败再回退到当前自定义 TCP relay。

当前和 NAT 流程直接相关的代码：

- `crates/netfile-core/src/stun.rs`
- `crates/netfile-core/src/hole_punch.rs`
- `crates/netfile-core/src/signal_client.rs`
- `crates/netfile-core/src/transfer/service.rs`
- `crates/netfile-signal/src/protocol.rs`
- `crates/netfile-signal/src/server.rs`
- `crates/netfile-gui/src/lib.rs`

当前方案的问题也很明确：

- NAT 类型判断和打洞时序都由业务层自己维护。
- `RequestPunch` / `PunchReady` / `PunchStart` 是手写状态机。
- relay 是“自定义 TCP 双向转发”，不是一个完整的、和传输栈深度集成的打洞/回退方案。
- 传输层强绑定 Quinn 细节，导致“打洞逻辑”和“文件协议”耦合在一起。

## 3. 先给结论

### 3.1 结论

如果是 `iroh`，它比 `webrtc-rs` 更适合当前项目。

原因很直接：

- 当前项目的 P2P 传输本来就是基于 QUIC 流。
- `iroh` 对外暴露的仍然是 QUIC connection / stream 接口。
- 它把 hole punching、relay 辅助建立连接、地址更新这些复杂部分藏在底层。

所以对于 NetFile，`iroh` 的定位不是“另一个 STUN 库”，而是：

**用 `iroh::Endpoint` 替换当前手写的 STUN + 打洞协调 + 自定义 relay + 部分 Quinn endpoint 管理逻辑，同时尽量保留现有文件协议和 QUIC stream 使用方式。**

### 3.2 和 webrtc-rs 的差异

如果换 `webrtc-rs`，通常意味着：

- NAT 建链层切到 ICE / STUN / TURN / DataChannel。
- 文件传输承载也要逐步切到 DataChannel。

如果换 `iroh`，更合理的路径是：

- 继续走 QUIC。
- 继续走 `open_bi()` / `accept_bi()`。
- 把 NAT 穿透、relay 回退、地址发现、地址更新交给 `iroh`。

因此，对这个项目来说，`iroh` 更像是“更贴近当前架构的重构方案”。

## 4. iroh 现在能提供什么

截至 **2026-03-06**，docs.rs 上 `iroh` crate 的最新版本是 `0.96.1`，`iroh-relay` 也是 `0.96.x` 系列。

官方文档对 `iroh` 的描述很明确：

- 它是 **peer-to-peer QUIC connections**。
- 对外仍然暴露 QUIC 连接和流。
- 底层通过 **hole punching + relay servers** 建立连接。
- 连接建立成功后优先迁移到直连；如果直连不行，可以持续走 relay。

对本项目最重要的能力有：

- `Endpoint`
  - 单应用一个 endpoint，统一管理连接。
- `Builder::secret_key`
  - 持久化身份，生成稳定的 `EndpointId`。
- `Builder::alpns`
  - 配置可接受的应用协议。
- `Builder::relay_mode`
  - 配置默认 relay、禁用 relay 或自定义 relay。
- `Endpoint::connect`
  - 直接连接远端 `EndpointAddr` 或 `EndpointId`。
- `Endpoint::accept`
  - 接收入站连接。
- `Connection::open_bi` / `accept_bi`
  - 和当前 Quinn 使用方式非常接近。
- `Endpoint::addr` / `watch_addr`
  - 获取和订阅当前地址信息变化。
- `Endpoint::online`
  - 等待 endpoint 进入“可被外网拨号”的状态。
- `Endpoint::remote_info`
  - 观察远端的已知地址信息。
- `MdnsDiscovery` / `DnsDiscovery` / `PkarrPublisher`
  - 可选的地址发布与发现能力。
- `iroh-relay`
  - 可自建 relay server。

## 5. 用 iroh 改造后，整体架构应该怎么变

### 5.1 推荐路线

推荐的最小改造路线是：

- 保留当前好友系统、邀请码、离线消息、GUI/TUI/CLI。
- 保留当前文件协议、文本消息协议、断点续传、压缩、历史记录。
- 保留当前局域网 UDP 发现，至少第一阶段保留。
- 把跨 NAT 连接建立层改成 `iroh::Endpoint`。
- 用 `iroh` 提供的 QUIC 连接和流替换当前手管的 Quinn endpoint + STUN watcher + relay fallback。

也就是说，改造后的职责边界应是：

- `signal server`
  - 负责好友关系、在线状态、离线消息、地址分发。
- `iroh`
  - 负责 NAT 穿透、relay 建链、地址变化、P2P QUIC 连接。
- `TransferService`
  - 继续负责业务协议和传输任务，但不再自己决定何时 STUN、何时 punch、何时 relay。

### 5.2 不推荐路线

不建议：

- 继续保留当前 `RequestPunch/PunchStart` 手写流程，只把底层 socket 换成 `iroh`。
- 继续保留当前自定义 TCP relay 作为主路径。
- 同时维护“原 Quinn 打洞”和“iroh 打洞”两套 NAT 流程。

这样只会让连接层重复。

## 6. 需要改哪些东西

### 6.1 新增依赖

建议先加：

```toml
[workspace.dependencies]
iroh = "0.96.1"
```

如果决定自建 relay，可以额外研究：

```toml
iroh-relay = "0.96.1"
```

注意：

- `iroh` 版本迭代较快，建议先锁定具体小版本。
- 先做 PoC，不要一开始就跟着最新 breaking change 滚动升级。

### 6.2 新增独立的 Iroh 连接层

建议在 `netfile-core` 新建一个独立模块，例如：

```text
netfile-core/src/
├── iroh_net/
│   ├── mod.rs
│   ├── manager.rs       # IrohManager：单例 Endpoint 管理
│   ├── address.rs       # EndpointAddr 编解码与对外展示
│   ├── transport.rs     # iroh Connection/Stream 封装
│   ├── cache.rs         # 连接缓存
│   └── observer.rs      # watch_addr / remote_info / metrics
```

`IrohManager` 的职责建议包括：

- 持久化 `SecretKey`
- 创建全局 `Endpoint`
- 配置 ALPN
- 配置 relay mode
- 维护连接缓存
- 对外暴露 `connect(peer_addr)` 和 `accept_loop()`
- 监听 `watch_addr()`，把地址变化同步给 signal server

### 6.3 持久化 iroh 身份

这是改造里非常关键的一点。

`iroh` 的 `EndpointId` 来自 `SecretKey` 的公钥。如果不持久化 `SecretKey`，每次重启都会变成新身份。

所以需要新增稳定身份文件，例如：

```text
~/.netfile/data/iroh/secret_key
```

同时要决定业务身份映射：

### 方案 A：保留当前 `device_id`

- `device_id` 继续作为好友系统和授权系统的业务 ID。
- 每个设备额外维护一个 `iroh_endpoint_id`。

这是最稳妥的方案。

### 方案 B：直接让 `EndpointId` 成为业务 ID

- 好处是身份统一。
- 缺点是会影响现有好友、授权、历史消息、配置和 UI 展示。

不建议一阶段这样做。

### 6.4 Signal 协议要从“打洞协调”改成“地址分发”

当前信令协议中的这些消息可以逐步下线：

- `RequestPunch`
- `PunchReady`
- `PunchCoordinate`
- `PunchRequest`
- `PunchStart`
- `RequestRelay`
- `RelaySession`
- `IncomingRelay`
- `RelayUnavailable`

改造后，signal server 的 NAT 相关职责应该简化成：

1. 存储在线设备当前的 `iroh` 地址信息。
2. 把地址信息同步给好友。
3. 在地址变化时通知对端刷新缓存。

建议新增的信令字段或消息示例：

```json
{
  "type": "register",
  "device_id": "...",
  "instance_name": "...",
  "iroh_endpoint_id": "...",
  "iroh_relay_url": "https://relay.example.com",
  "iroh_direct_addrs": ["1.2.3.4:45678", "10.0.0.2:45678"]
}
```

或独立更新消息：

```json
{
  "type": "update_iroh_addr",
  "iroh_endpoint_id": "...",
  "iroh_relay_url": "https://relay.example.com",
  "iroh_direct_addrs": ["1.2.3.4:45678"]
}
```

如果希望更贴近 `iroh` 原生类型，也可以直接传序列化后的 `EndpointAddr`，但工程上更建议先拆成可读字段：

- `endpoint_id`
- `relay_url`
- `direct_addrs`

这样调试简单很多。

### 6.5 用 watch_addr 替换当前 STUN watcher

当前 GUI 里有一个定时 STUN watcher，会：

- 定期刷新公网地址
- 更新 NAT 类型
- 调 `SignalClient::update_transfer_addr`

如果改成 `iroh`，这部分建议改成：

1. `Endpoint` 启动后，必要时等待一次 `online()`。
2. 通过 `watch_addr()` 订阅当前 `EndpointAddr`。
3. 每次 `EndpointAddr` 变化时，把新的：
   - `relay_url`
   - `direct_addrs`
   - `endpoint_id`
   同步到 signal server。

这会直接替代：

- 手动 STUN 刷新公网地址
- `update_transfer_addr`
- 一部分 NAT 诊断逻辑

需要注意的官方行为：

- `online()` 没有超时，外层要自己包 `timeout`。
- `online()` 依赖 relay 可达，如果应用希望支持纯 LAN 离线场景，启动时不要阻塞等它。

### 6.6 TransferService 需要从“自己持有 Quinn endpoint”改成“依赖 Iroh transport”

这部分是主要代码改造点。

当前 `TransferService` 自己负责：

- 绑定 UDP socket
- 创建 Quinn endpoint
- `get_or_connect()`
- `punch_hole()`
- 连接缓存

改造后建议：

- `TransferService` 不再创建 Quinn endpoint。
- 它只依赖 `IrohManager` 或统一的传输抽象。

可以抽出一个统一接口：

```text
trait PeerConnector {
    async fn connect(&self, peer: PeerAddr) -> anyhow::Result<PeerConnection>;
    async fn accept(&self) -> anyhow::Result<PeerConnection>;
}
```

再把 `PeerConnection` 封装成：

```text
trait PeerConnection {
    async fn open_bi(&self) -> anyhow::Result<(SendSide, RecvSide)>;
    async fn accept_bi(&self) -> anyhow::Result<(SendSide, RecvSide)>;
    fn close(&self);
}
```

这样业务层基本不用关心底下是不是 `quinn::Connection` 还是 `iroh::endpoint::Connection`。

### 6.7 文件协议可以大概率保留

这是 `iroh` 相比 `webrtc-rs` 的最大优势之一。

当前项目的文件协议建立在：

- QUIC 双向流
- 先发 `TransferRequest`
- 再按 chunk 发送
- 最后 `TransferComplete`

而 `iroh` 连接建立后仍然是 QUIC 流：

- `open_bi()`
- `accept_bi()`
- `open_uni()`
- `accept_uni()`

所以：

- `TransferRequest/Response/Complete/Error`
- chunk 传输
- 压缩
- 断点续传
- 文本消息

这些大部分逻辑都可以保留。

需要注意一个 `iroh`/QUIC 的行为细节：

- 调用 `open_bi()` 后，必须先真正写出数据，对端 `accept_bi()` 才会感知到这个流。

而当前项目在打开流后马上就会写 `TransferRequest` 或 `TextMessage`，这和 `iroh` 的使用方式是兼容的。

### 6.8 当前自定义 TCP relay 应该降级为 legacy fallback

如果全链路切到 `iroh`，当前 `netfile-signal --relay-port` 这套文件中继不应该再作为主路径。

因为 `iroh` 自己就有 relay 体系：

- 初始连接可借助 relay 建立。
- 能直连就迁移到直连。
- 不能直连也可以持续走 relay。

因此：

- 一阶段可以保留当前 TCP relay，防止新方案不稳定时兜底。
- 二阶段以后应把它降级成可选 legacy fallback。
- 三阶段可以完全下线。

如果产品化部署，建议不要依赖公共默认 relay，而是自建 `iroh-relay`，然后使用 `RelayMode::Custom`。

### 6.9 Signal server 的 punch session 可以删掉

当前服务端 `punch_sessions` 的职责，是手工同步双端打洞时序。

改成 `iroh` 后，这一层不应再存在。

signal server 侧与 NAT 穿透有关的核心状态，应该只剩：

- 在线设备 -> 当前 iroh 地址信息
- 地址变更通知
- 好友可见性控制

这样会直接删掉一类复杂状态：

- `initiator_ready`
- `target_ready`
- `created_at`
- 超时回收
- `PunchStart` 双端同时发令

## 7. 配置项建议

建议新增独立的 `[iroh]` 配置，而不是继续把所有东西塞进现有网络字段。

```toml
[iroh]
enabled = true
alpn = "netfile/1"
secret_key_path = "~/.netfile/data/iroh/secret_key"

[iroh.relay]
mode = "custom"   # default | custom | disabled
urls = ["https://relay.example.com"]

[iroh.discovery]
use_signal_addr_exchange = true
enable_mdns = false
enable_dns = false
enable_pkarr = false
```

建议解释：

- `enabled`
  - 是否启用 iroh 连接层。
- `alpn`
  - 应用协议名，例如 `netfile/1`。
- `secret_key_path`
  - 稳定身份文件路径。
- `relay.mode`
  - 第一阶段建议 `custom`。
- `use_signal_addr_exchange`
  - 第一阶段建议 `true`，继续依赖 signal server 同步地址。
- `enable_mdns`
  - 可作为后续替代现有 LAN 发现的实验项。

## 8. NAT 流程改造后会变成什么样

### 8.1 当前流程

```text
LAN 直连失败
-> signal 请求 punch
-> 服务端协调 punch session
-> 双端本地 QUIC connect 预热
-> 成功复用 QUIC
-> 失败请求 relay
-> 服务端做 TCP 中继
```

### 8.2 改造后流程

```text
本地启动单一 iroh Endpoint
-> endpoint 选择 relay / 收集 direct addrs
-> watch_addr 把地址变化同步到 signal server
-> 发起方从 signal server 获取对端 EndpointAddr
-> endpoint.connect(peer_addr, ALPN)
-> iroh 底层完成 relay-assisted connect + hole punching
-> 成功后得到 QUIC connection
-> 业务层继续 open_bi()/accept_bi() 发文本/文件
-> 如果始终不能直连，则连接继续走 iroh relay
```

这里最重要的变化是：

**业务层不再自己决定“现在该打洞还是该 relay”，而是只负责“发起连接”。**

## 9. 最小改造方案

这是最推荐的第一阶段方案。

### 保留

- `DiscoveryService` 的 UDP 局域网发现
- `SignalClient`
- `netfile-signal` 的好友、邀请、在线状态、离线消息
- `TransferService` 的业务协议
- GUI/TUI/CLI 层调用结构

### 替换

- `stun.rs`
- `hole_punch.rs`
- `TransferService` 里的 Quinn endpoint 创建
- `TransferService::punch_hole`
- `SignalClient::request_punch`
- `SignalClient::request_relay`
- `signal server` 的 punch/relay 状态机

### 新增

- `IrohManager`
- iroh 地址同步协议
- iroh 地址缓存
- 连接诊断和 metrics

## 10. 更深层的 iroh 化

如果后面想进一步减少自研网络逻辑，还可以继续做这些事。

### 10.1 用 MdnsDiscovery 替代部分 LAN UDP 发现

`iroh` 提供 `MdnsDiscovery`，可以发现局域网内 endpoint。

但对当前项目来说，现有 UDP 发现除了地址以外，还带有：

- 设备名
- 实例名
- 版本
- UI 所需元数据

所以不建议第一阶段直接替换。

更合理的方式是：

- 第一阶段保留现有 `DiscoveryService`。
- 第二阶段把 `MdnsDiscovery` 作为地址发现补充信息源。

### 10.2 用 Address Lookup / DNS / pkarr 替代 signal 地址分发

`iroh` 允许：

- 用 DNS discovery
- 用 pkarr publisher / resolver
- 甚至只用 `EndpointId` 连接

但对当前项目来说，signal server 还承担：

- 好友关系
- 在线状态
- 离线消息

因此第一阶段没必要把地址发布再外移到独立 discovery 基础设施。

更现实的做法是：

- 先继续通过 signal server 分发 `EndpointAddr`。
- 后续如果要做“无中心服务器可连接”，再考虑 pkarr / DNS。

## 11. 推荐迁移步骤

### 阶段 0：PoC

- 引入 `iroh = "0.96.1"`
- 写一个最小 endpoint demo
- 验证 `connect`、`accept`、`open_bi`、`accept_bi`
- 在双 NAT Docker 环境里验证能否打通

交付物：

- 两个测试节点能通过 iroh 建立连接并交换简单消息。

### 阶段 1：基础设施接入

- 新增 `IrohManager`
- 持久化 `SecretKey`
- 配置 ALPN
- 配置 `RelayMode::Custom`
- 自建或接入 `iroh-relay`

交付物：

- 单应用单 endpoint 可运行。
- endpoint 地址可通过 `watch_addr` 观察到变化。

### 阶段 2：地址同步改造

- 扩展 signal 协议，增加 iroh 地址字段
- 设备上线时注册地址
- 地址变化时增量更新
- 好友在线通知里带上 iroh 地址信息

交付物：

- UI 和业务层可以拿到对端 `EndpointAddr`。

### 阶段 3：连接层替换

- `TransferService` 改为依赖 `IrohManager`
- 用 iroh connection 替代 Quinn connection cache
- 删除 `punch_hole()` 主路径
- 发送文件和发送文本改为直接 `connect(peer_addr)`

交付物：

- 文件和文本消息都能通过 iroh 连接完成。

### 阶段 4：服务端状态收口

- 删除 punch 相关信令消息
- 删除 `punch_sessions`
- 下线 `request_punch()` 相关 UI 和调用链
- 当前 TCP relay 改成可选 legacy fallback

交付物：

- 业务层不再显式感知 NAT 打洞状态机。

### 阶段 5：可选的进一步简化

- 评估是否接入 `MdnsDiscovery`
- 评估是否接入 DNS/pkarr 地址发布
- 决定是否彻底删除 legacy relay

## 12. 需要特别注意的风险

### 12.1 公共 relay 不适合作为正式生产依赖

官方默认 `RelayMode::Default` 使用的是 n0 的公共 relay。

PoC 可以用。
正式产品不建议长期依赖。

更合理的是：

- 自建 `iroh-relay`
- 使用 `RelayMode::Custom`

### 12.2 EndpointId 和业务身份不是一回事

`iroh` 会保证：

- 你连上的确实是那个 `EndpointId`

但它不会自动保证：

- 这个节点就是你的“好友”
- 这个节点被允许传文件

所以业务授权仍然要继续保留在应用层。

### 12.3 iroh 版本变化较快

`iroh` 近几个版本 API 还在持续迭代，尤其是 relay 和连接相关能力。

建议：

- 锁版本
- 用适配层包住
- 业务层不要直接散落大量 iroh API 调用

### 12.4 online() 的使用时机要谨慎

官方文档明确说：

- `online()` 没有超时
- 它依赖 relay 可达
- 无 WAN 场景可能一直等待

所以：

- 不要把它写成应用启动强阻塞前置条件
- 最好只在需要外网可拨号时有限时地等待

### 12.5 当前文件协议仍要验证性能

虽然 iroh 仍然是 QUIC stream，但仍要验证：

- 大文件吞吐
- 多文件并发
- 断点续传
- 压缩前后速率
- relay 下的表现

理论上改动小，不等于实际不用测。

## 13. 验收标准

- 不再依赖 `stun.rs` 和手写 `RequestPunch` 状态机建立跨 NAT 连接。
- 发送文件时业务层只负责 `connect`，不再显式写 punch / relay 分支。
- `TransferRequest/Response/Complete` 协议仍可工作。
- 文本消息和文件传输都可以通过 iroh connection 完成。
- symmetric NAT 或无法直连时，可以自动走 iroh relay。
- signal server 仍能维护好友、在线状态和离线消息。
- 地址变化时，`watch_addr` 可以驱动 signal 中的地址同步。
- 现有 GUI 主流程不需要理解 ICE/STUN/TURN 这类额外状态机。

## 14. 一句话总结

如果用 `iroh` 改造 NetFile 的 NAT 打洞流程，最合理的做法不是“再做一层新的打洞状态机”，而是把当前**手写 STUN + 手写打洞协调 + 手写 relay fallback**整体替换成 `iroh::Endpoint` 提供的连接能力，同时保留现有 QUIC 流上的业务协议。

对这个项目来说，`iroh` 的优势在于：

- NAT 穿透逻辑可以大幅收口。
- 文件传输协议大部分可以保留。
- 改造成本明显低于 WebRTC/DataChannel 路线。

## 15. 参考资料

- iroh crate docs: [https://docs.rs/iroh/latest/iroh/](https://docs.rs/iroh/latest/iroh/)
- `Endpoint` API: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Endpoint.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Endpoint.html)
- `Builder` API: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Builder.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Builder.html)
- `Connection` API: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Connection.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Connection.html)
- discovery 模块: [https://docs.rs/iroh/latest/iroh/discovery/](https://docs.rs/iroh/latest/iroh/discovery/)
- `MdnsDiscovery`: [https://docs.rs/iroh/latest/iroh/discovery/mdns/](https://docs.rs/iroh/latest/iroh/discovery/mdns/)
- `protocol::Router`: [https://docs.rs/iroh/latest/iroh/protocol/](https://docs.rs/iroh/latest/iroh/protocol/)
- iroh-relay crate docs: [https://docs.rs/iroh-relay/latest/iroh_relay/](https://docs.rs/iroh-relay/latest/iroh_relay/)
- iroh 官方博客，relay 与连接 API 更新： [https://www.iroh.computer/blog/iroh-0-95-0-new-relay](https://www.iroh.computer/blog/iroh-0-95-0-new-relay)
