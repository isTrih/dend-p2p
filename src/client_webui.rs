use std::net::SocketAddr;
use axum::{
    routing::{get, post},
    Router,
    Json,
    response::Html,
};
use tower_http::services::ServeDir;
use serde::Deserialize;
use serde_json::json;
use log::info;
use std::sync::Mutex;
use once_cell::sync::Lazy;

// 全局状态
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
}

#[derive(Debug, Clone, Default)]
pub struct LocalDeviceInfo {
    pub client_id: String,
    pub vip: String,
}

pub fn set_local_info(client_id: &str, vip: &str) {
    let mut info = LOCAL_INFO.lock().unwrap();
    info.client_id = client_id.to_string();
    info.vip = vip.to_string();
}

pub fn set_connection_status(status: ConnectionStatus) {
    let mut s = CONNECTION_STATUS.lock().unwrap();
    *s = status;
}

// ==================== HTTP 处理函数 ====================

async fn get_device() -> Json<serde_json::Value> {
    let info = LOCAL_INFO.lock().unwrap();

    Json(json!({
        "clientId": info.client_id,
        "IP": info.vip,
        "natType": "未知"
    }))
}

async fn get_peer_status() -> Json<serde_json::Value> {
    let status = CONNECTION_STATUS.lock().unwrap().clone();

    Json(json!({
        "device": {
            "clientId": "",
            "IP": status.peer_ip,
            "alive": status.is_connected,
            "latency": status.peer_latency
        },
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

#[derive(Deserialize)]
struct ConnectRequest {
    target_id: String,
}

async fn connect_peer(Json(request): Json<ConnectRequest>) -> Json<serde_json::Value> {
    if request.target_id.is_empty() || request.target_id.len() < 8 {
        return Json(json!({
            "code": -1,
            "message": "连接码无效，请输入正确的连接码"
        }));
    }

    // 更新连接状态
    let mut status = CONNECTION_STATUS.lock().unwrap();
    status.is_connecting = true;
    status.connect_message = "正在连接...".to_string();
    status.status_text = "正在连接...".to_string();
    status.connect_failed = false;

    Json(json!({
        "code": 0,
        "message": "正在建立 P2P 连接..."
    }))
}

async fn index() -> Html<&'static str> {
    Html(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>dend-p2p · P2P 连接工具</title>
    <link rel="stylesheet" href="/static/css/style.css">
    <script src="/static/js/vue.global.prod.js"></script>
</head>
<body>
<div id="app">
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
                    </div>
                </div>
            </div>
            <div class="card">
                <div class="card-header">
                    <svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
                        <path d="M16 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2"></path>
                        <circle cx="8.5" cy="7" r="4"></circle>
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
        </div>
    </div>
</div>
<script>
async function pollStatus() {
    try {
        const response = await fetch('/api/peerStatus', { method: 'POST' });
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
        }

        const deviceResponse = await fetch('/api/device', { method: 'POST' });
        const deviceData = await deviceResponse.json();
        document.getElementById('clientId').textContent = deviceData.clientId;
        document.getElementById('localIp').textContent = deviceData.IP + '/24';
    } catch (e) {
        console.error('获取状态失败:', e);
    }
}

async function connectPeer() {
    const targetId = document.getElementById('targetId').value.trim();
    if (!targetId || targetId.length < 8) {
        alert('请输入正确的连接码');
        return;
    }

    try {
        const response = await fetch('/api/connect', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ target_id: targetId })
        });
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

pub async fn start_web_server(port: u16) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let app = Router::new()
        .route("/", get(index))
        .route("/api/device", post(get_device))
        .route("/api/peerStatus", post(get_peer_status))
        .route("/api/connect", post(connect_peer))
        .nest_service("/static", ServeDir::new("src/client/static"));

    info!("========================================");
    info!("  WebUI 控制面板已启动!");
    info!("  访问地址: http://127.0.0.1:{}", port);
    info!("========================================");

    open_browser(format!("http://127.0.0.1:{}", port));

    if let Err(e) = axum::serve(
        tokio::net::TcpListener::bind(&addr).await.unwrap(),
        app
    ).await {
        log::error!("Web 服务器错误: {}", e);
    }
}

fn open_browser(url: String) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(&["/c", "start", &url])
            .spawn();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg(&url)
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn();
    }

    info!("正在尝试自动打开浏览器...");
}
