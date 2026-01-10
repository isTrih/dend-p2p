use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Packet {
    // 客户端 <-> 服务器
    Handshake { token: String, client_id: String },
    HandshakeAck { ip: String, cidr: String },
    Keepalive,

    // 客户端请求连接对等节点
    RequestConnect { target_id: String, password: String },

    // v0.2.2 P2P 扩展 - 服务器通知双方打洞
    ConnectPeer {
        peer_vip: Ipv4Addr,
        peer_addr: Ipv4Addr,
        port: u16,
        peer_id: String,
    },

    // 服务器通知客户端更换端口
    ChangePort { password: String },

    // 客户端端口更换完毕回调
    PortChanged { client_id: String },

    // 服务器通知双方启用中转
    EnableRelay { src_id: String, target_id: String, vip: String },
    RelayEnabled { peer_id: String, peer_vip: String },

    // 服务器通知断开连接
    DisconnectPeer,

    // 中转模式相关
    RelayLatencyTest { target_id: String, timestamp: u64 },
    RelayLatencyReply { target_id: String, timestamp: u64 },

    // 数据传输
    Data(Vec<u8>), // Raw IP packet
    KeepalivePacket,

    // P2P 打洞
    PeerInfo { peer_vip: String, peer_addr: String },
    Punch { vip: String },
    Beat {
        use_port: u16,
        vip: String,
        c: u32,
        t: i64,
        a: u32,
        id: String,
    },
    BeatAck { t: i64 },

    // 延迟测量
    Ping(u64), // timestamp (millis)
    Pong(u64), // echo timestamp

    Error(String),
}

// 简单的 IPv4 Header 解析辅助 (只为了拿 dst ip)
// 假设数据包至少有 20 字节
pub fn get_dest_ip(packet: &[u8]) -> Option<std::net::Ipv4Addr> {
    if packet.len() < 20 {
        return None;
    }
    // IPv4 header destination address is at bytes 16..20
    let ip = std::net::Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
    Some(ip)
}
