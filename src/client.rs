use crate::config::Config;
use crate::protocol::Packet;
use anyhow::{Result, Context};
use log::{info, error, warn};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::Duration;

pub async fn run(config: Config) -> Result<()> {
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
        let packet = Packet::Handshake { token: config.token.clone() };
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
        .destination(Ipv4Addr::new(10, 10, 0, 1)) // Set gateway/peer? simple ptp usually
        .up();

    if let Some(name) = &config.tun_name {
        tun_config.name(name);
    }
    
    #[cfg(target_os = "linux")]
    tun_config.platform(|config| {
        config.packet_information(true);
    });

    let tun_dev = tun::create_as_async(&tun_config).context("创建 TUN 设备失败")?;
    info!("TUN 设备已创建");
    
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
