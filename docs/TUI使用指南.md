# NetFile TUI 使用指南

## 启动 TUI 界面

### 基本启动

```bash
# 使用默认配置启动
netfile-tui

# 或者使用完整路径
D:/cargo_target/release/netfile-tui.exe
```

### 指定实例名称

为了让两个实例能够互相发现，需要给每个实例指定不同的名称：

```bash
# 终端 1 - 启动第一个实例
netfile-tui --name Server1

# 终端 2 - 启动第二个实例
netfile-tui --name Server2
```

**重要说明**：
- 使用 `--name` 参数会自动生成新的 `instance_id`，确保两个实例不会互相忽略
- 每次使用不同的 `--name` 启动时，都会生成新的唯一标识符
- 如果之前已经运行过 TUI，建议先删除旧的配置文件：`rm ~/.netfile/config.toml`

### 使用自定义配置文件

```bash
# 使用指定的配置文件
netfile-tui --config /path/to/config.toml

# 或者
netfile-tui --config ~/.netfile/server1.toml --name Server1
```

## 界面操作

### 标签页切换

- **Tab** 键：切换到下一个标签页
- **Shift+Tab** 键：切换到上一个标签页
- **1** 键：直接切换到 Devices（设备列表）
- **2** 键：直接切换到 Transfers（传输队列）
- **3** 键：直接切换到 Settings（设置）

### 退出程序

- **q** 键：退出 TUI 界面

## 界面说明

### 1. Devices（设备列表）

显示局域网内所有在线的 NetFile 设备：

```
┌Online Devices (2)────────────────────────────────┐
│ PC-202507251818 - Server1 (192.168.1.10:37050)  │
│ PC-202507251818 - Server2 (192.168.1.10:37051)  │
└──────────────────────────────────────────────────┘
```

- 设备名：主机名
- 实例名：通过 `--name` 参数指定的名称
- IP:端口：设备的传输地址

**自动更新**：设备列表每 1 秒自动刷新

### 2. Transfers（传输队列）

显示当前正在进行的文件传输：

```
┌test.txt (45.2%)──────────────────────────────────┐
│ ████████████░░░░░░░░░░░░░░░░  12.50 MB/s        │
└──────────────────────────────────────────────────┘
```

- 文件名和进度百分比
- 进度条显示传输进度
- 传输速度（MB/s）

**自动更新**：传输进度每 500ms 自动刷新

### 3. Settings（设置）

显示当前实例的配置信息：

```
┌Settings──────────────────────────────────────────┐
│ Instance: Server1                                │
│ Device: PC-202507251818                          │
│ Discovery Port: 37020                            │
│ Transfer Port: 37050                             │
│ Chunk Size: 1048576 bytes                        │
│ Max Concurrent: 3                                │
│ Compression: false                               │
│ Require Auth: true                               │
│ TLS Enabled: false                               │
└──────────────────────────────────────────────────┘
```

## 测试两个实例互相发现

### 步骤 1：打开两个终端窗口

**终端 1**：
```bash
D:/cargo_target/release/netfile-tui.exe --name Server1
```

**终端 2**：
```bash
D:/cargo_target/release/netfile-tui.exe --name Server2
```

### 步骤 2：等待设备发现

启动后等待 5-10 秒，设备发现服务会自动广播并接收其他设备的信息。

### 步骤 3：查看设备列表

在任一终端按 **1** 键切换到 Devices 标签页，应该能看到两个设备：
- Server1
- Server2

## 常见问题

### Q: 为什么看不到其他设备？

**A**: 可能的原因：

1. **实例名称相同**：确保每个实例使用不同的 `--name` 参数
2. **旧配置文件干扰**：删除旧配置文件 `rm ~/.netfile/config.toml` 后重新启动
3. **等待时间不够**：设备发现需要 5-10 秒，请耐心等待
4. **防火墙阻止**：检查防火墙是否允许 UDP 端口 37020-37040
5. **网络隔离**：确保两个实例在同一局域网内

**推荐步骤**：
```bash
# 1. 删除旧配置
rm ~/.netfile/config.toml

# 2. 启动第一个实例
netfile-tui --name Server1

# 3. 在另一个终端启动第二个实例
netfile-tui --name Server2

# 4. 等待 5-10 秒后按 1 键查看设备列表
```

### Q: 如何发送文件？

**A**: TUI 界面目前只支持查看设备和传输状态。要发送文件，请使用 CLI 工具：

```bash
# 在另一个终端使用 CLI 发送文件
netfile send 127.0.0.1:37050 /path/to/file.txt
```

然后在 TUI 的 Transfers 标签页可以看到传输进度。

### Q: 如何修改配置？

**A**: 配置文件位于 `~/.netfile/config.toml`，可以手动编辑：

```toml
[instance]
instance_name = "My Server"

[transfer]
enable_compression = true

[security]
require_auth = false
```

修改后重启 TUI 生效。

## 配置文件位置

- **Windows**: `C:\Users\<用户名>\.netfile\config.toml`
- **Linux/macOS**: `~/.netfile/config.toml`

## 日志查看

TUI 会输出日志到标准错误流，可以重定向到文件：

```bash
netfile-tui --name Server1 2> server1.log
```

## 与 CLI 工具配合使用

TUI 和 CLI 可以同时使用：

1. **TUI**：用于监控设备和传输状态
2. **CLI**：用于执行文件传输操作

示例：
```bash
# 终端 1：启动 TUI 监控
netfile-tui --name Monitor

# 终端 2：使用 CLI 发送文件
netfile send 192.168.1.10:37050 large_file.zip --recursive
```

在 TUI 的 Transfers 标签页可以实时看到传输进度。
