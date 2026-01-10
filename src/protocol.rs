use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub enum Packet {
    Handshake { token: String },
    HandshakeAck { ip: String, cidr: String },
    Data(Vec<u8>), // Raw IP packet
    Keepalive,
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
