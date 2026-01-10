use crate::config::Config;
use crate::protocol::{Packet, get_dest_ip};
use anyhow::{Result, Context};
use log::{info, error, warn, debug};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::{Duration, Instant};

// ==================== 客户端信息 ====================

#[derive(Debug, Clone)]
struct ClientInfo {
    addr: SocketAddr,
    conn: Arc<UdpSocket>,
    last_seen: Instant,
    client_id: String,
    vip: Option<Ipv4Addr>,
}

// ==================== NAT 打洞会话 ====================

#[derive(Debug, Clone)]
struct NatSession {
    peer_one_id: String,
    peer_one_addr: SocketAddr,
    peer_one_vip: Ipv4Addr,
    peer_two_id: String,
    peer_two_addr: SocketAddr,
    peer_two_vip: Ipv4Addr,
    created_at: Instant,
}

impl NatSession {
    fn new(
        peer_one_id: String, peer_one_addr: SocketAddr, peer_one_vip: Ipv4Addr,
        peer_two_id: String, peer_two_addr: SocketAddr, peer_two_vip: Ipv4Addr,
    ) -> Self {
        Self {
            peer_one_id,
            peer_one_addr,
            peer_one_vip,
            peer_two_id,
            peer_two_addr,
            peer_two_vip,
            created_at: Instant::now(),
        }
    }
}

// ==================== 中转会话 ====================

#[derive(Debug)]
struct RelaySession {
    client_one_id: String,
    client_two_id: String,
    enabled: bool,
}

// ==================== 服务器主结构 ====================

pub async fn run(config: Config) -> Result<()> {
    info!("正在启动服务器: {}", config.server_addr);

    // 绑定 UDP 套接字
    let socket = Arc::new(UdpSocket::bind(&config.server_addr).await?);
    info!("服务器已启动，监听地址: {}", config.server_addr);

    // 解析 CIDR 获取网络信息
    let cidr_str = config.cidr.clone().context("服务器模式需要指定 CIDR")?;
    let (network_ip, _) = parse_cidr(&cidr_str)?;
    let server_ip = next_ip(network_ip, 1);

    // 创建 TUN 设备
    let tun_dev = create_tun_device(&config.tun_name, server_ip).await?;
    info!("服务器 TUN 设备已创建: {}", server_ip);

    let (mut tun_reader, tun_writer) = tokio::io::split(tun_dev);

    // 共享数据结构
    let peer_map: Arc<Mutex<HashMap<SocketAddr, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let client_id_map: Arc<Mutex<HashMap<String, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    let nat_sessions: Arc<Mutex<HashMap<String, NatSession>>> = Arc::new(Mutex::new(HashMap::new()));
    let relay_sessions: Arc<Mutex<HashMap<String, RelaySession>>> = Arc::new(Mutex::new(HashMap::new()));

    // ==================== 阶段 1: 客户端超时清理任务 ====================

    let peer_map_clean = peer_map.clone();
    let client_id_map_clean = client_id_map.clone();
    let nat_sessions_clean = nat_sessions.clone();

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = Instant::now();

            let mut removed_addrs = Vec::new();
            {
                let mut peers = peer_map_clean.lock().await;
                peers.retain(|addr, client| {
                    let active = now.duration_since(client.last_seen).as_secs() < 60;
                    if !active {
                        info!("客户端 {} ({}) 超时，释放 IP", client.client_id, addr);
                        removed_addrs.push(addr.clone());
                    }
                    active
                });
            }

            // 清理客户端 ID 映射
            {
                let mut id_map = client_id_map_clean.lock().await;
                for addr in &removed_addrs {
                    id_map.retain(|_id, client| &client.addr != addr);
                }
            }

            // 清理 NAT 会话
            {
                let mut sessions = nat_sessions_clean.lock().await;
                sessions.retain(|_key, session| {
                    now.duration_since(session.created_at).as_secs() < 300
                });
            }
        }
    });

    // ==================== 阶段 2: UDP 数据包处理 ====================

    let socket_recv = socket.clone();
    let peer_map_recv = peer_map.clone();
    let client_id_map_recv = client_id_map.clone();
    let nat_sessions_recv = nat_sessions.clone();
    let relay_sessions_recv = relay_sessions.clone();
    let config_token = config.token.clone();
    let cidr_str_clone = cidr_str.clone();
    let server_ip_clone = server_ip;

    // IP 分配器
    let find_free_ip = move |peers: &HashMap<SocketAddr, ClientInfo>| -> Option<Ipv4Addr> {
        for i in 2..254 {
            let ip = next_ip(network_ip, i);
            let ip_used = peers.values().any(|c| c.vip == Some(ip));
            if !ip_used {
                return Some(ip);
            }
        }
        None
    };

    // TUN 写入器
    let tun_writer = Arc::new(Mutex::new(tun_writer));

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let packet_data = &buf[..len];

                    if let Ok(packet) = bincode::deserialize::<Packet>(packet_data) {
                        match packet {
                            // ==================== 握手请求 ====================
                            Packet::Handshake { token, client_id } => {
                                if token != config_token {
                                    warn!("令牌验证失败: {}", addr);
                                    continue;
                                }

                                let mut peers = peer_map_recv.lock().await;
                                let mut id_map = client_id_map_recv.lock().await;

                                // 检查是否已有该客户端 ID 的连接
                                let mut assigned_ip = None;
                                if let Some(existing) = id_map.get(&client_id) {
                                    assigned_ip = existing.vip;
                                    // 更新地址
                                    if let Some(p) = peers.get_mut(&existing.addr) {
                                        p.addr = addr;
                                        p.last_seen = Instant::now();
                                    }
                                    info!("客户端 {} 重新握手 ({})，保持 IP {:?}", client_id, addr, assigned_ip);
                                }

                                let new_ip = if let Some(ip) = assigned_ip {
                                    ip
                                } else {
                                    // 分配新 IP
                                    if let Some(ip) = find_free_ip(&peers) {
                                        info!("客户端 {} ({}) 分配 IP {}", client_id, addr, ip);
                                        ip
                                    } else {
                                        warn!("无可用 IP 分配给客户端 {}", addr);
                                        continue;
                                    }
                                };

                                // 保存/更新客户端信息
                                let client_info = ClientInfo {
                                    addr,
                                    conn: socket_recv.clone(),
                                    last_seen: Instant::now(),
                                    client_id: client_id.clone(),
                                    vip: Some(new_ip),
                                };
                                peers.insert(addr, client_info.clone());
                                id_map.insert(client_id.clone(), client_info);

                                // 发送握手确认
                                let resp = Packet::HandshakeAck {
                                    ip: new_ip.to_string(),
                                    cidr: cidr_str_clone.clone(),
                                };
                                if let Ok(resp_bytes) = bincode::serialize(&resp) {
                                    let _ = socket_recv.send_to(&resp_bytes, addr).await;
                                }

                                // 通知其他客户端有新节点加入
                                broadcast_new_peer(
                                    &socket_recv,
                                    &peers,
                                    addr,
                                    &new_ip,
                                    &client_id,
                                ).await;
                            }

                            // ==================== 心跳 ====================
                            Packet::Ping(ts) => {
                                let pong = Packet::Pong(ts);
                                if let Ok(bytes) = bincode::serialize(&pong) {
                                    let _ = socket_recv.send_to(&bytes, addr).await;
                                }

                                // 更新最后活跃时间
                                if let Some(client) = peer_map_recv.lock().await.get_mut(&addr) {
                                    client.last_seen = Instant::now();
                                }
                            }

                            // ==================== 请求连接对等节点 ====================
                            Packet::RequestConnect { target_id, password } => {
                                // 获取源客户端 ID
                                let src_client_id = {
                                    let id_map = client_id_map_recv.lock().await;
                                    id_map.iter()
                                        .find(|(_, c)| c.addr == addr)
                                        .map(|(id, _)| id.clone())
                                        .unwrap_or_default()
                                };

                                if src_client_id.is_empty() {
                                    warn!("无法识别客户端: {}", addr);
                                    continue;
                                }

                                info!("收到连接请求: {} -> {}", src_client_id, target_id);

                                // 验证目标客户端存在
                                let target_client = {
                                    let id_map = client_id_map_recv.lock().await;
                                    id_map.get(&target_id).cloned()
                                };

                                if target_client.is_none() {
                                    warn!("目标客户端 {} 不存在", target_id);
                                    continue;
                                }

                                // 提取 target_client 的值
                                let target = target_client.unwrap();

                                // 创建 NAT 打洞会话
                                let src_client = {
                                    let id_map = client_id_map_recv.lock().await;
                                    id_map.get(&src_client_id).cloned()
                                };

                                if let Some(src) = src_client {
                                    let session = NatSession::new(
                                        src_client_id.clone(),
                                        addr,
                                        src.vip.unwrap(),
                                        target_id.clone(),
                                        target.addr,
                                        target.vip.unwrap(),
                                    );

                                    let mut sessions = nat_sessions_recv.lock().await;
                                    sessions.insert(src_client_id.clone(), session.clone());
                                    sessions.insert(target_id.clone(), session.clone());

                                    // 通知双方更换端口后打洞
                                    notify_change_port(
                                        &socket_recv,
                                        addr,
                                        target.addr,
                                        password,
                                    ).await;
                                }
                            }

                            // ==================== 客户端端口更换完毕 ====================
                            Packet::PortChanged { client_id: cid } => {
                                debug!("收到客户端 {} 端口更换完成通知", cid);

                                let mut sessions = nat_sessions_recv.lock().await;
                                if let Some(session) = sessions.get(&cid) {
                                    // 通知双方开始打洞
                                    send_connect_peer(
                                        &socket_recv,
                                        &session,
                                    ).await;
                                }
                            }

                            // ==================== 启用中转模式 ====================
                            Packet::EnableRelay { src_id, target_id, vip } => {
                                let session_key = get_session_key(&src_id, &target_id);

                                {
                                    let mut relay_map = relay_sessions_recv.lock().await;
                                    relay_map.insert(session_key.clone(), RelaySession {
                                        client_one_id: src_id.clone(),
                                        client_two_id: target_id.clone(),
                                        enabled: true,
                                    });
                                }

                                info!("已启用中转模式: {} <-> {}", src_id, target_id);

                                // 通知双方中转模式已启用
                                notify_relay_enabled(
                                    &socket_recv,
                                    &client_id_map_recv,
                                    &src_id,
                                    &target_id,
                                    &vip,
                                ).await;
                            }

                            // ==================== 中转延迟测试 ====================
                            Packet::RelayLatencyTest { target_id, timestamp } => {
                                let target_client = {
                                    let id_map = client_id_map_recv.lock().await;
                                    id_map.get(&target_id).cloned()
                                };

                                if let Some(target) = target_client {
                                    let packet = Packet::RelayLatencyTest {
                                        target_id,
                                        timestamp,
                                    };
                                    if let Ok(bytes) = bincode::serialize(&packet) {
                                        let _ = socket_recv.send_to(&bytes, target.addr).await;
                                    }
                                }
                            }

                            // ==================== 中转延迟回复 ====================
                            Packet::RelayLatencyReply { target_id, timestamp } => {
                                let target_client = {
                                    let id_map = client_id_map_recv.lock().await;
                                    id_map.get(&target_id).cloned()
                                };

                                if let Some(target) = target_client {
                                    let packet = Packet::RelayLatencyReply {
                                        target_id,
                                        timestamp,
                                    };
                                    if let Ok(bytes) = bincode::serialize(&packet) {
                                        let _ = socket_recv.send_to(&bytes, target.addr).await;
                                    }
                                }
                            }

                            // ==================== 数据包中转 ====================
                            Packet::Data(data) => {
                                if let Some(dst_ip) = get_dest_ip(&data) {
                                    if dst_ip == server_ip {
                                        // 发送到服务器 TUN
                                        let mut writer = tun_writer.lock().await;
                                        let _ = writer.write_all(&data).await;
                                    } else {
                                        // 转发到目标客户端
                                        let peers = peer_map_recv.lock().await;
                                        for client in peers.values() {
                                            if client.vip == Some(dst_ip) {
                                                let packet = Packet::Data(data);
                                                if let Ok(bytes) = bincode::serialize(&packet) {
                                                    let _ = socket_recv.send_to(&bytes, client.addr).await;
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                            }

                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    error!("UDP 接收错误: {}", e);
                }
            }
        }
    });

    // ==================== 阶段 3: TUN -> 客户端数据转发 ====================

    let socket_tun = socket.clone();
    let peer_map_tun = peer_map.clone();

    let mut buf_tun = [0u8; 4096];
    loop {
        match tun_reader.read(&mut buf_tun).await {
            Ok(len) => {
                let data = &buf_tun[..len];
                if let Some(dst) = get_dest_ip(data) {
                    let peers = peer_map_tun.lock().await;
                    for client in peers.values() {
                        if client.vip == Some(dst) {
                            let packet = Packet::Data(data.to_vec());
                            if let Ok(encoded) = bincode::serialize(&packet) {
                                let _ = socket_tun.send_to(&encoded, client.addr).await;
                            }
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                error!("TUN 读取错误: {}", e);
                break;
            }
        }
    }

    Ok(())
}

// ==================== 辅助函数 ====================

async fn broadcast_new_peer(
    socket: &UdpSocket,
    peers: &HashMap<SocketAddr, ClientInfo>,
    new_addr: SocketAddr,
    new_ip: &Ipv4Addr,
    new_id: &str,
) {
    let packet = Packet::PeerInfo {
        peer_vip: new_ip.to_string(),
        peer_addr: new_addr.to_string(),
    };

    if let Ok(bytes) = bincode::serialize(&packet) {
        for (addr, client) in peers.iter() {
            if addr != &new_addr {
                let _ = socket.send_to(&bytes, client.addr).await;
            }
        }

        // 也通知新客户端关于现有的对等节点
        for (addr, client) in peers.iter() {
            if addr != &new_addr {
                let peer_packet = Packet::PeerInfo {
                    peer_vip: client.vip.unwrap().to_string(),
                    peer_addr: addr.to_string(),
                };
                if let Ok(peer_bytes) = bincode::serialize(&peer_packet) {
                    let _ = socket.send_to(&peer_bytes, new_addr).await;
                }
            }
        }
    }
}

async fn notify_change_port(
    socket: &UdpSocket,
    src_addr: SocketAddr,
    target_addr: SocketAddr,
    password: String,
) {
    // 向双方发送更换端口命令
    let packet = Packet::ChangePort { password: password.clone() };

    if let Ok(bytes) = bincode::serialize(&packet) {
        let _ = socket.send_to(&bytes, src_addr).await;
        let _ = socket.send_to(&bytes, target_addr).await;
    }

    info!("已通知双方更换端口");
}

async fn send_connect_peer(socket: &UdpSocket, session: &NatSession) {
    // 通知双方开始打洞

    // 转换为 Ipv4Addr
    let peer_two_addr_v4 = match session.peer_two_addr.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => {
            // IPv6 地址无法用于此场景
            return;
        }
    };

    let peer_one_addr_v4 = match session.peer_one_addr.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => {
            return;
        }
    };

    // 发送给第一方：关于第二方的信息
    let packet1 = Packet::ConnectPeer {
        peer_vip: session.peer_two_vip,
        peer_addr: peer_two_addr_v4,
        port: session.peer_two_addr.port(),
        peer_id: session.peer_two_id.clone(),
    };

    // 发送给第二方：关于第一方的信息
    let packet2 = Packet::ConnectPeer {
        peer_vip: session.peer_one_vip,
        peer_addr: peer_one_addr_v4,
        port: session.peer_one_addr.port(),
        peer_id: session.peer_one_id.clone(),
    };

    if let Ok(bytes1) = bincode::serialize(&packet1) {
        let _ = socket.send_to(&bytes1, session.peer_one_addr).await;
    }
    if let Ok(bytes2) = bincode::serialize(&packet2) {
        let _ = socket.send_to(&bytes2, session.peer_two_addr).await;
    }

    info!("已通知 {} 和 {} 开始打洞", session.peer_one_id, session.peer_two_id);
}

async fn notify_relay_enabled(
    socket: &UdpSocket,
    id_map: &Arc<Mutex<HashMap<String, ClientInfo>>>,
    src_id: &str,
    target_id: &str,
    src_vip: &str,
) {
    let id_map = id_map.lock().await;

    let src_client = id_map.get(src_id);
    let target_client = id_map.get(target_id);

    if let (Some(src), Some(target)) = (src_client, target_client) {
        // 通知源客户端
        let packet1 = Packet::RelayEnabled {
            peer_id: target_id.to_string(),
            peer_vip: target.vip.unwrap().to_string(),
        };
        if let Ok(bytes) = bincode::serialize(&packet1) {
            let _ = socket.send_to(&bytes, src.addr).await;
        }

        // 通知目标客户端
        let packet2 = Packet::RelayEnabled {
            peer_id: src_id.to_string(),
            peer_vip: src_vip.to_string(),
        };
        if let Ok(bytes) = bincode::serialize(&packet2) {
            let _ = socket.send_to(&bytes, target.addr).await;
        }
    }
}

fn get_session_key(id1: &str, id2: &str) -> String {
    if id1 < id2 {
        format!("{}_{}", id1, id2)
    } else {
        format!("{}_{}", id2, id1)
    }
}

fn parse_cidr(cidr: &str) -> Result<(Ipv4Addr, u8)> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        anyhow::bail!("无效的 CIDR 格式");
    }
    let ip: Ipv4Addr = parts[0].parse()?;
    let prefix: u8 = parts[1].parse()?;
    Ok((ip, prefix))
}

fn next_ip(base: Ipv4Addr, offset: u32) -> Ipv4Addr {
    let mut octets = base.octets();
    let mut val = u32::from_be_bytes(octets);
    val += offset;
    octets = val.to_be_bytes();
    Ipv4Addr::from(octets)
}

async fn create_tun_device(tun_name: &Option<String>, ip: Ipv4Addr) -> Result<tun::AsyncDevice> {
    let mut config = tun::Configuration::default();
    config
        .address(ip)
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        .up();

    #[cfg(not(target_os = "windows"))]
    config.destination(ip);

    if let Some(name) = tun_name {
        config.name(name);
    }

    #[cfg(target_os = "linux")]
    config.platform(|config| {
        config.packet_information(true);
    });

    tun::create_as_async(&config).context("创建 TUN 设备失败")
}
