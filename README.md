# dend-p2p

一个简单的 P2P 连接工具 (基于 VPN 模型)，支持自动分配虚拟 IP。

作者: HUA Haohui

## 功能

- 支持服务端 (Server) 和客户端 (Client) 模式。
- 服务端自动管理虚拟 IP 地址分配。
- 跨平台支持 (Linux, macOS, Windows)。
- 建立虚拟网卡 (TUN)，实现 IP 层互通。

## 构建

需要安装 Rust 工具链。

```bash
cargo build --release
```

编译产物位于 `target/release/dend-p2p` (Linux/macOS) 或 `target/release/dend-p2p.exe` (Windows)。

## 配置

在可执行文件同目录下创建 `config.toml`。

**服务端配置 (config.toml):**
```toml
mode = "server"
server_addr = "0.0.0.0:9696"
token = "my-secret-token"
tun_name = "dend0"
cidr = "10.10.0.0/24" # 虚拟IP网段
```

**客户端配置 (config.toml):**
```toml
mode = "client"
server_addr = "服务端公网IP:9696"
token = "my-secret-token"
tun_name = "dend0"
```

## 运行

**注意**: 程序需要创建虚拟网络设备，因此需要管理员权限 (sudo/Admin)。

### Linux / macOS

```bash
sudo ./dend-p2p --config config.toml
```

### Windows

1. 确保已下载 [Wintun 驱动](https://www.wintun.net/) 的 `wintun.dll` 并放置在 `dend-p2p.exe` 同级目录下。
2. 右键以管理员身份运行 PowerShell 或 CMD。
3. 运行程序:
   ```cmd
   dend-p2p.exe --config config.toml
   ```

## 开发说明

- 使用 `tun` crate 操作虚拟网卡。
- 使用 `tokio` 进行异步 UDP 通信。
- 自定义简单协议进行握手和数据封装。

## 常见问题

- **macOS 权限**: 必须使用 `sudo` 运行，否则无法打开 `/dev/tunX`。
- **Windows 依赖**: 缺少 `wintun.dll` 会导致启动失败。
