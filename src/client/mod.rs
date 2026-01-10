use crate::config::Config;
use crate::protocol::{Packet, get_dest_ip};
use crate::cache::ClientCache;
use crate::client_webui;
use anyhow::{Result, Context};
use log::{info, error, warn, debug};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio::net::UdpSocket;
use std::collections::HashMap;

// ==================== NAT 类型定义 ====================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatType {
    Nat1,     // 完全锥形 (Full Cone) - 任何外部IP都可以连接
    Nat2,     // 地址限制锥形 (Address Restricted Cone)
    Nat3,     // 端口限制锥形 (Port Restricted Cone)
    Nat4,     // 对称型 (Symmetric) - 每个目标地址映射不同端口
    Unknown,  // 未知
}

impl NatType {
    pub fn as_str(&self) -> &'static str {
        match self {
            NatType::Nat1 => "1",
            NatType::Nat2 => "2",
            NatType::Nat3 => "3",
            NatType::Nat4 => "4",
            NatType::Unknown => "unknown",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            NatType::Nat1 => "NAT1 (完全锥形)",
            NatType::Nat2 => "NAT2 (地址限制)",
            NatType::Nat3 => "NAT3 (端口限制)",
            NatType::Nat4 => "NAT4 (对称型)",
            NatType::Unknown => "NAT (未知)",
        }
    }
}

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
    punch_initiated: bool, // v0.2.3: 是否已发起打洞（用户点击连接后才为true）
    server_ping_time: Option<Instant>, // 记录发送给服务器的 Ping 时间
    p2p_ping_time: Option<Instant>,    // 记录发送给 P2P 对等节点的 Ping 时间
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
            punch_initiated: false,
            server_ping_time: None,
            p2p_ping_time: None,
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
    keepalive_interval: u64,  // v0.2.3: 保活间隔（秒）
    punch_interval: u64,      // v0.2.3: 打洞包间隔（毫秒）
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
            keepalive_interval: 30,  // 每30秒发送一次保活包（NAT超时通常为30-60秒）
            punch_interval: 200,     // 每200ms发送一次打洞包
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

    info!("========================================");
    info!("  dend-p2p v{} - P2P 直连组网工具", env!("CARGO_PKG_VERSION"));
    info!("========================================");
    info!("我的连接码: {}", client_id);
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
                    if let Ok(Packet::HandshakeAck { ip, cidr: _ }) = bincode::deserialize::<Packet>(&buf[..len]) {
                        info!("========================================");
                        info!("  连接成功!");
                        info!("========================================");
                        info!("我的连接码: {}", client_id);
                        info!("我的虚拟IP: {}/24", ip);
                        my_ip = ip.parse()?;
                        break;
                    }
                }
            }
            Ok(Err(e)) => error!("接收错误: {}", e),
            Err(_) => warn!("握手超时, 正在重试..."),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // ==================== 阶段 1.5: NAT 类型检测 ====================
    let nat_type = detect_nat_type(socket.clone(), server_addr, local_addr).await;
    info!("NAT 类型: {}", nat_type.display_name());

    // ==================== 阶段 2: 创建 TUN 设备 ====================
    let tun_dev = create_tun_device(&config.tun_name, my_ip).await?;
    info!("TUN 设备已创建: {}", my_ip);

    let (mut tun_reader, mut tun_writer) = tokio::io::split(tun_dev);

    // ==================== 阶段 3: 启动 WEBUI 服务器（如果启用） ====================
    let _conn_state_webui = conn_state.clone();
    let client_id_webui = client_id.clone();
    let my_ip_webui = my_ip;
    let webui_enabled = config.webui;

    if webui_enabled {
        tokio::spawn(async move {
            client_webui::set_local_info(&client_id_webui, &my_ip_webui.to_string(), nat_type.as_str());
            client_webui::start_web_server().await;
        });
    } else {
        // 即使不启动WebUI，也设置本地信息（供API使用）
        client_webui::set_local_info(&client_id_webui, &my_ip_webui.to_string(), nat_type.as_str());
        info!("WebUI 已禁用（Linux 默认），如需启用请在配置文件中设置 webui = true");
    }

    // ==================== 阶段 4: 注册服务器请求处理器 ====================

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

    // ==================== 阶段 5: 心跳、保活和打洞任务 ====================
    let socket_ka = socket.clone();
    let conn_state_ka = conn_state.clone();
    let _peer_map_ka = peer_map.clone();
    let punch_config_ka = punch_config.clone();
    let server_addr_ka = server_addr;

    tokio::spawn(async move {
        let mut last_beat_time = Instant::now();
        let mut last_keepalive_time = Instant::now();

        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(last_beat_time);
            let keepalive_elapsed = now.duration_since(last_keepalive_time);

            // v0.2.3: 检查是否有 P2P 连接触发请求
            if let Some(target_id) = client_webui::get_p2p_trigger() {
                info!("收到 P2P 连接请求: {}", target_id);

                // 发送连接请求到服务器
                let packet = Packet::RequestConnect {
                    target_id: target_id.clone(),
                    password: String::new(),
                };

                if let Ok(bytes) = bincode::serialize(&packet) {
                    let _ = socket_ka.send_to(&bytes, server_addr_ka).await;
                    info!("已发送 P2P 连接请求到服务器");

                    // 更新状态
                    let mut state = conn_state_ka.write().await;
                    state.is_connecting = true;
                    state.connect_message = "正在等待服务器响应...".to_string();
                    state.punch_initiated = true;  // 标记已开始打洞
                }
            }

            // 发送心跳到服务器 (每 3 秒)
            if elapsed.as_secs() >= 3 {
                // 使用系统时间作为时间戳
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or(Duration::ZERO)
                    .as_millis() as u64;
                let ping = Packet::Ping(timestamp);
                if let Ok(bytes) = bincode::serialize(&ping) {
                    let _ = socket_ka.send_to(&bytes, server_addr_ka).await;
                    // 记录发送时间用于计算延迟
                    let mut state = conn_state_ka.write().await;
                    state.server_ping_time = Some(Instant::now());
                }
                last_beat_time = now;
            }

            // 获取当前连接状态
            let (is_p2p, punch_initiated, peer_addr, peer_vip, last_punch) = {
                let state = conn_state_ka.read().await;
                (
                    state.is_p2p,
                    state.punch_initiated,
                    state.peer_addr,
                    state.peer_vip,
                    state.last_punch,
                )
            };

            // v0.2.3: 增强保活机制
            // 如果 P2P 已建立，定期发送保活包防止 NAT 映射超时
            if is_p2p && keepalive_elapsed.as_secs() >= punch_config_ka.keepalive_interval {
                // 发送保活包到对等节点
                if let Some(addr) = peer_addr {
                    // 使用系统时间作为时间戳
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or(Duration::ZERO)
                        .as_millis() as u64;
                    let ping = Packet::Ping(timestamp);
                    if let Ok(bytes) = bincode::serialize(&ping) {
                        let _ = socket_ka.send_to(&bytes, addr).await;
                        debug!("发送 P2P 保活包到 {}", addr);

                        // 记录发送时间
                        let mut state = conn_state_ka.write().await;
                        state.p2p_ping_time = Some(Instant::now());
                    }
                }

                last_keepalive_time = now;
            }

            // 如果有对等节点，处理打洞
            if let (Some(addr), Some(_vip)) = (peer_addr, peer_vip) {
                // v0.2.3: 只有用户点击连接后才开始打洞
                if punch_initiated && !is_p2p {
                    let punch_config = &*punch_config_ka;
                    let punch_interval = punch_config.punch_interval;

                    // 检查是否到了发送打洞包的时间
                    if now.duration_since(last_punch).as_millis() >= punch_interval as u128 {
                        let peer_port = addr.port();
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
                            if local_addr.port() == port && local_addr.ip() == addr.ip() {
                                continue;
                            }

                            let target_addr = SocketAddr::new(addr.ip(), port);
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

                        let mut state = conn_state_ka.write().await;
                        state.last_punch = now;
                        state.punch_attempts += 1;
                    }
                }
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    // ==================== 阶段 6: TUN -> Socket 数据转发 ====================
    let socket_send = socket.clone();
    let conn_state_send = conn_state.clone();
    let _peer_map_send = peer_map.clone();

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match tun_reader.read(&mut buf).await {
                Ok(len) => {
                    if len == 0 { break; }
                    let data = &buf[..len];

                    if let Some(_dst_ip) = get_dest_ip(data) {
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

    // ==================== 阶段 7: Socket -> TUN 数据接收 ====================
    let socket_recv = socket.clone();
    let conn_state_recv = conn_state.clone();
    let _peer_map_recv = peer_map.clone();
    let _punch_config_recv = punch_config.clone();

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
                                    state.punch_initiated = false;  // 重置打洞标记

                                    info!("========================================");
                                    info!("  P2P 直连已建立!");
                                    info!("========================================");
                                    info!("对等节点: {}", vip_addr);
                                    info!("连接模式: 直连");
                                    info!("延迟: {} ms", state.last_latency);

                                    // 更新 WEBUI 状态
                                    client_webui::set_connection_status(
                                        client_webui::ConnectionStatus {
                                            mode: "直连".to_string(),
                                            mode_code: 0,
                                            is_connected: true,
                                            status_text: "P2P 连接已建立".to_string(),
                                            is_connecting: false,
                                            connect_failed: false,
                                            connect_message: String::new(),
                                            peer_ip: src.ip().to_string(),
                                            peer_latency: state.last_latency,
                                            online_devices: vec![],
                                        }
                                    );
                                }
                            }
                            Packet::Ping(_ts) => {
                                // 回复 Pong (直接回传时间戳，不用于延迟计算)
                                let pong = Packet::Pong(0);
                                if let Ok(bytes) = bincode::serialize(&pong) {
                                    let _ = socket_recv.send_to(&bytes, src).await;
                                }
                            }
                            Packet::Pong(_ts) => {
                                // 使用记录的发送时间来计算延迟
                                let mut state = conn_state_recv.write().await;
                                let now = Instant::now();
                                let rtt = if src == server_addr {
                                    // 服务器延迟
                                    if let Some(ping_time) = state.server_ping_time {
                                        state.server_ping_time = None; // 清除已使用的记录
                                        now.duration_since(ping_time).as_millis() as i32
                                    } else {
                                        -1 // 无法计算
                                    }
                                } else {
                                    // P2P 延迟
                                    if let Some(ping_time) = state.p2p_ping_time {
                                        state.p2p_ping_time = None; // 清除已使用的记录
                                        now.duration_since(ping_time).as_millis() as i32
                                    } else {
                                        -1 // 无法计算
                                    }
                                };

                                state.last_latency = rtt;

                                // 过滤自身连接
                                let is_self = if let Some(peer_addr) = state.peer_addr {
                                    src.port() == peer_addr.port() && src.ip() == peer_addr.ip()
                                } else {
                                    false
                                };

                                if is_self {
                                    debug!("收到自身的Pong包，忽略");
                                } else if rtt < 0 {
                                    debug!("收到Pong但无对应Ping记录，忽略");
                                } else if src == server_addr {
                                    info!("【中转】延迟: {} ms", rtt);
                                } else {
                                    info!("【P2P】延迟: {} ms (来自 {})", rtt, src);

                                    // 更新 WEBUI 延迟显示
                                    if state.is_p2p {
                                        client_webui::set_connection_status(
                                            client_webui::ConnectionStatus {
                                                mode: "直连".to_string(),
                                                mode_code: 0,
                                                is_connected: true,
                                                status_text: "P2P 连接已建立".to_string(),
                                                is_connecting: false,
                                                connect_failed: false,
                                                connect_message: String::new(),
                                                peer_ip: src.ip().to_string(),
                                                peer_latency: state.last_latency,
                                                online_devices: vec![],
                                            }
                                        );
                                    }
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

// ==================== NAT 类型检测 ====================

async fn detect_nat_type(socket: Arc<UdpSocket>, server_addr: SocketAddr, local_addr: SocketAddr) -> NatType {
    // v0.2.3 NAT 类型检测流程：
    // 1. 向服务器发送检测包，服务器返回它看到的源IP和端口
    // 2. 比较本地端口和服务器看到的端口
    // 3. 如果相同，可能是 NAT1 (完全锥形) 或无 NAT
    // 4. 如果不同，进一步检测 NAT 类型

    info!("开始 NAT 类型检测...");

    // 向服务器发送 NAT 类型检测请求
    let packet = Packet::NatTypeDetect {
        port: local_addr.port(),
    };

    if let Ok(bytes) = bincode::serialize(&packet) {
        if let Ok(_) = socket.send_to(&bytes, server_addr).await {
            // 等待服务器响应
            let mut buf = [0u8; 1024];
            let timeout_result = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await;

            match timeout_result {
                Ok(Ok((len, src))) => {
                    if src == server_addr {
                        if let Ok(Packet::NatTypeResult { nat_type, mapped_port, .. }) =
                            bincode::deserialize::<Packet>(&buf[..len])
                        {
                            info!("NAT 检测结果: 类型={}, 映射端口={}", nat_type, mapped_port);

                            return match nat_type {
                                1 => NatType::Nat1,
                                2 => NatType::Nat2,
                                3 => NatType::Nat3,
                                4 => NatType::Nat4,
                                _ => NatType::Unknown,
                            };
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!("NAT 检测接收错误: {}", e);
                }
                Err(_) => {
                    warn!("NAT 检测超时");
                }
            }
        }
    }

    // 检测失败，返回 Unknown
    NatType::Unknown
}

// ==================== 处理服务器命令 ====================

async fn handle_server_packet(
    packet: &Packet,
    _socket: &UdpSocket,
    _src: SocketAddr,
    conn_state: &Arc<RwLock<ConnectionState>>,
    _peer_map: &Arc<Mutex<HashMap<Ipv4Addr, PeerState>>>,
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

                            info!("========================================");
                            info!("  P2P 直连失败，切换到中转模式");
                            info!("========================================");
                            info!("对等节点: {}", state.peer_vip.map(|v| v.to_string()).unwrap_or_else(|| "未知".to_string()));
                            info!("连接模式: 中转");

                            // 更新 WEBUI 状态
                            client_webui::set_connection_status(
                                client_webui::ConnectionStatus {
                                    mode: "中转".to_string(),
                                    mode_code: 1,
                                    is_connected: true,
                                    status_text: "中转模式".to_string(),
                                    is_connecting: false,
                                    connect_failed: false,
                                    connect_message: String::new(),
                                    peer_ip: state.peer_addr.map(|a| a.ip().to_string()).unwrap_or_default(),
                                    peer_latency: state.last_latency,
                                    online_devices: vec![],
                                }
                            );
                        } else {
                            state.connect_failed = true;
                            state.connect_message = "连接失败：无法建立 P2P 连接".to_string();
                            state.is_connecting = false;

                            error!("========================================");
                            error!("  P2P 连接失败");
                            error!("========================================");
                            error!("对等节点: {}", state.peer_vip.map(|v| v.to_string()).unwrap_or_else(|| "未知".to_string()));
                        }
                    }
                });
            }
        }
        Packet::ConnectPeer { peer_vip, peer_addr, port, .. } => {
            // 服务器通知连接对等节点
            let vip = *peer_vip;
            let raddr = SocketAddr::new(std::net::IpAddr::V4(*peer_addr), *port);

            info!("========================================");
            info!("  收到服务器连接指令");
            info!("========================================");
            info!("目标连接码: {}", vip);
            info!("目标地址: {}", raddr);
            info!("请在WEBUI中点击连接按钮发起P2P连接");

            let mut state = conn_state.write().await;
            state.peer_addr = Some(raddr);
            state.peer_vip = Some(vip);
            state.is_connecting = false;
            state.connect_message = "等待用户点击连接...".to_string();

            // v0.2.3: 只有 punch_initiated 为 true 时才真正开始打洞
            // 这样可以确保用户点击连接按钮后才开始 P2P 尝试
        }
        Packet::ChangePort { password: _ } => {
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

                // 更新 WEBUI 状态
                client_webui::set_connection_status(
                    client_webui::ConnectionStatus {
                        mode: "中转".to_string(),
                        mode_code: 1,
                        is_connected: true,
                        status_text: "中转模式已启用".to_string(),
                        is_connecting: false,
                        connect_failed: false,
                        connect_message: String::new(),
                        peer_ip: peer_vip.clone(),
                        peer_latency: -1,
                        online_devices: vec![],
                    }
                );
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
            state.punch_initiated = false;

            // 更新 WEBUI 状态
            client_webui::set_connection_status(
                client_webui::ConnectionStatus {
                    mode: "断开".to_string(),
                    mode_code: 2,
                    is_connected: false,
                    status_text: "已断开".to_string(),
                    is_connecting: false,
                    connect_failed: false,
                    connect_message: String::new(),
                    peer_ip: String::new(),
                    peer_latency: -1,
                    online_devices: vec![],
                }
            );
        }
        Packet::DeviceList { devices } => {
            debug!("收到设备列表，共 {} 个设备", devices.len());
            // 更新 WEBUI 中的在线设备列表
            let online_devices: Vec<client_webui::OnlineDevice> = devices.iter()
                .filter(|d| d.is_online)
                .map(|d| client_webui::OnlineDevice {
                    client_id: d.client_id.clone(),
                    vip: d.vip.clone(),
                    is_online: d.is_online,
                })
                .collect();

            client_webui::update_online_devices(online_devices);
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
        "connectMessage": "",
        "natType": "unknown"
    }).to_string()
}
