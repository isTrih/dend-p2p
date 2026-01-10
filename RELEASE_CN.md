# dend-p2p v0.2.4 Release Notes

## 版本简介

dend-p2p v0.2.4 是一个重要的功能更新版本，主要新增了在线设备列表功能、优化了 Linux 客户端的 WebUI 配置，并修复了多个 bug。

## 新功能

### 1. 在线设备列表 (v0.2.4)

服务器端每 10 秒向所有客户端广播当前在线设备列表，客户端可以在 WebUI 中实时查看：

- **显示所有在线设备**：包括设备 ID、虚拟 IP、在线状态
- **实时更新**：设备列表每 10 秒自动刷新
- **点击连接**：点击设备即可快速填充连接码

### 2. Linux 客户端 WebUI 配置优化

Linux 客户端默认禁用 WebUI，以适应服务器/无头环境：

```toml
# 客户端配置文件 (client.toml)
[client]
webui = true  # Linux 默认 false，macOS/Windows 默认 true
```

**为什么 Linux 默认禁用？**
- Linux 常用于服务器环境，无需图形界面
- 减少不必要的依赖和资源占用
- 无头部署更加简洁

**启用方式**：在配置文件中添加 `webui = true`

### 3. WebUI 端口自动探测

WebUI 服务器现在会自动探测可用端口（按顺序尝试）：

```
8080, 8081, 8082, 8083, 8084, 8085, 9000, 9001
```

如果默认端口被占用，会自动使用下一个可用端口，并自动打开浏览器。

## Bug 修复

### 1. 延迟计算修复

**问题**：之前使用 `now.duration_since(Instant::now())` 导致延迟计算错误，显示负数或 0ms。

**修复**：现在正确记录 Ping 发送时的时间戳，在收到 Pong 时计算 RTT：

```rust
// 发送 Ping 时记录时间
state.p2p_ping_time = Some(Instant::now());

// 收到 Pong 时计算延迟
if let Some(ping_time) = state.p2p_ping_time {
    let rtt = now.duration_since(ping_time).as_millis() as i32;
    state.last_latency = rtt;
}
```

### 2. 自身连接过滤

**问题**：客户端可能错误地连接到自身。

**修复**：在收到 Pong 包时过滤自身连接：

```rust
let is_self = if let Some(peer_addr) = state.peer_addr {
    src.port() == peer_addr.port() && src.ip() == peer_addr.ip()
} else {
    false
};

if is_self {
    debug!("收到自身的Pong包，忽略");
    continue;
}
```

### 3. 命令行日志增强

**新增日志输出**：
- 运行时显示版本号
- 显示客户端连接码
- 显示 NAT 类型
- P2P 连接建立/失败时输出详细状态

## 改进

### 1. 延迟 P2P 连接

用户点击 WebUI 的"开始连接"按钮后才会发起 P2P 打洞，而不是连接服务器后立即打洞。

**优点**：
- 避免不必要的打洞流量
- 让用户有更多控制权
- 减少服务器负载

### 2. 连接保活机制

每 30 秒发送一次保活包，防止 NAT 超时：

```rust
// 配置
keepalive_interval: 30,  // 秒
```

### 3. 并发打洞优化

最多 2 个并发打洞尝试，加速连接建立：

```rust
// 配置
max_concurrency: 2,
punch_interval: 200,  // 毫秒
```

## 使用指南

### 服务器部署

```bash
# 启动服务器
sudo ./target/release/dend-p2p --config server.toml
```

服务器配置文件示例 (`server.toml`)：

```toml
[server]
mode = "server"
server_addr = "0.0.0.0:7070"
token = "your-secret-token"
cidr = "10.10.0.0/24"
tun_name = "dend0"
```

### 客户端部署

```bash
# 启动客户端 (Linux 默认无 WebUI)
sudo ./target/release/dend-p2p --config client.toml

# 如果需要 WebUI，在配置文件中添加：
# [client]
# webui = true
```

客户端配置文件示例 (`client.toml`)：

```toml
[client]
mode = "client"
server_addr = "127.0.0.1:7070"
token = "your-secret-token"
tun_name = "dend0"
# webui = true  # Linux 上默认为 false
```

### WebUI 使用

1. 启动客户端后（如果启用了 WebUI），浏览器会自动打开
2. 查看"我的连接码"，分享给对方
3. 在"在线设备"列表中点击目标设备，或手动输入连接码
4. 点击"开始连接"按钮
5. 等待 P2P 连接建立

## 已知问题

1. 服务器端暂不支持推送在线设备列表（v0.2.4 已支持）
2. 对称型 NAT (NAT4) 打洞成功率较低，建议使用中转模式

## 升级注意事项

1. **配置文件兼容**：v0.2.4 兼容旧配置文件，但 Linux 客户端默认不再启动 WebUI
2. **WebUI 端口**：如果使用了自定义端口配置，可能需要调整
3. **日志格式**：日志输出格式略有调整，建议更新日志解析逻辑

## 感谢

感谢所有反馈问题和提出建议的用户！

## 下载

- GitHub Releases: https://github.com/yourusername/dend-p2p/releases

---

**dend-p2p** - 简单易用的 P2P 直连组网工具
