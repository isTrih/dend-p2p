use crate::config::Config;
use crate::protocol::Packet;
use anyhow::{Result, Context};
use log::{info, error, warn};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::Duration;

mod cache;

pub async fn run(config: Config) -> Result<()> {
    // Load or create client ID
    let cache = crate::cache::ClientCache::load_or_create();
    let client_id = cache.client_id;
    info!("Using Client ID: {}", client_id);

    // Resolve server address
    let server_addr: SocketAddr = config.server_addr.parse().context("服务器地址无效")?;
    
    // Bind to any local port
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    info!("客户端绑定端口: {}", socket.local_addr()?);
    
    // 1. Handshake Loop
    let my_ip: Ipv4Addr;
    let mut _network_cidr: String;
    
    loop {
        info!("正在向服务器 {} 发送握手请求...", server_addr);
        let packet = Packet::Handshake { token: config.token.clone(), client_id: client_id.clone() };
        let bytes = bincode::serialize(&packet)?;
        socket.send_to(&bytes, server_addr).await?;
        
        let mut buf = [0u8; 1024];
        let res = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await;
        
        match res {
            Ok(Ok((len, src))) => {
                if src == server_addr {
                    if let Ok(Packet::HandshakeAck { ip, cidr }) = bincode::deserialize::<Packet>(&buf[..len]) {
                        info!("握手成功! 分配 IP: {}", ip);
                        my_ip = ip.parse()?;
                        _network_cidr = cidr;
                        break;
                    }
                }
            }
            Ok(Err(e)) => error!("接收错误: {}", e),
            Err(_) => warn!("握手超时, 正在重试..."),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // 2. Setup TUN
    let mut tun_config = tun::Configuration::default();
    tun_config
        .address(my_ip)
        .netmask(Ipv4Addr::new(255, 255, 255, 0)) // Assuming /24 for now based on server
        .up();
        
    #[cfg(not(target_os = "windows"))]
    tun_config.destination(Ipv4Addr::new(10, 10, 0, 1)); // Set gateway/peer? simple ptp usually

    if let Some(name) = &config.tun_name {
        // macOS utun devices must be named "utunX" where X is a number (e.g. utun0, utun1)
        // or simply allow the system to pick one by not setting a name if the user provided one isn't valid?
        // But for cross-platform simplicity, we might just let the system pick if on macOS and the name doesn't start with utun.
        // Or we warn the user.
        
        #[cfg(target_os = "macos")]
        {
            if !name.starts_with("utun") {
               warn!("在 macOS 上，TUN 设备名称必须以 'utun' 开头且后跟数字 (如 utun5)。由于配置名称 '{}' 不符合规范，将自动使用系统分配的名称。", name, name);
               // 不设置 name，让系统自动分配
            } else {
                tun_config.name(name);
            }
        }
        
        #[cfg(not(target_os = "macos"))]
        {
            tun_config.name(name);
        }
    }
    
    #[cfg(target_os = "linux")]
    tun_config.platform(|config| {
        config.packet_information(true);
    });

    let tun_dev = tun::create_as_async(&tun_config).context("创建 TUN 设备失败")?;
    info!("TUN 设备已创建");
    
    // Windows 平台特殊修复：
    // 当 TUN 获取到 IP 且子网掩码只有 IP 本身时，路由表可能将发往公网服务器的流量错误地路由到了 TUN 接口。
    // 这会导致 "UDP 发包 -> 路由到 TUN -> 读取 TUN -> UDP 发包 -> 路由到 TUN" 的死循环，
    // 或者直接报错 "Unreachable host" (os error 10065)，因为系统认为去往服务器的路径不通。
    // 
    // 解决方案：为防止 UDP socket 绑定的流量走 TUN，我们需要将 UDP socket 绑定到具体的物理网卡 IP (但这在 DHCP 下会变且麻烦)。
    // 或者，更简单的：确保 TUN 的掩码和路由不要覆盖去往服务器 123.60.176.158 的路径。
    // 当前我们设置了 TUN IP 和默认路由？看代码：destination(10.10.0.1)
    
    // Windows 下，tun crate 可能会默认添加路由。如果出现 10065，说明去往 Server 的路由坏了。
    // 这是一个常见的 VPN/TUN 路由冲突问题。
    // 暂时可以通过只绑定 socket 到 0.0.0.0 来尝试解决（我们已经这么做了）。
    // 
    // 更有可能是 Windows 防火墙或路由表优先级的锅。
    
    let (mut tun_reader, mut tun_writer) = tokio::io::split(tun_dev);
    let socket = Arc::new(socket);

    // 3. Keepalive Task
    let socket_ka = socket.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let packet = Packet::Keepalive;
            if let Ok(bytes) = bincode::serialize(&packet) {
                let _ = socket_ka.send_to(&bytes, server_addr).await;
            }
        }
    });

    // 4. Data Transfer Loops
    
    // Task: TUN -> Socket
    let socket_send = socket.clone();
    let tun_read_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tun_reader.read(&mut buf).await {
                Ok(len) => {
                    if len == 0 { break; }
                    let data = &buf[..len];
                    // Encapsulate
                    let packet = Packet::Data(data.to_vec());
                    if let Ok(bytes) = bincode::serialize(&packet) {
                        if let Err(e) = socket_send.send_to(&bytes, server_addr).await {
                            error!("发送到服务器失败: {}", e);
                        }
                    }
                }
                Err(e) => {
                    error!("读取 TUN 错误: {}", e);
                    break;
                }
            }
        }
    });

    // Task: Socket -> TUN
    let socket_recv = socket.clone();
    let tun_write_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    if src == server_addr {
                        match bincode::deserialize::<Packet>(&buf[..len]) {
                            Ok(Packet::Data(data)) => {
                                if let Err(e) = tun_writer.write_all(&data).await {
                                    error!("写入 TUN 错误: {}", e);
                                }
                            }
                            Ok(Packet::Keepalive) => {} // Ping back?
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    error!("Socket 接收错误: {}", e);
                    break;
                }
            }
        }
    });

    let _ = tokio::join!(tun_read_task, tun_write_task);
    
    Ok(())
}
