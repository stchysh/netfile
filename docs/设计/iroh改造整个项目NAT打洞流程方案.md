# 使用 iroh crate 改造整个项目 NAT 打洞流程方案

## 1. 目标

本文从整个项目的角度，整理如果使用 `iroh` crate 改造 NetFile 的 NAT 打洞流程，需要做哪些事情。

这里讨论的不是“把现有 STUN 查询换成另一个库”，而是回答这些问题：

1. `iroh` 适不适合当前项目。
2. 它应该替换掉当前哪一层。
3. 信令、传输、relay、配置要改到什么程度。
4. 哪些现有模块可以保留，哪些应该退出主路径。
5. 应该按什么顺序迁移，成本最低。

## 2. 当前项目现状

当前跨 NAT 主流程大致如下：

1. `TransferService` 启动时对 `transfer_port` 做 STUN，得到 `public_addr` 和 `nat_type`。
2. GUI 发文件时先尝试局域网地址直连。
3. 直连失败后，如果本端 NAT 被判断为可打洞，则通过 `SignalClient` 发 `RequestPunch`。
4. `netfile-signal` 协调 `PunchCoordinate` / `PunchRequest` / `PunchReady` / `PunchStart`。
5. 客户端收到协调消息后，调用 `TransferService::punch_hole(peer_addr)`。
6. `punch_hole()` 实际上不是简单 UDP PUNCH，而是直接发起 Quinn/QUIC `connect()` 预热。
7. 连接成功后缓存 `quinn::Connection`，后续文本消息和文件传输都复用它。
8. 如果失败，再回退到当前信令服务端提供的自定义 TCP relay。

当前和 NAT 流程直接相关的代码：

- `crates/netfile-core/src/stun.rs`
- `crates/netfile-core/src/hole_punch.rs`
- `crates/netfile-core/src/signal_client.rs`
- `crates/netfile-core/src/transfer/service.rs`
- `crates/netfile-signal/src/protocol.rs`
- `crates/netfile-signal/src/server.rs`
- `crates/netfile-gui/src/lib.rs`

当前这套方案的主要问题：

- NAT 类型判断、打洞时序、回退策略都由业务层自己维护。
- `RequestPunch/PunchReady/PunchStart` 是手写状态机。
- relay 是自定义 TCP 中继，不是和连接层深度耦合的标准能力。
- 传输层强依赖 Quinn connection，导致网络层和业务层耦合很深。

## 3. 先给结论

### 3.1 总结论

如果选 `iroh`，它比 `webrtc` 更贴近当前项目。

原因很简单：

- 当前项目本来就是基于 QUIC 连接和流传输。
- `iroh` 对外仍然暴露 QUIC 连接与流。
- 它把 relay-assisted connect、hole punching、地址更新、路径切换收敛到 `Endpoint` 内部。

因此对 NetFile 来说，合理的方向不是：

- 继续保留当前“手写 STUN + 手写 punch session + 手写 relay fallback”；
- 只把某个 socket 层替换成 `iroh`。

更合理的方向是：

**用 `iroh::Endpoint` 取代当前的 NAT 穿透和连接建立主路径，同时尽量保留现有基于 QUIC stream 的业务协议。**

### 3.2 这条路线的意义

如果走 `iroh` 路线：

- 连接建立逻辑会明显简化。
- 文件协议、文本协议、断点续传语义大概率可以保留。
- 改造深度明显小于 WebRTC/DataChannel 路线。

这也是它比 `webrtc` 更适合当前项目的根本原因。

## 4. 官方 crate 现状

根据 docs.rs，当前 `iroh` 的最新版本是 **`0.96.1`**，`iroh-relay` 当前也是 **`0.96.1`**。

官方文档对 `iroh` 的几个关键信息：

- `Endpoint` 是主入口，负责建立和接收连接。
- 官方建议每个应用只创建一个 `Endpoint` 实例。
- `Builder::secret_key` 可以设置稳定身份；如果不设置，会生成随机 `SecretKey`，从而得到新的 `EndpointId`。
- `Builder::alpns` 用于配置可接受的 ALPN；如果不设，虽然仍能发起连接，但要接受入站连接至少要有一个 ALPN。
- `Builder::relay_mode` 控制 relay 行为；relay 不只是“兜底转发”，也参与初始建连和 hole punching。
- `Endpoint::addr()` 返回当前 `EndpointAddr`。
- `Endpoint::watch_addr()` 可持续观察地址变化。
- `Endpoint::online()` 只是一个等待“已连上 relay 且具备外网可拨号条件”的便利方法，它**没有超时**。
- `Endpoint::connect()` 接受 `EndpointAddr` 或 `EndpointId`，如果 `EndpointAddr` 里带 direct addrs，会优先尝试直连。
- `Connection::open_bi()` / `accept_bi()` / `open_uni()` / `accept_uni()` 与当前 Quinn 使用方式接近。
- `Connection::paths()` 可以观察连接上有哪些网络路径，例如 relay 路径与 hole-punched 直连路径。

对本项目非常关键的一点是：

**`iroh` 底层已经把“通过 relay 协调 + 打洞 + 迁移到直连路径”这类复杂逻辑内建了。**

## 5. 改造后的目标架构

### 5.1 建议的职责边界

推荐将职责改成这样：

1. `DiscoveryService`
   - 继续负责局域网设备发现。
2. `Signal Server`
   - 继续负责好友、邀请码、在线状态、离线消息。
   - 新增对 `EndpointAddr` 的同步和转发。
3. `Iroh Endpoint`
   - 负责 NAT 穿透、relay-assisted connect、直连路径建立、路径迁移。
4. `TransferService`
   - 继续负责文件协议、文本协议、断点续传、压缩、进度与历史。
   - 不再自己决定何时 STUN、何时打洞、何时请求 relay。

### 5.2 一句话描述改造后流程

当前流程：

```text
局域网失败 -> signal 请求 punch -> 服务端协调 -> 本地 QUIC connect 预热 -> 失败再走自定义 relay
```

改造后流程：

```text
局域网失败 -> 从 signal 获取对端 EndpointAddr -> Endpoint::connect() -> iroh 底层完成 relay-assisted connect + hole punching -> 成功后继续复用 QUIC streams
```

最重要的变化是：

**业务层不再显式维护 NAT 打洞状态机，只负责“拿到对端地址并发起连接”。**

## 6. 核心改造点

### 6.1 新增独立的 iroh 连接层

建议在 `netfile-core` 里新建一个模块，例如：

```text
crates/netfile-core/src/
├── iroh_net/
│   ├── mod.rs
│   ├── manager.rs       # IrohManager：单例 Endpoint 管理
│   ├── address.rs       # EndpointAddr 编解码
│   ├── transport.rs     # iroh Connection/Stream 封装
│   ├── cache.rs         # 连接缓存
│   └── observe.rs       # watch_addr / paths / metrics
```

`IrohManager` 负责：

- 持久化 `SecretKey`
- 创建单例 `Endpoint`
- 配置 ALPN
- 配置 `RelayMode`
- 管理连接缓存
- 对外提供 `connect(peer_addr)` 和 `accept_loop()`
- 监听 `watch_addr()`，将地址变化同步给 signal server

### 6.2 持久化 iroh 身份

这是必须做的。

官方文档说明：

- 如果不设置 `Builder::secret_key`，`Endpoint` 会生成随机 `SecretKey`
- 这会导致每次重启获得新的 `EndpointId`

所以要新增一个稳定身份文件，例如：

```text
~/.netfile/data/iroh/secret_key
```

并且建议：

- **保留当前 `device_id` 作为业务身份**
- 额外维护一个 `iroh_endpoint_id`

不建议第一阶段直接把 `EndpointId` 替换成业务身份，因为这会影响：

- 好友关系
- 授权
- 消息历史
- 配置与 UI 展示

### 6.3 `TransferService` 从 Quinn endpoint 中解耦

当前 `TransferService` 自己负责：

- 创建 UDP socket
- 创建 Quinn endpoint
- STUN
- NAT 类型判断
- `get_or_connect()`
- `punch_hole()`
- connection cache

改造后建议：

- `TransferService` 不再直接持有 Quinn endpoint
- `TransferService` 只依赖一个统一的传输抽象

例如：

```text
trait PeerTransport {
    async fn open_bi(&self) -> anyhow::Result<(SendSide, RecvSide)>;
    async fn accept_bi(&self) -> anyhow::Result<(SendSide, RecvSide)>;
    async fn close(&self) -> anyhow::Result<()>;
}
```

然后由：

- `QuicTransport`
- `IrohTransport`

分别实现这个接口。

这样迁移时：

- 业务协议层基本不动
- 网络承载层替换掉

### 6.4 文件与文本协议大概率可以保留

这是 `iroh` 路线最大的优势。

因为 `iroh::endpoint::Connection` 仍然提供：

- `open_bi()`
- `accept_bi()`
- `open_uni()`
- `accept_uni()`

当前项目依赖的很多语义可以继续保留：

- `TransferRequest`
- `TransferResponse`
- `TransferComplete`
- `TransferError`
- 文本消息
- chunk 发送
- 压缩
- 断点续传
- 进度统计

官方文档还特别指出：

- stream 是轻量的
- 只有发起端真正写入数据后，对端才会感知并 accept 到这个 stream

而当前项目在打开 stream 后会立刻发送头部消息，这和 `iroh` 的使用方式是兼容的。

### 6.5 用 `watch_addr()` 替换当前 STUN watcher

当前 GUI 里有一个定时 STUN watcher：

- 周期性刷新公网地址
- 更新 NAT 类型
- 调 `SignalClient::update_transfer_addr`

改用 `iroh` 后，建议改成：

1. `Endpoint` 创建完成后，必要时有限时地等待 `online()`
2. 使用 `watch_addr()` 持续观察 `EndpointAddr`
3. 地址变化时，把新的地址信息同步给 signal server

这里要注意官方文档里的两个点：

- `online()` **没有超时**
- `online()` 依赖 relay 可达；对于纯 LAN 场景，可能一直等不到

因此：

- 不要把 `online()` 写成应用启动的硬阻塞前置条件
- 需要超时时必须外层自己包 `timeout`

### 6.6 Signal 协议从“打洞协调”改成“地址交换”

当前这些消息可以逐步退出主路径：

- `request_punch`
- `punch_ready`
- `punch_coordinate`
- `punch_request`
- `punch_start`
- `request_relay`
- `relay_session`
- `incoming_relay`
- `relay_unavailable`

改造后，signal server 的 NAT 相关职责应缩减为：

1. 存储在线设备当前的 `EndpointAddr`
2. 把 `EndpointAddr` 同步给好友
3. 地址变化时推送更新

可以有两种编码方式：

#### 方式 A：拆字段传输

```json
{
  "type": "register",
  "device_id": "...",
  "instance_name": "...",
  "iroh_endpoint_id": "...",
  "relay_urls": ["https://relay.example.com"],
  "direct_addrs": ["1.2.3.4:45678", "10.0.0.2:45678"]
}
```

#### 方式 B：直接传序列化后的 `EndpointAddr`

这更贴近内部类型，但调试性较差。

工程上建议第一阶段先用方式 A。

### 6.7 `stun.rs` 和 `hole_punch.rs` 的去向

改造完成后：

- `stun.rs`
  - 不再属于主路径
  - 第一阶段可降级为诊断模块
  - 后续可删除
- `hole_punch.rs`
  - 第一阶段标记为实验代码
  - 后续可删除

### 6.8 `SignalClient` 需要收口

改造后 `SignalClient` 应保留：

- 好友系统
- 邀请码
- 离线消息
- 在线状态
- 地址同步

应逐步删除主路径能力：

- `request_punch()`
- `send_punch_ready()`
- `request_relay()`

新的主路径变成：

- 上报本端 `EndpointAddr`
- 获取对端 `EndpointAddr`
- 地址变化推送

### 6.9 `netfile-signal` 的服务端状态可大幅简化

当前服务端有：

- `relay_sessions`
- `punch_sessions`

改成 `iroh` 后：

- `punch_sessions` 不应继续存在
- 当前自定义 `relay_sessions` 也不应再是主路径

服务端与 NAT 流程相关的核心状态只需要剩下：

- 在线设备 -> 当前 `EndpointAddr`
- 地址变更推送
- 好友可见性控制

## 7. relay 策略

### 7.1 当前自定义 TCP relay 的问题

现在的 relay 是：

- 两端各自连上服务端一个 TCP socket
- 服务端 `copy_bidirectional()`

它能兜底，但它不是 `iroh` 连接层的一部分。

### 7.2 改造后的建议

如果全链路切到 `iroh`，推荐：

- PoC 阶段可以先用默认 relay
- 生产阶段不要长期依赖公共默认 relay
- 正式部署时自建 `iroh-relay`
- 在 `Builder::relay_mode` 里切到自定义 relay 配置

官方 `Builder::relay_mode` 文档明确说明：

- relay 服务器用于协助建立连接
- 它们和 hole punching 是一体的

同时 `iroh-relay` crate 官方说明也明确：

- relay 会在直连暂时不可行时帮助转发加密流量
- 一旦建立了直接路径，relay 就退居次要位置

### 7.3 当前自定义 relay 的建议去向

- 第一阶段保留为 legacy fallback
- 第二阶段降级为可选兼容路径
- 稳定后彻底删除

## 8. 配置项建议

建议新增独立配置段：

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
enable_dns_lookup = false
enable_pkarr = false
```

说明：

- `enabled`
  - 是否启用 iroh 主路径
- `alpn`
  - 应用协议名
- `secret_key_path`
  - 稳定身份文件
- `relay.mode`
  - PoC 可以 `default`
  - 生产建议 `custom`
- `use_signal_addr_exchange`
  - 第一阶段建议继续依赖 signal server 同步地址

## 9. 可选的进一步简化

### 9.1 `MdnsDiscovery`

官方 `iroh::discovery::mdns` 提供 `MdnsDiscovery`，可发现局域网内 endpoint。

但当前项目的 UDP discovery 除了地址外，还承载：

- 设备名
- 实例名
- 版本
- UI 所需展示信息

所以不建议第一阶段直接替换。

更合理的做法是：

- 第一阶段保留现有 `DiscoveryService`
- 第二阶段把 `MdnsDiscovery` 作为地址发现的补充能力

### 9.2 Address Lookup / DNS / pkarr

官方 `Endpoint::connect()` 文档写得很清楚：

- 如果 `EndpointAddr` 里没有足够地址信息，仍然可以依赖 `Builder::address_lookup` 配置的 Address Lookup

这意味着长期看可以考虑：

- DNS lookup
- pkarr
- 甚至只给 `EndpointId`

但对当前项目，一阶段没必要引入额外基础设施。

更实际的方案是：

- 继续用 signal server 同步 `EndpointAddr`
- 后续如果要做弱中心化，再考虑 Address Lookup

## 10. 推荐迁移步骤

### 阶段 0：PoC

- 引入 `iroh = "0.96.1"`
- 写一个最小 endpoint demo
- 验证：
  - `Endpoint::connect`
  - `Endpoint::accept`
  - `Connection::open_bi`
  - `Connection::accept_bi`
- 在双 NAT 环境里验证能否打通

交付物：

- 两端能通过 iroh 建连并交换简单消息

### 阶段 1：基础设施接入

- 新增 `IrohManager`
- 持久化 `SecretKey`
- 配置 ALPN
- 配置 `relay_mode`
- 接入或自建 `iroh-relay`

交付物：

- 单应用单 `Endpoint`
- `watch_addr()` 可稳定观察地址变化

### 阶段 2：地址同步改造

- 扩展 signal 协议，增加 `EndpointAddr` 相关字段
- 上线时注册本端地址
- 地址变化时增量更新
- 好友在线通知里带上对端地址

交付物：

- GUI 和业务层能拿到对端 `EndpointAddr`

### 阶段 3：连接层替换

- `TransferService` 依赖 `IrohTransport`
- 用 `iroh::endpoint::Connection` 替代当前 Quinn connection cache
- 删除 `punch_hole()` 主路径
- 发送文件和发送文本改成直接 `connect(endpoint_addr)`

交付物：

- 文本和文件主路径都能跑在 iroh 上

### 阶段 4：服务端收口

- 删除 punch 相关信令
- 删除 `punch_sessions`
- 自定义 TCP relay 改成 legacy fallback

交付物：

- 业务层不再显式感知 NAT 打洞状态机

### 阶段 5：可选进一步演进

- 评估是否引入 `MdnsDiscovery`
- 评估是否引入 Address Lookup / DNS / pkarr
- 决定是否彻底删除 legacy relay

## 11. 主要风险

### 11.1 公共 relay 依赖风险

PoC 可以使用默认 relay。

正式生产不建议长期依赖公共默认 relay。

建议：

- 生产阶段自建 `iroh-relay`
- 使用自定义 `relay_mode`

### 11.2 身份映射风险

`EndpointId` 是网络层身份，不自动等于业务身份。

所以仍要保留：

- 当前 `device_id`
- 好友关系
- 授权逻辑

### 11.3 `online()` 使用风险

官方文档明确：

- `online()` 没有超时
- 如果没有 WAN/relay 可达，可能一直等待

所以不要把它写成启动阶段硬阻塞。

### 11.4 版本演进风险

`iroh` 近几个版本 API 仍然在持续演进。

建议：

- 锁定版本
- 用本地适配层包住
- 不要让业务层直接散落大量 `iroh` API 调用

### 11.5 监控与可观测性风险

当前项目需要知道：

- 当前是否直连
- 当前是否走 relay
- 地址是否变化
- RTT 和路径切换情况

改造后建议重点观察：

- `Endpoint::watch_addr()`
- `Connection::paths()`
- `remote_info()`

并在 UI 中展示：

- 当前对端是否有 direct path
- 当前是否仍在 relay 路径上

## 12. 验收标准

- 不再依赖 `stun.rs` 和手写 `Punch*` 状态机建立跨 NAT 连接
- `TransferService` 主路径不再显式区分 punch / relay
- 文本消息和文件传输都能通过 `iroh::endpoint::Connection` 完成
- 文件协议、断点续传、压缩、进度统计仍可工作
- 无法直连时，可自动走 relay-assisted 路径
- 地址变化可通过 `watch_addr()` 同步到 signal server
- signal server 仍能维护好友、在线状态、离线消息

## 13. 一句话总结

如果使用 `iroh` 改造整个项目的 NAT 打洞流程，最合理的做法不是继续维护一套新的业务层 punch 状态机，而是把当前 **手写 STUN + 手写打洞协调 + 手写 relay fallback** 整体收口到 `iroh::Endpoint`，同时尽量保留现有基于 QUIC stream 的业务协议。

对 NetFile 来说，这条路线的价值在于：

- 连接层复杂度显著下降
- 文件协议改造面相对较小
- 整体落地成本低于 WebRTC/DataChannel 路线

## 14. 参考资料

- iroh crate docs: [https://docs.rs/iroh/latest/iroh/](https://docs.rs/iroh/latest/iroh/)
- `Endpoint` docs: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Endpoint.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Endpoint.html)
- `Builder` docs: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Builder.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Builder.html)
- `Connection` docs: [https://docs.rs/iroh/latest/iroh/endpoint/struct.Connection.html](https://docs.rs/iroh/latest/iroh/endpoint/struct.Connection.html)
- `EndpointAddr` docs: [https://docs.rs/iroh/latest/iroh/struct.EndpointAddr.html](https://docs.rs/iroh/latest/iroh/struct.EndpointAddr.html)
- `MdnsDiscovery` docs: [https://docs.rs/iroh/latest/iroh/discovery/mdns/](https://docs.rs/iroh/latest/iroh/discovery/mdns/)
- iroh-relay crate docs: [https://docs.rs/iroh-relay/latest/iroh_relay/](https://docs.rs/iroh-relay/latest/iroh_relay/)
- Iroh 0.96.0 blog: [https://www.iroh.computer/blog/iroh-0-96-0-the-quic-multipaths-to-1-0](https://www.iroh.computer/blog/iroh-0-96-0-the-quic-multipaths-to-1-0)
