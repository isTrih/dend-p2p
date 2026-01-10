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
    _last_seen: std::time::Instant,
}

pub async fn run(config: Config) -> Result<()> {
    // 监听地址
    info!("Starting server on {}", config.server_addr);
    
    // Bind UDP socket
    let socket = Arc::new(UdpSocket::bind(&config.server_addr).await?);
    
    // 检查是否无人连接，如果有连接才开始 active
    // 当前为事件驱动模型：如果没有packet收到，process是sleep的，已经是低功耗模式
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
    let mut current_ip_idx = 2; // Start allocating from .2
    
    // Setup TUN device for server (to participate in the network)
    let mut tun_config = tun::Configuration::default();
    tun_config
        .address(server_ip)
        .netmask(Ipv4Addr::new(255, 255, 255, 0))
        .up();
    
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
    
    // Loop 1: Handle UDP packets (Client -> Server)
    let socket_recv = socket.clone();
    let peer_map_recv = peer_map.clone();
    let config_token = config.token.clone();
    
    let ip_allocator = Arc::new(Mutex::new(move || -> Ipv4Addr {
        let ip = next_ip(network_ip, current_ip_idx);
        current_ip_idx += 1;
        ip
    }));

    // Start UDP receive task
    let socket_send_ref = socket.clone();
    
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
                                Packet::Handshake { token } => {
                                    if token == config_token {
                                        let mut peers = peer_map_recv.lock().await;
                                        // Simple Logic: Alloc new IP every time or check if exists?
                                        // For simplicity, just alloc new.
                                        let mut alloc = ip_allocator.lock().await;
                                        let new_ip = alloc();
                                        
                                        info!("Handshake from {}, assigning {}", addr, new_ip);
                                        peers.insert(new_ip, ClientInfo {
                                            addr,
                                            _last_seen: std::time::Instant::now(),
                                        });

                                        // Relay mode naturally supports all NAT types (Cone, Symmetric, etc)
                                        // since server relays traffic and responds to the source port of the client.

                                        let resp = Packet::HandshakeAck {
                                            ip: new_ip.to_string(),
                                            cidr: cidr_str.clone(),
                                        };
                                        if let Ok(resp_bytes) = bincode::serialize(&resp) {
                                            let _ = socket_recv.send_to(&resp_bytes, addr).await;
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
                                            let peers = peer_map_recv.lock().await;
                                            if let Some(target) = peers.get(&dst) {
                                                let _ = socket_recv.send_to(packet_data, target.addr).await;
                                            } else {
                                                // Unknown dest, maybe drop
                                            }
                                        }
                                        
                                        // Update sender last seen? We don't know who sent this IP packet easily
                                        // without looking at the outer map, but we trust the sender addr?
                                        // This naive impl allows spoofing.
                                        // In real world, we should check Source IP inside packet matches the expected Addr.
                                    }
                                }
                                Packet::Keepalive => {
                                    // Find who sent this and update last_seen?
                                    // We'd need to reverse lookup addr to IP or just store addr->last_seen
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
    let socket_tun = socket_send_ref;
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
