# Linux 服务端部署指南

本指南介绍如何在 Linux 服务器上长期运行 `dend-p2p` 作为服务端，并将日志输出到同目录下的文件。

## 1. 准备文件

假设我们将程序部署在 `/opt/dend-p2p` 目录。

```bash
sudo mkdir -p /opt/dend-p2p
# 将编译好的 Linux 可执行文件上传到这里
sudo cp dend-p2p /opt/dend-p2p/
sudo chmod +x /opt/dend-p2p/dend-p2p

# 创建配置文件
sudo vim /opt/dend-p2p/config.toml
```

**config.toml 内容示例:**
```toml
mode = "server"
server_addr = "0.0.0.0:9696"
token = "your-secure-token-here"
tun_name = "dend0"
cidr = "10.10.0.0/24"
```

## 2. 设置 Systemd 服务

创建一个 systemd 服务文件来实现开机自启和后台运行。

文件路径: `/etc/systemd/system/dend-p2p.service`

```ini
[Unit]
Description=Dend P2P Service
After=network.target

[Service]
Type=simple
User=root
WorkingDirectory=/opt/dend-p2p
# 将标准输出和标准错误追加重定向到同目录下的 server.log
ExecStart=/bin/sh -c 'exec /opt/dend-p2p/dend-p2p --config config.toml >> /opt/dend-p2p/server.log 2>&1'
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

**注意**: 因为需要创建 TUN 设备，`User` 必须是 `root`，或者给二进制文件设置 cap_net_admin 能力。这里为了方便使用 root。

## 3. 启动和管理服务

```bash
# 重载配置
sudo systemctl daemon-reload

# 启动服务
sudo systemctl start dend-p2p

# 设置开机自启
sudo systemctl enable dend-p2p

# 查看状态
sudo systemctl status dend-p2p
```

## 4. 查看日志

日志会自动追加到 `/opt/dend-p2p/server.log` 文件中。

```bash
tail -f /opt/dend-p2p/server.log
```

## 5. 维护与更新

- **更新程序**: 停止服务 -> 替换二进制文件 -> 启动服务。
- **日志轮转**: 如果日志文件过大，可以配置 `logrotate` 或手动清空 (`echo "" > server.log`)。
