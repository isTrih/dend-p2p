use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use axum::{
    routing::{get, post},
    Router,
    Json,
    response::Html,
    extract::Query,
};
use serde::Deserialize;
use serde_json::json;
use log::info;
use std::sync::Mutex;
use once_cell::sync::Lazy;

// ==================== 请求结构体 ====================

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub target_id: String,
}

// ==================== 全局状态 ====================

/// WebUI 服务器端口（原子变量，支持自动探测）
static WEBUI_PORT: AtomicU16 = AtomicU16::new(8080);

/// 浏览器是否已打开
static BROWSER_OPENED: AtomicBool = AtomicBool::new(false);

/// P2P 连接触发器（用于 WEBUI 点击连接后触发打洞）
static P2P_TRIGGER: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

static CONNECTION_STATUS: Lazy<Mutex<ConnectionStatus>> = Lazy::new(|| Mutex::new(ConnectionStatus::default()));
static LOCAL_INFO: Lazy<Mutex<LocalDeviceInfo>> = Lazy::new(|| Mutex::new(LocalDeviceInfo::default()));

#[derive(Debug, Clone, Default)]
pub struct ConnectionStatus {
    pub mode: String,
    pub mode_code: u8,
    pub is_connected: bool,
    pub status_text: String,
    pub is_connecting: bool,
    pub connect_failed: bool,
    pub connect_message: String,
    pub peer_ip: String,
    pub peer_latency: i32,
    pub online_devices: Vec<OnlineDevice>,
}

#[derive(Debug, Clone, Default)]
pub struct OnlineDevice {
    pub client_id: String,
    pub vip: String,
    pub is_online: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LocalDeviceInfo {
    pub client_id: String,
    pub vip: String,
    pub nat_type: String,
}

pub fn set_local_info(client_id: &str, vip: &str, nat_type: &str) {
    let mut info = LOCAL_INFO.lock().unwrap();
    info.client_id = client_id.to_string();
    info.vip = vip.to_string();
    info.nat_type = nat_type.to_string();
}

pub fn set_connection_status(status: ConnectionStatus) {
    let mut s = CONNECTION_STATUS.lock().unwrap();
    *s = status;
}

/// 更新在线设备列表（供客户端模块调用）
pub fn update_online_devices(devices: Vec<OnlineDevice>) {
    let mut s = CONNECTION_STATUS.lock().unwrap();
    s.online_devices = devices;
}

/// 设置 P2P 连接触发器（供 WEBUI API 调用）
pub fn set_p2p_trigger(target_id: String) {
    let mut trigger = P2P_TRIGGER.lock().unwrap();
    *trigger = Some(target_id);
}

/// 获取 P2P 连接触发器（供客户端模块调用）
/// 返回 target_id 并清除触发器
pub fn get_p2p_trigger() -> Option<String> {
    let mut trigger = P2P_TRIGGER.lock().unwrap();
    trigger.take()
}

/// 获取当前 WebUI 端口
pub fn get_webui_port() -> u16 {
    WEBUI_PORT.load(Ordering::Relaxed)
}

// ==================== HTTP 处理函数 ====================

async fn get_device() -> Json<serde_json::Value> {
    let info = LOCAL_INFO.lock().unwrap();

    Json(json!({
        "clientId": info.client_id,
        "IP": info.vip,
        "natType": info.nat_type,
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn get_peer_status() -> Json<serde_json::Value> {
    let status = CONNECTION_STATUS.lock().unwrap().clone();
    let local_info = LOCAL_INFO.lock().unwrap().clone();

    let devices: Vec<_> = status.online_devices.iter()
        .filter(|d| d.client_id != local_info.client_id)
        .map(|d| json!({
            "clientId": d.client_id,
            "IP": d.vip,
            "alive": d.is_online
        }))
        .collect();

    Json(json!({
        "device": {
            "clientId": "",
            "IP": status.peer_ip,
            "alive": status.is_connected,
            "latency": status.peer_latency
        },
        "devices": devices,
        "status": {
            "mode": status.mode,
            "modeCode": status.mode_code,
            "isConnected": status.is_connected,
            "isConnecting": status.is_connecting,
            "connectFailed": status.connect_failed,
            "connectMessage": status.connect_message,
            "statusText": status.status_text
        }
    }))
}

// 连接处理函数 - 在start_web_server内定义闭包
async fn index() -> Html<&'static str> {
    Html(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>dend-p2p · P2P 连接工具</title>
    <script src="https://unpkg.com/vue@3/dist/vue.global.prod.js"></script>
    <style>
:root {
    --primary: #6366f1;
    --primary-hover: #4f46e5;
    --success: #22c55e;
    --warning: #f59e0b;
    --danger: #ef4444;
    --bg: #0f172a;
    --card-bg: #1e293b;
    --border: #334155;
    --text: #f1f5f9;
    --text-secondary: #94a3b8;
}
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; background: linear-gradient(135deg, var(--bg) 0%, #1e1b4b 100%); min-height: 100vh; color: var(--text); }
#app { min-height: 100vh; }
.dashboard { max-width: 1200px; margin: 0 auto; padding: 20px; }
.header { display: flex; align-items: center; gap: 16px; padding: 24px; margin-bottom: 24px; background: rgba(30, 41, 59, 0.8); border-radius: 16px; border: 1px solid var(--border); }
.logo { width: 56px; height: 56px; background: linear-gradient(135deg, var(--primary) 0%, #8b5cf6 100%); border-radius: 12px; display: flex; align-items: center; justify-content: center; font-size: 24px; font-weight: bold; color: white; }
.header-content h1 { font-size: 24px; font-weight: 600; margin-bottom: 4px; }
.product-version { color: var(--text-secondary); font-size: 14px; }
.main-content { display: grid; grid-template-columns: 1fr 1fr; gap: 24px; margin-bottom: 24px; }
@media (max-width: 768px) { .main-content { grid-template-columns: 1fr; } }
.card { background: rgba(30, 41, 59, 0.8); border-radius: 16px; border: 1px solid var(--border); overflow: hidden; }
.card-header { display: flex; align-items: center; gap: 12px; padding: 16px 20px; background: rgba(51, 65, 85, 0.5); border-bottom: 1px solid var(--border); font-weight: 600; font-size: 16px; }
.card-header svg { color: var(--primary); }
.card-body { padding: 20px; }
.credentials { margin-bottom: 20px; }
.credential-item { margin-bottom: 16px; }
.credential-label { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; font-size: 14px; color: var(--text-secondary); }
.credential-value { font-size: 28px; font-weight: 600; font-family: 'SF Mono', Monaco, monospace; letter-spacing: 2px; color: var(--primary); }
.info-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
.info-item { padding: 12px; background: rgba(51, 65, 85, 0.5); border-radius: 8px; }
.info-label { font-size: 12px; color: var(--text-secondary); margin-bottom: 4px; }
.info-value { font-size: 14px; font-weight: 500; }
.status-card { background: rgba(51, 65, 85, 0.5); border-radius: 12px; padding: 16px; margin-bottom: 20px; }
.status-header { display: flex; align-items: center; gap: 8px; font-size: 16px; font-weight: 600; margin-bottom: 12px; }
.status-dot { width: 10px; height: 10px; border-radius: 50%; animation: pulse 2s infinite; }
@keyframes pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.5; } }
.status-item { display: flex; justify-content: space-between; padding: 8px 0; border-bottom: 1px solid rgba(51, 65, 85, 0.5); }
.status-item:last-child { border-bottom: none; }
.status-label { color: var(--text-secondary); }
.status-value { display: flex; align-items: center; gap: 8px; font-weight: 500; }
.connection-form { margin-top: 16px; }
.input-group { margin-bottom: 16px; }
.input-label { display: block; margin-bottom: 8px; font-size: 14px; color: var(--text-secondary); }
.input-required { color: var(--danger); margin-left: 4px; }
.connection-input { width: 100%; padding: 12px 16px; background: rgba(15, 23, 42, 0.8); border: 1px solid var(--border); border-radius: 8px; color: var(--text); font-size: 16px; transition: border-color 0.2s; }
.connection-input:focus { outline: none; border-color: var(--primary); }
.connection-input::placeholder { color: var(--text-secondary); }
.connect-btn { width: 100%; padding: 14px 24px; background: linear-gradient(135deg, var(--primary) 0%, #8b5cf6 100%); border: none; border-radius: 8px; color: white; font-size: 16px; font-weight: 600; cursor: pointer; transition: all 0.2s; }
.connect-btn:hover:not(.disabled) { transform: translateY(-1px); box-shadow: 0 4px 12px rgba(99, 102, 241, 0.4); }
.connect-btn.disabled { opacity: 0.6; cursor: not-allowed; }
.status-bar { display: flex; gap: 24px; padding: 12px 20px; background: rgba(30, 41, 59, 0.8); border-radius: 8px; font-size: 13px; color: var(--text-secondary); }
.status-item { display: flex; align-items: center; gap: 4px; }
.status-online { color: var(--success); }
.status-connecting { color: var(--warning); }
.status-offline { color: var(--danger); }
.device-list { max-height: 200px; overflow-y: auto; }
.device-item { display: flex; justify-content: space-between; align-items: center; padding: 12px; background: rgba(51, 65, 85, 0.3); border-radius: 8px; margin-bottom: 8px; cursor: pointer; transition: all 0.2s; }
.device-item:hover { background: rgba(51, 65, 85, 0.6); }
.device-item.empty { justify-content: center; color: var(--text-secondary); cursor: default; }
.device-item.empty:hover { background: rgba(51, 65, 85, 0.3); }
.device-info { display: flex; flex-direction: column; gap: 4px; }
.device-id { font-weight: 500; font-size: 14px; }
.device-ip { font-size: 12px; color: var(--text-secondary); font-family: 'SF Mono', Monaco, monospace; }
.device-status { padding: 4px 10px; border-radius: 12px; font-size: 12px; font-weight: 500; }
.device-status.online { background: rgba(34, 197, 94, 0.2); color: var(--success); }
.device-status.offline { background: rgba(239, 68, 68, 0.2); color: var(--danger); }
.version-badge { position: fixed; top: 10px; right: 10px; background: var(--primary); color: white; padding: 4px 12px; border-radius: 12px; font-size: 12px; opacity: 0.8; }
.nat-badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 12px; margin-left: 8px; }
.nat-1 { background: #10b981; color: white; }
.nat-2 { background: #3b82f6; color: white; }
.nat-3 { background: #f59e0b; color: white; }
.nat-4 { background: #ef4444; color: white; }
.nat-unknown { background: #6b7280; color: white; }
    </style>
</head>
<body>
<div id="app">
    <div class="version-badge">v<span id="appVersion">0.3.0</span></div>
    <div class="dashboard">
        <div class="header">
            <div class="logo">De</div>
            <div class="header-content">
                <h1>dend-p2p · P2P 直连组网</h1>
                <div class="product-version">一键组网 · 远程办公 · 游戏串流</div>
            </div>
        </div>
        <div class="main-content">
            <div class="card">
                <div class="card-header">
                    <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <rect x="2" y="3" width="20" height="14" rx="2"></rect>
                        <line x1="8" y1="21" x2="16" y2="21"></line>
                        <line x1="12" y1="17" x2="12" y2="21"></line>
                    </svg>
                    我的设备
                </div>
                <div class="card-body">
                    <div class="credentials">
                        <div class="credential-item">
                            <div class="credential-label">我的连接码</div>
                            <div class="credential-value" id="clientId">加载中...</div>
                        </div>
                    </div>
                    <div class="info-grid">
                        <div class="info-item">
                            <div class="info-label">虚拟IP</div>
                            <div class="info-value" id="localIp">0.0.0.0/24</div>
                        </div>
                        <div class="info-item">
                            <div class="info-label">NAT类型</div>
                            <div class="info-value"><span id="natType">检测中...</span></div>
                        </div>
                    </div>
                </div>
            </div>
            <div class="card">
                <div class="card-header">
                    <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                        <circle cx="9" cy="7" r="4"></circle>
                        <path d="M23 21v-2a4 4 0 0 0-3-3.87"></path>
                        <path d="M16 3.13a4 4 0 0 1 0 7.75"></path>
                    </svg>
                    在线设备
                </div>
                <div class="card-body">
                    <div id="onlineDevices" class="device-list">
                        <div class="device-item empty">暂无可用设备</div>
                    </div>
                </div>
            </div>
            <div class="card">
                <div class="card-header">
                    <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                        <circle cx="8.5" cy="7" r="4"></circle>
                        <line x1="20" y1="8" x2="20" y2="14"></line>
                        <line x1="23" y1="11" x2="17" y2="11"></line>
                    </svg>
                    连接远程设备
                </div>
                <div class="card-body">
                    <div id="connectionStatus" style="display:none;">
                        <div class="status-card">
                            <div class="status-header">
                                <div class="status-dot" id="statusDot"></div>
                                <span id="statusText">连接成功</span>
                            </div>
                            <div class="status-item">
                                <span class="status-label">模式</span>
                                <span class="status-value" id="connectionMode">-</span>
                            </div>
                            <div class="status-item">
                                <span class="status-label">延迟</span>
                                <span class="status-value" id="latency">-</span>
                            </div>
                        </div>
                    </div>
                    <div class="connection-form">
                        <div class="input-group">
                            <label class="input-label">对方连接码 <span class="input-required">*</span></label>
                            <input type="text" id="targetId" class="connection-input" placeholder="输入连接码" maxlength="36">
                        </div>
                        <button class="connect-btn" id="connectBtn" onclick="connectPeer()">开始连接</button>
                    </div>
                </div>
            </div>
        </div>
        <div class="status-bar">
            <span class="status-item">状态: <span id="mainStatus">未连接</span></span>
            <span class="status-item">NAT类型: <span id="natStatus">-</span></span>
        </div>
    </div>
</div>
<script>
const NAT_TYPE_NAMES = {
    '1': 'NAT1 (完全锥形)',
    '2': 'NAT2 (地址限制锥形)',
    '3': 'NAT3 (端口限制锥形)',
    '4': 'NAT4 (对称型)',
    'unknown': 'NAT (未知)'
};

function getNatClass(natType) {
    if (natType === '1') return 'nat-1';
    if (natType === '2') return 'nat-2';
    if (natType === '3') return 'nat-3';
    if (natType === '4') return 'nat-4';
    return 'nat-unknown';
}

async function pollStatus() {
    // 获取连接状态
    try {
        const response = await fetch('/api/peerStatus', { method: 'POST' });
        if (response.ok) {
            const data = await response.json();
            if (data.status) {
                const status = data.status;
                const device = data.device;

                const statusDiv = document.getElementById('connectionStatus');
                const statusDot = document.getElementById('statusDot');
                const statusText = document.getElementById('statusText');
                const mainStatus = document.getElementById('mainStatus');
                const connectBtn = document.getElementById('connectBtn');

                if (status.isConnected) {
                    statusDiv.style.display = 'block';
                    statusDot.style.backgroundColor = 'var(--success)';
                    statusText.textContent = status.statusText || '已连接';
                    mainStatus.textContent = '已连接 (' + status.mode + ')';
                    mainStatus.className = 'status-online';
                    connectBtn.textContent = '已连接';
                    connectBtn.disabled = true;

                    document.getElementById('connectionMode').textContent = status.mode;
                    document.getElementById('latency').textContent = device.latency >= 0 ? device.latency + 'ms' : '-';
                } else if (status.isConnecting) {
                    statusDiv.style.display = 'block';
                    statusDot.style.backgroundColor = 'var(--warning)';
                    statusText.textContent = status.statusText || '连接中...';
                    mainStatus.textContent = '连接中...';
                    mainStatus.className = 'status-connecting';
                    connectBtn.textContent = '连接中...';
                    connectBtn.disabled = true;
                } else {
                    statusDiv.style.display = 'none';
                    mainStatus.textContent = '未连接';
                    mainStatus.className = 'status-offline';
                    connectBtn.textContent = '开始连接';
                    connectBtn.disabled = false;
                }

                // 更新在线设备列表
                if (data.devices && data.devices.length > 0) {
                    const devicesDiv = document.getElementById('onlineDevices');
                    let html = '';
                    data.devices.forEach(d => {
                        const statusClass = d.alive ? 'online' : 'offline';
                        const statusText = d.alive ? '在线' : '离线';
                        html += '<div class="device-item" onclick="setTargetId(\'' + d.clientId + '\')">';
                        html += '<div class="device-info">';
                        html += '<span class="device-id">' + d.clientId + '</span>';
                        html += '<span class="device-ip">' + d.IP + '</span>';
                        html += '</div>';
                        html += '<span class="device-status ' + statusClass + '">' + statusText + '</span>';
                        html += '</div>';
                    });
                    devicesDiv.innerHTML = html;
                }
            }
        }
    } catch (e) {
        console.error('获取状态失败:', e);
    }

    // 获取本地设备信息（独立处理）
    try {
        const deviceResponse = await fetch('/api/device', { method: 'POST' });
        if (deviceResponse.ok) {
            const deviceData = await deviceResponse.json();

            if (deviceData.clientId) {
                document.getElementById('clientId').textContent = deviceData.clientId;
            }
            if (deviceData.IP) {
                document.getElementById('localIp').textContent = deviceData.IP + '/24';
            }

            const natType = deviceData.natType || 'unknown';
            document.getElementById('natType').innerHTML = '<span class="nat-badge ' + getNatClass(natType) + '">' + (NAT_TYPE_NAMES[natType] || NAT_TYPE_NAMES['unknown']) + '</span>';
            document.getElementById('natStatus').textContent = NAT_TYPE_NAMES[natType] || NAT_TYPE_NAMES['unknown'];

            if (deviceData.version) {
                document.getElementById('appVersion').textContent = deviceData.version;
            }
        }
    } catch (e) {
        console.error('获取设备信息失败:', e);
    }
}

function setTargetId(id) {
    document.getElementById('targetId').value = id;
}

async function connectPeer() {
    const targetId = document.getElementById('targetId').value.trim();
    if (!targetId || targetId.length < 8) {
        alert('请输入正确的连接码');
        return;
    }

    try {
        // 使用 GET 请求，target_id 作为查询参数
        const response = await fetch('/api/connect?target_id=' + encodeURIComponent(targetId));
        const data = await response.json();

        if (data.code === 0) {
            document.getElementById('connectBtn').textContent = '连接中...';
            document.getElementById('connectBtn').disabled = true;
        } else {
            alert(data.message || '连接失败');
        }
    } catch (e) {
        alert('连接失败: ' + e.message);
    }
}

setInterval(pollStatus, 1000);
pollStatus();
</script>
</body>
</html>"#)
}

// ==================== 启动 Web 服务器 ====================

pub async fn start_web_server() -> u16 {
    // 尝试多个端口，找到可用的
    let mut port = 8080;
    let mut actual_port = port;

    for p in [8080, 8081, 8082, 8083, 8084, 8085, 9000, 9001] {
        if let Ok(socket) = tokio::net::TcpSocket::new_v4() {
            let addr = SocketAddr::from(([127, 0, 0, 1], p));
            if socket.bind(addr).is_ok() {
                if let Ok(listener) = socket.listen(1) {
                    drop(listener);
                    actual_port = p;
                    port = p;
                    break;
                }
            }
        }
    }

    WEBUI_PORT.store(port, Ordering::Relaxed);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // 创建连接处理器闭包
    let connect_handler = |Query(params): Query<ConnectRequest>| async move {
        let target_id = params.target_id;

        if target_id.is_empty() || target_id.len() < 8 {
            return Json(json!({
                "code": -1,
                "message": "连接码无效，请输入正确的连接码"
            }));
        }

        // 设置 P2P 触发器，客户端循环会检测并发起连接
        set_p2p_trigger(target_id.clone());

        // 更新连接状态
        let mut status = CONNECTION_STATUS.lock().unwrap();
        status.is_connecting = true;
        status.connect_message = "正在建立 P2P 连接...".to_string();
        status.status_text = "正在连接...".to_string();
        status.connect_failed = false;

        Json(json!({
            "code": 0,
            "message": "正在建立 P2P 连接..."
        }))
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/api/device", post(get_device))
        .route("/api/peerStatus", post(get_peer_status))
        .route("/api/connect", get(connect_handler));

    // 尝试打开浏览器
    let browser_url = format!("http://127.0.0.1:{}", port);
    open_browser(&browser_url);

    info!("========================================");
    info!("  WebUI 控制面板已启动!");
    info!("  访问地址: {}", browser_url);
    info!("========================================");

    if let Err(e) = axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app
    ).await {
        log::error!("Web 服务器错误: {}", e);
    }

    port
}

fn open_browser(url: &str) {
    // 防止重复打开
    if BROWSER_OPENED.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return;
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(&["/c", "start", url])
            .spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(url)
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .spawn();
    }

    info!("正在尝试自动打开浏览器...");
}
