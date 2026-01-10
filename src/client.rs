use crate::config::Config;
use crate::protocol::Packet;
use anyhow::{Result, Context};
use log::{info, error, warn};
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::Duration;



use std::collections::HashMap;
use tokio::sync::Mutex;

struct PeerState {
    real_addr: SocketAddr,
    is_p2p: bool,
    last_punch: std::time::Instant,
    punch_attempts: u32, // Count punch attempts for aggressive mode
}

pub async fn run(config: Config) -> Result<()> {
    // Load or create client ID
    let cache = crate::cache::ClientCache::load_or_create();
    let client_id = cache.client_id;
    info!("Using Client ID: {}", client_id);

    // Resolve server address
    let server_addr: SocketAddr = config.server_addr.parse().context("服务器地址无效")?;
    
    // Bind to any local port
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    info!("客户端绑定端口: {}", socket.local_addr()?);

    // Peer Map: VIP -> Real Addr
    let p2p_peers: Arc<Mutex<HashMap<Ipv4Addr, PeerState>>> = Arc::new(Mutex::new(HashMap::new()));
    
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
        #[cfg(target_os = "macos")]
        {
            if !name.starts_with("utun") {
               warn!("在 macOS 上，TUN 设备名称必须以 'utun' 开头且后跟数字 (如 utun5)。由于配置名称 '{}' 不符合规范，将自动使用系统分配的名称。", name);
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
    
    let (mut tun_reader, mut tun_writer) = tokio::io::split(tun_dev);
    // let socket = Arc::new(socket); // Already Arc above

    // 3. Keepalive & P2P Punch/Ping Task
    let socket_ka = socket.clone();
    let p2p_peers_ka = p2p_peers.clone();
    let my_ip_str = my_ip.to_string();
    
    tokio::spawn(async move {
        loop {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
            
            // Keepalive to Server (Use Ping for RTT)
            let ping_packet = Packet::Ping(now);
            if let Ok(bytes) = bincode::serialize(&ping_packet) {
                let _ = socket_ka.send_to(&bytes, server_addr).await;
            }

            // P2P Punching / Ping
            let mut peers = p2p_peers_ka.lock().await;
            let instant_now = std::time::Instant::now();
            let punch_packet = Packet::Punch { vip: my_ip_str.clone() };
            let punch_bytes = bincode::serialize(&punch_packet).unwrap_or_default();
            
            let ping_bytes = bincode::serialize(&ping_packet).unwrap_or_default();

            for (_vip, state) in peers.iter_mut() {
                // 如果还未 P2P 直连，发送 Punch (aggressive mode logic)
                if !state.is_p2p {
                    // Aggressive punching: every 500ms
                    // For Port Restricted Cone NAT / Symmetric NAT, we need frequent packets
                    // to ensure one gets through when the other side's hole opens.
                    if instant_now.duration_since(state.last_punch).as_millis() >= 500 {
                        // 1. Send to primary address (most likely correct for Full Cone / Address Restricted)
                        let _ = socket_ka.send_to(&punch_bytes, state.real_addr).await;
                        
                        // 2. Symmetric NAT Port Prediction (Birthday Attack Light)
                        // If peer is Symmetric, they might be using a different port for us.
                        // We try ports near the one observed by the server.
                        if let SocketAddr::V4(v4) = state.real_addr {
                            let port = v4.port();
                            // Try ports +/- 1, 2, 3... limited range to avoid too much spam
                            // But ONLY if we haven't successfully connected yet
                            for offset in [-1, 1, -2, 2] {
                                let new_port = (port as i32 + offset) as u16;
                                if new_port > 0 {
                                     let try_addr = SocketAddr::V4(std::net::SocketAddrV4::new(*v4.ip(), new_port));
                                     let _ = socket_ka.send_to(&punch_bytes, try_addr).await;
                                }
                            }
                        }

                        state.last_punch = instant_now;
                        state.punch_attempts += 1;
                    }
                } else {
                    // 如果已经 P2P 直连，发送 Ping (维持心跳 + 测延迟, 2s)
                     if instant_now.duration_since(state.last_punch).as_secs() >= 2 {
                        let _ = socket_ka.send_to(&ping_bytes, state.real_addr).await;
                        state.last_punch = instant_now;
                        // debug!("Sending Ping to P2P peer {}", vip);
                    }
                }
            }

            drop(peers);
            // Main loop interval: check often (e.g. 100ms) to support aggressive 500ms punch
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    // 4. Data Transfer Loops
    
    // Task: TUN -> Socket
    let socket_send = socket.clone();
    let p2p_peers_send = p2p_peers.clone();
    
    let tun_read_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tun_reader.read(&mut buf).await {
                Ok(len) => {
                    if len == 0 { break; }
                    let data = &buf[..len];
                    // Check dest IP
                    let dst_ip = crate::protocol::get_dest_ip(data);
                    
                    let mut target_addr = server_addr;
                    
                    if let Some(ip) = dst_ip {
                        let peers = p2p_peers_send.lock().await;
                        if let Some(state) = peers.get(&ip) {
                            if state.is_p2p {
                                target_addr = state.real_addr;
                            }
                        }
                    }

                    // Encapsulate
                    let packet = Packet::Data(data.to_vec());
                    if let Ok(bytes) = bincode::serialize(&packet) {
                        if let Err(e) = socket_send.send_to(&bytes, target_addr).await {
                            error!("发送到 {} 失败: {}", target_addr, e);
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
    let p2p_peers_recv = p2p_peers.clone();
    
    let tun_write_task = tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    let packet_data = &buf[..len];
                    
                    match bincode::deserialize::<Packet>(packet_data) {
                        Ok(Packet::Data(data)) => {
                            // 收到数据包
                            // 如果是从 Server 来的，说明是中继
                            // 如果是从其他 IP 来的，说明是 P2P 直连
                            
                            // 更新 P2P 状态? 如果收到直连 Data 包，说明 P2P 是通的
                            if src != server_addr {
                                // 这是一个 P2P 数据包
                                // 反向查找 VIP? 比较麻烦，暂时只需确认收到数据即可写入 TUN
                            }

                            if let Err(e) = tun_writer.write_all(&data).await {
                                error!("写入 TUN 错误: {}", e);
                            }
                        }
                        Ok(Packet::PeerInfo { peer_vip, peer_addr }) => {
                            if let (Ok(vip), Ok(raddr)) = (peer_vip.parse::<Ipv4Addr>(), peer_addr.parse::<SocketAddr>()) {
                                info!("收到 Peer 信息: VIP={} Real={}", vip, raddr);
                                let mut peers = p2p_peers_recv.lock().await;
                                peers.entry(vip).or_insert(PeerState {
                                    real_addr: raddr,
                                    is_p2p: false, // Default false, wait for punch ack
                                    last_punch: std::time::Instant::now(),
                                    punch_attempts: 0,
                                });
                                // 如果之前已经有 entry，更新地址
                                if let Some(state) = peers.get_mut(&vip) {
                                    state.real_addr = raddr;
                                    // Reset p2p state? Maybe network changed
                                    state.is_p2p = false;
                                    state.punch_attempts = 0; // Reset attempts to trigger aggressive mode again
                                }
                            }
                        }
                        Ok(Packet::Punch { vip }) => {
                            // 收到打洞包
                            if let Ok(vip_addr) = vip.parse::<Ipv4Addr>() {
                                info!("收到来自 {} ({}) 的打洞包! P2P 连接建立。", vip_addr, src);
                                let mut peers = p2p_peers_recv.lock().await;
                                let state = peers.entry(vip_addr).or_insert(PeerState {
                                    real_addr: src, // Trust source addr of punch
                                    is_p2p: true,
                                    last_punch: std::time::Instant::now(),
                                    punch_attempts: 0,
                                });
                                state.is_p2p = true;
                                state.real_addr = src; // Update to actual working address
                                
                                // Reset punch attempts to normal ping mode
                                state.punch_attempts = 0;
                            }
                        }
                        Ok(Packet::Keepalive) => {} 
                        Ok(Packet::Ping(ts)) => {
                            // Reply Pong
                            let pong = Packet::Pong(ts);
                            if let Ok(bytes) = bincode::serialize(&pong) {
                                let _ = socket_recv.send_to(&bytes, src).await;
                            }
                        }
                        Ok(Packet::Pong(ts)) => {
                            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                            let rtt = now.saturating_sub(ts);
                            if src == server_addr {
                                info!("【中转】延迟: {} ms", rtt);
                            } else {
                                info!("【P2P】延迟: {} ms (来自 {})", rtt, src);
                            }
                        }
                        _ => {}
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
