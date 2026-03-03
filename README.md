# NetFile

NetFile 是一个高性能的内网文件传输工具，支持设备发现、文件传输、断点续传、压缩传输、TLS 加密、NAT 穿透等功能。

## 核心特性

- **设备发现**: 基于 UDP 广播的自动设备发现，支持局域网内设备互相发现
- **文件传输**: 基于 TCP 的可靠文件传输，支持分块传输和断点续传
- **文件夹传输**: 支持递归传输文件夹，保留完整目录结构
- **进度跟踪**: 实时显示传输进度、速度和预计剩余时间
- **压缩传输**: 使用 zstd 算法智能压缩，自动判断压缩收益
- **安全机制**: 支持设备授权、密码保护和 TLS 加密
- **NAT 穿透**: 支持 STUN 协议获取公网 IP 和 UDP 打洞
- **跨平台**: 支持 Windows、Linux、macOS

## 架构设计

### 模块结构

```
netfile/
├── crates/
│   ├── netfile-core/          # 核心库
│   │   ├── src/
│   │   │   ├── config.rs      # 配置管理
│   │   │   ├── discovery/     # 设备发现
│   │   │   ├── transfer/      # 文件传输
│   │   │   ├── protocol.rs    # 协议定义
│   │   │   ├── auth.rs        # 身份验证
│   │   │   ├── compression.rs # 压缩模块
│   │   │   ├── tls.rs         # TLS 加密
│   │   │   ├── stun.rs        # STUN 协议
│   │   │   └── hole_punch.rs  # UDP 打洞
│   │   └── tests/             # 集成测试
│   └── netfile-cli/           # CLI 工具
│       └── src/
│           └── main.rs        # 命令行入口
└── docs/                      # 文档目录
```

### 核心组件

#### 1. 设备发现 (Discovery)

使用 UDP 广播实现局域网设备自动发现：
- 定时广播设备信息（默认 5 秒间隔）
- 监听其他设备的广播消息
- 维护在线设备列表
- 支持心跳超时检测

#### 2. 文件传输 (Transfer)

基于 TCP 的可靠文件传输：
- **分块传输**: 将文件分成固定大小的块（默认 1MB）
- **断点续传**: 记录已传输的块，支持中断后继续
- **并发控制**: 支持多个文件并发传输（默认最多 3 个）
- **完整性校验**: 使用 SHA256 校验文件完整性，CRC32 校验块完整性

#### 3. 协议设计

使用 bincode 序列化的二进制协议：

```rust
// 传输请求
TransferRequest {
    file_id: String,
    file_name: String,
    relative_path: Option<String>,
    file_size: u64,
    file_hash: [u8; 32],
    chunk_size: u32,
    device_id: String,
    password_hash: Option<String>,
}

// 块数据
ChunkData {
    file_id: String,
    chunk_index: u32,
    data: Vec<u8>,
    checksum: u32,
    compressed: bool,
}
```

#### 4. 压缩传输

使用 zstd 算法（压缩级别 3）：
- 仅对大于 1024 字节的块进行压缩
- 自动判断压缩收益，无收益则跳过
- 接收端自动检测并解压

#### 5. 安全机制

- **设备授权**: 维护授权设备白名单
- **密码保护**: 使用 SHA256 哈希存储密码
- **TLS 加密**: 支持自签名证书的 TLS 加密传输

#### 6. NAT 穿透

- **STUN 协议**: 获取公网 IP 地址和端口映射
- **UDP 打洞**: 实现 P2P 连接建立

## 配置文件

配置文件位于 `~/.netfile/config.toml`：

```toml
[instance]
instance_id = "uuid"
instance_name = "默认实例"
device_name = "hostname"

[network]
discovery_port = 0        # 0 表示自动分配
transfer_port = 0
broadcast_interval = 5    # 秒

[transfer]
chunk_size = 1048576      # 1MB
max_concurrent = 3
enable_compression = false

[security]
require_auth = true
password = ""
allowed_devices = []
enable_tls = false
```

## CLI 命令

### 启动服务

```bash
# 启动 CLI 模式
netfile

# 指定实例名称
netfile --name Server1

# 指定配置文件
netfile --config /path/to/config.toml
```

### 设备管理

```bash
# 列出在线设备
netfile devices list

# 查看设备详情
netfile devices info <instance_id>
```

### 文件传输

```bash
# 发送文件
netfile send <target_ip:port> <file_path>

# 发送文件夹（递归）
netfile send <target_ip:port> <folder_path> --recursive
```

### 传输管理

```bash
# 列出传输任务
netfile transfers list

# 查看传输详情
netfile transfers info <task_id>
```

### 配置管理

```bash
# 显示配置
netfile config show

# 设置配置项
netfile config set transfer.enable_compression true

# 重置配置
netfile config reset
```

### 授权管理

```bash
# 列出授权设备
netfile auth list

# 添加授权设备
netfile auth allow <device_id>

# 移除授权设备
netfile auth deny <device_id>

# 设置密码
netfile auth set-password <password>
```

### 输出格式

所有命令支持三种输出格式：

```bash
# 表格格式（默认）
netfile devices list -o table

# JSON 格式
netfile devices list -o json

# 简洁格式
netfile devices list -o simple
```

## 数据流程

### 文件发送流程

1. 扫描文件/文件夹，生成文件列表
2. 计算文件哈希（SHA256）
3. 建立 TCP 连接到目标设备
4. 发送 TransferRequest
5. 等待 TransferResponse 确认
6. 分块读取文件并发送 ChunkData
7. 等待每个块的 ChunkAck 确认
8. 完成后发送 TransferComplete

### 文件接收流程

1. 监听 TCP 连接
2. 接收 TransferRequest
3. 创建临时文件（预分配空间）
4. 发送 TransferResponse 确认
5. 接收 ChunkData 并写入临时文件
6. 发送 ChunkAck 确认
7. 所有块接收完成后校验文件哈希
8. 将临时文件移动到目标位置

## 性能特性

- **零拷贝**: 使用流式读写减少内存拷贝
- **并发传输**: 支持多文件并发传输
- **智能压缩**: 自动判断压缩收益
- **断点续传**: 支持中断后继续传输
- **进度跟踪**: 实时显示传输进度和速度

## 测试

项目包含完整的单元测试和集成测试：

```bash
# 运行所有测试
cargo test

# 运行单元测试
cargo test --lib

# 运行集成测试
cargo test --test integration_test
```

测试覆盖：
- 配置模块：默认值、序列化、ID 生成
- 协议模块：消息序列化、反序列化
- 压缩模块：压缩/解压、边界情况
- 认证模块：密码哈希、设备授权
- 传输模块：服务创建、进度跟踪

## 技术栈

- **语言**: Rust 2021 Edition
- **异步运行时**: Tokio
- **序列化**: Serde, Bincode, TOML
- **网络**: Tokio TCP/UDP
- **加密**: SHA256, Rustls, RCGen
- **压缩**: Zstd
- **NAT 穿透**: STUN
- **CLI**: Clap
- **日志**: Tracing

## 开发状态

- ✅ 设备发现
- ✅ 文件传输
- ✅ 断点续传
- ✅ 文件夹传输
- ✅ 进度跟踪
- ✅ 身份验证
- ✅ TLS 加密
- ✅ 压缩传输
- ✅ STUN 协议
- ✅ UDP 打洞
- ✅ CLI 命令行
- ✅ 单元测试
- ✅ 集成测试
- ⏳ GUI 界面

## 许可证

MIT License
