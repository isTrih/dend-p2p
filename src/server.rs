use crate::config::Config;
use crate::protocol::{Packet, get_dest_ip};
use anyhow::{Result, Context};
use log::{info, error, warn};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct ClientInfo {
    addr: SocketAddr,
    last_seen: std::time::Instant, // Updated on keepalive/data
    client_id: String,
}

pub async fn run(config: Config) -> Result<()> {
    // 监听地址
    info!("Starting server on {}", config.server_addr);
    
    // Bind UDP socket
    let socket = Arc::new(UdpSocket::bind(&config.server_addr).await?);
    
    // 检查是否无人连接，如果有连接才开始 active
    info!("Waiting for incoming connections (Low Power Idle Mode)...");

    // IP allocation pool (very simple implementation)
    // Assume /24 network for simplicity as implied by "10.0.0.0/24"
    let cidr_str = config.cidr.clone().context("CIDR is required for server mode")?;
    // Parse CIDR "10.10.0.0/24" -> IP: 10.10.0.0, Prefix: 10.10.0.x
    // We assume the first IP .1 is server, pool starts at .2
    let (network_ip, network_prefix_len) = parse_cidr(&cidr_str)?;
    
    if network_prefix_len != 24 {
        warn!("Currently only /24 networks are fully supported for simple allocation logic.");
    }

    let server_ip = next_ip(network_ip, 1);
    
    // Setup TUN device for server
    let mut tun_config = tun::Configuration::default();
    tun_config
        .address(server_ip)
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        .up();
    
    #[cfg(not(target_os = "windows"))]
    tun_config.destination(server_ip); // Point-to-point usually needs destination, but depends on platform

    if let Some(name) = &config.tun_name {
        tun_config.name(name);
    }

    #[cfg(target_os = "linux")]
    tun_config.platform(|config| {
        config.packet_information(true);
    });

    let tun_dev = tun::create_as_async(&tun_config).context("Failed to create TUN device")?;
    info!("Server TUN device created with IP: {}", server_ip);

    let (mut tun_reader, tun_writer) = tokio::io::split(tun_dev);

    // Map Virtual IP -> Client Real Addr
    let peer_map: Arc<Mutex<HashMap<Ipv4Addr, ClientInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    
    // Auto-release task
    let peer_map_clean = peer_map.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let mut peers = peer_map_clean.lock().await;
            let now = std::time::Instant::now();
            // Remove clients silent for > 60s
            let before_count = peers.len();
            peers.retain(|ip, client| {
                let active = now.duration_since(client.last_seen).as_secs() < 60;
                if !active {
                    info!("Client {} ({}) timed out, releasing IP {}", client.client_id, client.addr, ip);
                }
                active
            });
        }
    });

    // Loop 1: Handle UDP packets (Client -> Server)
    let socket_recv = socket.clone();
    let peer_map_recv = peer_map.clone();
    let config_token = config.token.clone();
    
    // Simple Allocator: Scan 2..254 to find free IP
    let find_free_ip = move |peers: &HashMap<Ipv4Addr, ClientInfo>| -> Option<Ipv4Addr> {
        for i in 2..254 {
            let ip = next_ip(network_ip, i);
            if !peers.contains_key(&ip) {
                return Some(ip);
            }
        }
        None
    };

    // We need shared access to write to TUN (from UDP)
    let tun_writer = Arc::new(Mutex::new(tun_writer));

    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        loop {
            match socket_recv.recv_from(&mut buf).await {
                Ok((len, addr)) => {
                    let packet_data = &buf[..len];
                    // Deserialize
                    match bincode::deserialize::<Packet>(packet_data) {
                        Ok(packet) => {
                            match packet {
                                Packet::Handshake { token, client_id } => {
                                    if token == config_token {
                                        let mut peers = peer_map_recv.lock().await;

                                        // Check if this client ID already has an IP
                                        let mut assigned_ip = None;
                                        for (ip, client) in peers.iter() {
                                            if client.client_id == client_id {
                                                assigned_ip = Some(*ip);
                                                break;
                                            }
                                        }

                                        let new_ip = if let Some(ip) = assigned_ip {
                                            // Update address/timestamp
                                            if let Some(c) = peers.get_mut(&ip) {
                                                c.addr = addr;
                                                c.last_seen = std::time::Instant::now();
                                            }
                                            info!("Re-handshake from known client {} ({}), keeping IP {}", client_id, addr, ip);
                                            ip
                                        } else {
                                            // Allocate new
                                            if let Some(ip) = find_free_ip(&peers) {
                                                info!("Handshake from new client {} ({}), assigning {}", client_id, addr, ip);
                                                peers.insert(ip, ClientInfo {
                                                    addr,
                                                    last_seen: std::time::Instant::now(),
                                                    client_id: client_id.clone(),
                                                });
                                                ip
                                            } else {
                                                warn!("No free IPs available for client {}", addr);
                                                continue; 
                                            }
                                        };

                                        // Relay mode naturally supports all NAT types (Cone, Symmetric, etc)
                                        // since server relays traffic and responds to the source port of the client.

                                        let resp = Packet::HandshakeAck {
                                            ip: new_ip.to_string(),
                                            cidr: cidr_str.clone(),
                                        };
                                        if let Ok(resp_bytes) = bincode::serialize(&resp) {
                                            let _ = socket_recv.send_to(&resp_bytes, addr).await;
                                        }

                                        // V2 P2P: Broadcast new peer info to other clients
                                        // For simplicity, just tell EVERYONE about the new guy, and tell the new guy about EVERYONE.
                                        // (In large scale this is bad, but for small groups it's fine)
                                        let my_vip_str = new_ip.to_string();
                                        let my_real_addr_str = addr.to_string();
                                        
                                        // 1. Tell new guy about existing peers
                                        for (peer_ip, peer_info) in peers.iter() {
                                            if *peer_ip == new_ip { continue; }
                                            let p = Packet::PeerInfo {
                                                peer_vip: peer_ip.to_string(),
                                                peer_addr: peer_info.addr.to_string(),
                                            };
                                            if let Ok(b) = bincode::serialize(&p) {
                                                let _ = socket_recv.send_to(&b, addr).await;
                                            }
                                        }

                                        // 2. Tell existing peers about new guy
                                        let p_new = Packet::PeerInfo {
                                            peer_vip: my_vip_str,
                                            peer_addr: my_real_addr_str,
                                        };
                                        if let Ok(b_new) = bincode::serialize(&p_new) {
                                            for (peer_ip, peer_info) in peers.iter() {
                                                if *peer_ip == new_ip { continue; }
                                                let _ = socket_recv.send_to(&b_new, peer_info.addr).await;
                                            }
                                        }
                                    }
                                }
                                Packet::Data(data) => {
                                    // Received IP packet from client.
                                    // 1. If dest is Server IP -> Write to TUN.
                                    // 2. If dest is another Client -> Relay.
                                    if let Some(dst) = get_dest_ip(&data) {
                                        if dst == server_ip {
                                            let mut writer = tun_writer.lock().await;
                                            let _ = writer.write_all(&data).await;
                                        } else {
                                            // Relay
                                            let mut peers = peer_map_recv.lock().await;
                                            
                                            // Optional: Update sender info from source IP in packet
                                            if data.len() >= 20 {
                                                let src = std::net::Ipv4Addr::new(data[12], data[13], data[14], data[15]);
                                                if let Some(sender) = peers.get_mut(&src) {
                                                    sender.last_seen = std::time::Instant::now();
                                                    sender.addr = addr; 
                                                }
                                            }

                                            if let Some(target) = peers.get(&dst) {
                                                let _ = socket_recv.send_to(packet_data, target.addr).await;
                                            }
                                        }
                                    }
                                }
                                Packet::Keepalive => {
                                    // Update keepalive using address map scan (inefficient but works for small scale)
                                    let mut peers = peer_map_recv.lock().await;
                                    for (_, client) in peers.iter_mut() {
                                        if client.addr == addr {
                                            client.last_seen = std::time::Instant::now();
                                            break;
                                        }
                                    }
                                }
                                Packet::Ping(ts) => {
                                    // Reply Pong immediately for RTT measurement
                                    let pong = Packet::Pong(ts);
                                    if let Ok(bytes) = bincode::serialize(&pong) {
                                        let _ = socket_recv.send_to(&bytes, addr).await;
                                    }
                                    // Also treat as keepalive
                                    let mut peers = peer_map_recv.lock().await;
                                    for (_, client) in peers.iter_mut() {
                                        if client.addr == addr {
                                            client.last_seen = std::time::Instant::now();
                                            break;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        Err(_) => {
                            // warn!("Failed to deserialize packet from {}", addr);
                        }
                    }
                }
                Err(e) => {
                    error!("UDP receive error: {}", e);
                }
            }
        }
    });

    // Loop 2: Read from TUN -> Send to Client (Server -> Client)
    let socket_tun = socket.clone();
    let peer_map_tun = peer_map.clone();
    
    let mut buf_tun = [0u8; 4096];
    loop {
        match tun_reader.read(&mut buf_tun).await {
            Ok(len) => {
                let data = &buf_tun[..len];
                if let Some(dst) = get_dest_ip(data) {
                    let peers = peer_map_tun.lock().await;
                    if let Some(client) = peers.get(&dst) {
                        // Wrap in Packet::Data
                        let packet = Packet::Data(data.to_vec());
                        if let Ok(encoded) = bincode::serialize(&packet) {
                            let _ = socket_tun.send_to(&encoded, client.addr).await;
                        }
                    } else {
                        // warning: dest not found
                    }
                }
            }
            Err(e) => {
                error!("TUN read error: {}", e);
                break;
            }
        }
    }

    Ok(())
}

fn parse_cidr(cidr: &str) -> Result<(Ipv4Addr, u8)> {
    // format: 10.10.0.0/24
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid CIDR format");
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
