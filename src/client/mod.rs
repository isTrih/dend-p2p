use crate::config::Config;
use crate::protocol::{Packet, get_dest_ip};
use crate::cache::ClientCache;
use anyhow::{Result, Context};
use log::{info, error, warn, debug};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::net::UdpSocket;
use std::collections::HashMap;

// ==================== 连接模式管理 ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    Direct,      // 直连模式（打洞成功）
    Relay,       // 中转模式（通过服务器转发）
    Disconnected, // 断开状态
}

impl From<u8> for ConnectionMode {
    fn from(val: u8) -> Self {
        match val {
            0 => ConnectionMode::Direct,
            1 => ConnectionMode::Relay,
            _ => ConnectionMode::Disconnected,
        }
    }
}

impl From<ConnectionMode> for u8 {
    fn from(mode: ConnectionMode) -> Self {
        match mode {
            ConnectionMode::Direct => 0,
            ConnectionMode::Relay => 1,
            ConnectionMode::Disconnected => 2,
        }
    }
}

struct ConnectionState {
    mode: ConnectionMode,
    peer_addr: Option<SocketAddr>,
    peer_vip: Option<Ipv4Addr>,
    peer_id: String,
    is_p2p: bool,
    last_punch: Instant,
    punch_attempts: u32,
    last_latency: i32,
    last_mode_change: Instant,
    is_connecting: bool,
    connect_failed: bool,
    connect_message: String,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::Disconnected,
            peer_addr: None,
            peer_vip: None,
            peer_id: String::new(),
            is_p2p: false,
            last_punch: Instant::now(),
            punch_attempts: 0,
            last_latency: -1,
            last_mode_change: Instant::now(),
            is_connecting: false,
            connect_failed: false,
            connect_message: String::new(),
        }
    }
}

// ==================== 对等节点状态 ====================

struct PeerState {
    addr: SocketAddr,
    vip: Ipv4Addr,
    is_alive: bool,
    latency: i32,
}

// ==================== 打洞配置 ====================

#[derive(Debug, Clone)]
struct PunchHoleConfig {
    max_concurrency: usize,
    port_range: u16,
    base_port_offset: i32,
    enable_relay: bool,
    punch_timeout: u64,
    relay_fallback: bool,
}

impl Default for PunchHoleConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 2,
            port_range: 6,
            base_port_offset: 0,
            enable_relay: true,
            punch_timeout: 10,
            relay_fallback: true,
        }
    }
}

// ==================== 客户端主结构 ====================

pub async fn run(config: Config) -> Result<()> {
    info!("正在初始化客户端...");

    // 加载或创建客户端 ID
    let cache = ClientCache::load_or_create();
    let client_id = cache.client_id.clone();
    info!("客户端 ID: {}", client_id);

    // 解析服务器地址
    let server_addr: SocketAddr = config.server_addr.parse().context("服务器地址无效")?;
    info!("服务器地址: {}", server_addr);

    // 绑定 UDP 端口
    let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let local_addr = socket.local_addr()?;
    info!("客户端绑定端口: {}", local_addr);

    // 初始化连接状态
    let conn_state = Arc::new(RwLock::new(ConnectionState::default()));
    let peer_map: Arc<Mutex<HashMap<Ipv4Addr, PeerState>>> = Arc::new(Mutex::new(HashMap::new()));

    // 打洞配置
    let punch_config = Arc::new(PunchHoleConfig::default());

    // ==================== 阶段 1: 握手 ====================
    let my_ip: Ipv4Addr;
    let network_cidr: String;

    info!("正在与服务器握手...");
    loop {
        let packet = Packet::Handshake {
            token: config.token.clone(),
            client_id: client_id.clone(),
        };
        let bytes = bincode::serialize(&packet)?;

        socket.send_to(&bytes, server_addr).await?;
        debug!("已发送握手请求");

        let mut buf = [0u8; 1024];
        let res = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await;

        match res {
            Ok(Ok((len, src))) => {
                if src == server_addr {
                    if let Ok(Packet::HandshakeAck { ip, cidr }) = bincode::deserialize::<Packet>(&buf[..len]) {
                        info!("握手成功! 分配 IP: {}", ip);
                        my_ip = ip.parse()?;
                        network_cidr = cidr;
                        break;
                    }
                }
            }
            Ok(Err(e)) => error!("接收错误: {}", e),
            Err(_) => warn!("握手超时, 正在重试..."),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // ==================== 阶段 2: 创建 TUN 设备 ====================
    let tun_dev = create_tun_device(&config.tun_name, my_ip).await?;
    info!("TUN 设备已创建: {}", my_ip);

    let (mut tun_reader, mut tun_writer) = tokio::io::split(tun_dev);

    // ==================== 阶段 3: 注册服务器请求处理器 ====================

    let socket_clone = socket.clone();
    let conn_state_clone = conn_state.clone();
    let peer_map_clone = peer_map.clone();
    let punch_config_clone = punch_config.clone();
    let server_addr_clone = server_addr;

    // 接收服务器命令的任务
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_clone.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    if src == server_addr_clone {
                        let packet_data = &buf[..len];
                        if let Ok(packet) = bincode::deserialize::<Packet>(packet_data) {
                            handle_server_packet(
                                &packet,
                                &socket_clone,
                                src,
                                &conn_state_clone,
                                &peer_map_clone,
                                &punch_config_clone,
                            ).await;
                        }
                    }
                }
                Err(e) => {
                    debug!("UDP 接收错误: {}", e);
                    break;
                }
            }
        }
    });

    // ==================== 阶段 4: 心跳和打洞任务 ====================
    let socket_ka = socket.clone();
    let conn_state_ka = conn_state.clone();
    let peer_map_ka = peer_map.clone();
    let punch_config_ka = punch_config.clone();

    tokio::spawn(async move {
        let mut last_beat_time = Instant::now();
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(last_beat_time);

            // 发送心跳到服务器 (每 3 秒)
            if elapsed.as_secs() >= 3 {
                let ping = Packet::Ping(now.duration_since(Instant::now()).as_millis() as u64);
                if let Ok(bytes) = bincode::serialize(&ping) {
                    let _ = socket_ka.send_to(&bytes, server_addr).await;
                }
                last_beat_time = now;
            }

            // 获取当前连接状态
            let state = conn_state_ka.read().await;

            // 如果有对等节点，处理打洞和心跳
            if let (Some(peer_addr), Some(peer_vip)) = (state.peer_addr, state.peer_vip) {
                drop(state);

                let state = conn_state_ka.read().await;
                let mut need_write = false;

                if !state.is_p2p {
                    // P2P 模式：打洞
                    if now.duration_since(state.last_punch).as_millis() >= 500 {
                        drop(state);

                        let punch_config = &*punch_config_ka;
                        let peer_addr_copy = peer_addr;
                        let peer_port = peer_addr_copy.port();
                        let ports: Vec<u16> = generate_port_list(
                            peer_port,
                            punch_config.port_range,
                            punch_config.base_port_offset,
                        ).collect();

                        let concurrency = punch_config.max_concurrency;
                        let mut handles = Vec::new();

                        // 并发发送打洞包
                        for (i, port) in ports.into_iter().enumerate() {
                            // 不要向自己打洞
                            if local_addr.port() == port && local_addr.ip() == peer_addr_copy.ip() {
                                continue;
                            }

                            let target_addr = SocketAddr::new(peer_addr_copy.ip(), port);
                            let socket = socket_ka.clone();
                            let packet = Packet::Punch {
                                vip: my_ip.to_string(),
                            };

                            if let Ok(bytes) = bincode::serialize(&packet) {
                                let handle = tokio::spawn(async move {
                                    let _ = socket.send_to(&bytes, target_addr).await;
                                    // 随机延迟避免同时到达
                                    tokio::time::sleep(Duration::from_millis((i * 10) as u64)).await;
                                });
                                handles.push(handle);
                            }

                            // 限制并发数
                            if handles.len() >= concurrency {
                                let _ = handles.remove(0).await;
                            }
                        }

                        // 等待剩余的协程
                        for handle in handles {
                            let _ = handle.await;
                        }

                        need_write = true;
                    }
                } else {
                    // 直连模式：发送心跳维持连接
                    if now.duration_since(state.last_punch).as_secs() >= 2 {
                        let ping = Packet::Ping(now.duration_since(Instant::now()).as_millis() as u64);
                        if let Ok(bytes) = bincode::serialize(&ping) {
                            let _ = socket_ka.send_to(&bytes, peer_addr).await;
                        }
                        need_write = true;
                    }
                }

                if need_write {
                    let mut state = conn_state_ka.write().await;
                    state.last_punch = now;
                    state.punch_attempts += 1;
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    // ==================== 阶段 5: TUN -> Socket 数据转发 ====================
    let socket_send = socket.clone();
    let conn_state_send = conn_state.clone();
    let peer_map_send = peer_map.clone();

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tun_reader.read(&mut buf).await {
                Ok(len) => {
                    if len == 0 { break; }
                    let data = &buf[..len];

                    if let Some(dst_ip) = get_dest_ip(data) {
                        let target_addr = {
                            let state = conn_state_send.read().await;
                            if state.is_p2p && state.peer_addr.is_some() {
                                state.peer_addr.unwrap()
                            } else {
                                server_addr
                            }
                        };

                        let packet = Packet::Data(data.to_vec());
                        if let Ok(bytes) = bincode::serialize(&packet) {
                            if let Err(e) = socket_send.send_to(&bytes, target_addr).await {
                                error!("发送到 {} 失败: {}", target_addr, e);
                            }
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

    // ==================== 阶段 6: Socket -> TUN 数据接收 ====================
    let socket_recv = socket.clone();
    let conn_state_recv = conn_state.clone();
    let peer_map_recv = peer_map.clone();

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    let packet_data = &buf[..len];

                    if let Ok(packet) = bincode::deserialize::<Packet>(packet_data) {
                        match packet {
                            Packet::Data(data) => {
                                // 如果是从直连对等节点来的，直接写入 TUN
                                if let Err(e) = tun_writer.write_all(&data).await {
                                    error!("写入 TUN 错误: {}", e);
                                }
                            }
                            Packet::Punch { vip } => {
                                // 收到打洞包，说明 P2P 直连已建立
                                if let Ok(vip_addr) = vip.parse::<Ipv4Addr>() {
                                    info!("收到来自 {} ({}) 的打洞包! P2P 连接建立。", vip_addr, src);

                                    let mut state = conn_state_recv.write().await;
                                    state.is_p2p = true;
                                    state.peer_addr = Some(src);
                                    state.peer_vip = Some(vip_addr);
                                    state.mode = ConnectionMode::Direct;
                                    state.last_mode_change = Instant::now();
                                    state.is_connecting = false;
                                    state.connect_message = String::new();

                                    info!("切换到直连模式");
                                }
                            }
                            Packet::Ping(ts) => {
                                // 回复 Pong
                                let pong = Packet::Pong(ts);
                                if let Ok(bytes) = bincode::serialize(&pong) {
                                    let _ = socket_recv.send_to(&bytes, src).await;
                                }
                            }
                            Packet::Pong(ts) => {
                                let now = Instant::now();
                                let rtt = now.duration_since(Instant::now()).as_millis() as u64;

                                let mut state = conn_state_recv.write().await;
                                state.last_latency = rtt as i32;

                                if src == server_addr {
                                    info!("【中转】延迟: {} ms", rtt);
                                } else {
                                    info!("【P2P】延迟: {} ms (来自 {})", rtt, src);
                                }
                            }
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

    // 保持主线程运行
    std::future::pending::<()>().await;
    Ok(())
}

// ==================== 处理服务器命令 ====================

async fn handle_server_packet(
    packet: &Packet,
    socket: &UdpSocket,
    src: SocketAddr,
    conn_state: &Arc<RwLock<ConnectionState>>,
    peer_map: &Arc<Mutex<HashMap<Ipv4Addr, PeerState>>>,
    punch_config: &Arc<PunchHoleConfig>,
) {
    match packet {
        Packet::PeerInfo { peer_vip, peer_addr } => {
            if let (Ok(vip), Ok(raddr)) = (peer_vip.parse::<Ipv4Addr>(), peer_addr.parse::<SocketAddr>()) {
                info!("收到 Peer 信息: VIP={} Real={}", vip, raddr);

                let mut state = conn_state.write().await;
                state.peer_addr = Some(raddr);
                state.peer_vip = Some(vip);
                state.is_connecting = true;
                state.connect_message = "正在建立 P2P 连接...".to_string();
                state.connect_failed = false;
                state.punch_attempts = 0;

                drop(state);

                // 启动打洞超时检测
                let conn_state = conn_state.clone();
                let punch_config = punch_config.clone();

                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(punch_config.punch_timeout)).await;

                    let mut state = conn_state.write().await;
                    if !state.is_p2p && state.is_connecting {
                        warn!("打洞超时（{}秒），切换到中转模式", punch_config.punch_timeout);

                        if punch_config.enable_relay && punch_config.relay_fallback {
                            state.mode = ConnectionMode::Relay;
                            state.connect_message = "使用中转模式".to_string();
                            info!("已切换到中转模式");
                        } else {
                            state.connect_failed = true;
                            state.connect_message = "连接失败：无法建立 P2P 连接".to_string();
                            state.is_connecting = false;
                        }
                    }
                });
            }
        }
        Packet::ConnectPeer { peer_vip, peer_addr, port, .. } => {
            // 服务器通知连接对等节点
            // peer_vip 和 peer_addr 已经是 Ipv4Addr 类型
            let vip = *peer_vip;
            let raddr = SocketAddr::new(std::net::IpAddr::V4(*peer_addr), *port);

            info!("服务器通知连接对等节点: VIP={} Addr={}", vip, raddr);

            let mut state = conn_state.write().await;
            state.peer_addr = Some(raddr);
            state.peer_vip = Some(vip);
            state.is_connecting = true;
            state.connect_message = "正在打洞...".to_string();
        }
        Packet::ChangePort { password } => {
            info!("收到服务器更换端口命令");
            // 实际更换端口逻辑在客户端比较复杂，这里简化处理
        }
        Packet::RelayEnabled { peer_id, peer_vip } => {
            if let Ok(vip) = peer_vip.parse::<Ipv4Addr>() {
                let mut state = conn_state.write().await;
                state.mode = ConnectionMode::Relay;
                state.peer_id = peer_id.clone();
                state.peer_vip = Some(vip);
                state.is_connecting = false;
                state.connect_message = String::new();
                info!("中转模式已启用，对等节点: {}", peer_id);
            }
        }
        Packet::DisconnectPeer => {
            info!("收到断开连接命令");
            let mut state = conn_state.write().await;
            state.mode = ConnectionMode::Disconnected;
            state.peer_addr = None;
            state.peer_vip = None;
            state.is_p2p = false;
            state.is_connecting = false;
            state.connect_message = String::new();
        }
        _ => {}
    }
}

// ==================== 端口列表生成 ====================

fn generate_port_list(
    base_port: u16,
    port_range: u16,
    offset: i32,
) -> impl Iterator<Item = u16> {
    let start = (base_port as i32 + offset).max(1) as u16;
    let end = (start + port_range).min(65535);

    (start..=end).filter(move |&p| p > 0)
}

// ==================== TUN 设备创建 ====================

async fn create_tun_device(tun_name: &Option<String>, ip: Ipv4Addr) -> Result<tun::AsyncDevice> {
    let mut config = tun::Configuration::default();
    config
        .address(ip)
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        .up();

    #[cfg(not(target_os = "windows"))]
    config.destination(Ipv4Addr::new(10, 10, 0, 1));

    if let Some(name) = tun_name {
        #[cfg(target_os = "macos")]
        {
            if !name.starts_with("utun") {
                warn!("在 macOS 上，TUN 设备名称必须以 'utun' 开头，将使用系统自动分配的名称。");
            } else {
                config.name(name);
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            config.name(name);
        }
    }

    #[cfg(target_os = "linux")]
    config.platform(|config| {
        config.packet_information(true);
    });

    tun::create_as_async(&config).context("创建 TUN 设备失败")
}

// ==================== 获取连接状态 API ====================

pub async fn get_connection_status() -> String {
    // 返回 JSON 格式的连接状态
    serde_json::json!({
        "mode": "disconnected",
        "modeCode": 2,
        "isConnected": false,
        "isConnecting": false,
        "connectFailed": false,
        "connectMessage": ""
    }).to_string()
}
