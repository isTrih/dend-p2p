# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
cargo build --release  # Compiles to target/release/dend-p2p (or dend-p2p.exe on Windows)
```

## Project Overview

dend-p2p v0.2.2 is a P2P VPN tool that establishes peer-to-peer connections between clients via a central server. It creates a virtual network interface (TUN) on each node, enabling IP-layer communication between peers.

Key improvements in v0.2.2:
- Enhanced P2P hole punching with concurrent port scanning
- ChangePort mechanism for symmetric NAT traversal
- Simultaneous punch initiation from both peers
- Built-in WebUI for client mode (port 8080)

## Architecture

### Core Components

- **src/main.rs**: Entry point; parses CLI args, loads config, dispatches to server or client mode
- **src/server/mod.rs**: Server implementation handling:
  - Client authentication via token
  - Virtual IP allocation (CIDR pool, .1 is server, .2-.253 for clients)
  - Packet relay when P2P is unavailable
  - NAT session management for coordinated hole punching
  - ChangePort coordination for both peers before punch
  - Simultaneous ConnectPeer notification to both sides
  - 60-second timeout cleanup for inactive clients
- **src/client/mod.rs**: Client implementation with:
  - Handshake with server to obtain virtual IP
  - Connection mode management (Direct/Relay/Disconnected)
  - P2P hole punching with concurrent port scanning (max 2 concurrent)
  - Port range scanning for symmetric NATs
  - Bidirectional data loops: TUN ↔ UDP socket
  - Keepalive/Ping for NAT keep-alive and RTT measurement
- **src/protocol.rs**: Packet enum with new message types:
  - RequestConnect, ConnectPeer, ChangePort, PortChanged
  - EnableRelay, RelayEnabled, RelayLatencyTest
  - Beat, BeatAck for advanced NAT traversal
- **src/client_webui.rs**: WebUI server (client mode only):
  - HTTP API for device info and connection status
  - Built-in HTML/CSS/JS frontend
  - Auto-opens browser on startup at http://127.0.0.1:8080
- **src/config.rs**: TOML config loading (Mode::Server/Client, CIDR, token, tun_name)
- **src/cache.rs**: Persistent client ID storage in `dend-p2p-cache.json`

### Data Flow

1. **Server Mode**: TUN reads → packet relay to clients; UDP receives → write to TUN or relay to peer
2. **Client Mode**: TUN reads → send to server (or P2P peer if established); UDP receives → write to TUN
3. **P2P Establishment (v0.2.2 enhanced)**:
   - Client A requests connection to Client B
   - Server creates NAT session and notifies both to change port
   - Both clients change UDP port
   - Both clients report PortChanged to server
   - Server sends ConnectPeer to both with each other's real address
   - Both clients simultaneously punch to each other
   - Punch acknowledgment switches to P2P mode
   - Fallback to relay mode after 10s timeout if punch fails

### Key Implementation Details

- **Serialization**: bincode for Packet structs over UDP
- **Async**: tokio for all I/O (TUN split into reader/writer, shared UDP socket across tasks)
- **TUN Platform Differences**: macOS requires `utunX` naming; Linux enables packet information; Windows uses wintun.dll
- **NAT Traversal (v0.2.2)**:
  - ChangePort: both peers change UDP port before punching
  - Simultaneous punch: both peers punch at the same time
  - Concurrent scanning: up to 2 concurrent punch attempts
  - Port range: scans ±6 ports from base
  - Relay fallback: automatic switch to relay after timeout

## Running

Requires administrator/sudo privileges for TUN device access:

```bash
# Server mode
sudo ./target/release/dend-p2p --config server.toml

# Client mode (with WebUI)
sudo ./target/release/dend-p2p --config client.toml
# WebUI opens automatically at http://127.0.0.1:8080
```

On Windows, place `wintun.dll` alongside the executable.

## WebUI API

The client mode includes a built-in WebUI at port 8080:

- `GET /` - Main UI HTML
- `POST /api/device` - Get local device info
- `POST /api/peerStatus` - Get connection status
- `POST /api/connect` - Connect to peer by ID
