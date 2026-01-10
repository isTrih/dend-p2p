// API 服务
const apiService = {
    async fetchLocalDevice() {
        try {
            const response = await fetch('/api/device');
            return response.ok ? await response.json() : null;
        } catch (e) {
            console.error('获取设备信息失败:', e);
            return null;
        }
    },

    async fetchPeerStatus() {
        try {
            const response = await fetch('/api/peerStatus');
            return response.ok ? await response.json() : null;
        } catch (e) {
            console.error('获取连接状态失败:', e);
            return null;
        }
    },

    async connectPeer(targetId) {
        try {
            const response = await fetch('/api/connect', {
                method: 'POST',
                headers: {'Content-Type': 'application/json'},
                body: JSON.stringify({ targetId })
            });
            return await response.json();
        } catch (e) {
            console.error('连接失败:', e);
            return { code: -1, message: '连接失败，请检查网络' };
        }
    }
};

// Vue 应用
const {createApp} = Vue;

createApp({
    data() {
        return {
            localDevice: {
                clientId: '加载中...',
                IP: '0.0.0.0',
                natType: '未知'
            },
            peerDevice: {
                IP: '0.0.0.0',
                latency: '-',
                alive: false
            },
            connectionInfo: {
                mode: '断开状态',
                modeCode: 2,
                statusText: '未连接'
            },
            connectionStatus: {
                online: false,
                message: '未连接'
            },
            targetId: '',
            isConnecting: false,
            copyStatus: {
                id: '点击复制',
                pwd: '点击复制',
                peerIP: '点击复制'
            },
            peerStatusInterval: null,
            localAddr: '',
            serverAddr: '',
            statusText: '未连接'
        };
    },

    computed: {
        statusClass() {
            if (this.connectionStatus.online) return 'status-online';
            if (this.isConnecting) return 'status-connecting';
            return 'status-offline';
        }
    },

    methods: {
        async fetchLocalDevice() {
            const device = await apiService.fetchLocalDevice();
            if (device) {
                this.localDevice = device;
            }
        },

        async fetchPeerStatus() {
            if (this.peerStatusInterval) {
                clearInterval(this.peerStatusInterval);
            }

            const req = async () => {
                try {
                    const data = await apiService.fetchPeerStatus();
                    if (data) {
                        this.peerDevice = data.device;
                        this.connectionInfo = data.status;
                        this.updateConnectionStatus();
                    }
                } catch (e) {
                    console.error(e);
                }
            };

            this.peerStatusInterval = setInterval(req, 1000);
            req();
        },

        updateConnectionStatus() {
            if (this.connectionInfo.modeCode === 0) {
                this.connectionStatus.message = 'P2P 直连成功';
                this.connectionStatus.online = true;
                this.isConnecting = false;
                this.statusText = '已连接 (P2P)';
            } else if (this.connectionInfo.modeCode === 1) {
                this.connectionStatus.message = '服务器中转';
                this.connectionStatus.online = true;
                this.isConnecting = false;
                this.statusText = '已连接 (中转)';
            } else if (this.isConnecting) {
                this.connectionStatus.message = this.connectionInfo.statusText || '正在连接...';
                this.connectionStatus.online = false;
                this.statusText = '连接中...';
            } else {
                this.connectionStatus.message = '未连接';
                this.connectionStatus.online = false;
                this.isConnecting = false;
                this.statusText = '未连接';
            }
        },

        async connectPeer() {
            if (!this.targetId || this.targetId.length < 8) {
                alert('请输入正确的连接码');
                return;
            }

            this.isConnecting = true;
            this.connectionStatus.message = '正在连接...';
            this.statusText = '连接中...';

            try {
                const result = await apiService.connectPeer(this.targetId);
                if (result.code === 0) {
                    this.connectionStatus.message = '正在建立 P2P 连接...';
                } else {
                    this.connectionStatus.message = result.message || '连接失败';
                    this.isConnecting = false;
                    this.statusText = '连接失败';
                }
            } catch (e) {
                console.error(e);
                this.connectionStatus.message = '连接失败';
                this.isConnecting = false;
                this.statusText = '连接失败';
            }
        },

        async copyClientId() {
            if (this.localDevice.clientId && this.localDevice.clientId !== '加载中...') {
                try {
                    await navigator.clipboard.writeText(this.localDevice.clientId);
                    this.copyStatus.id = '已复制!';
                    setTimeout(() => { this.copyStatus.id = '点击复制'; }, 2000);
                } catch (e) {
                    this.copyStatus.id = '复制失败';
                }
            }
        },

        async copyPeerIP() {
            if (this.peerDevice.IP && this.peerDevice.IP !== '0.0.0.0') {
                try {
                    await navigator.clipboard.writeText(this.peerDevice.IP);
                    this.copyStatus.peerIP = '已复制!';
                    setTimeout(() => { this.copyStatus.peerIP = '点击复制'; }, 2000);
                } catch (e) {
                    this.copyStatus.peerIP = '复制失败';
                }
            }
        }
    },

    mounted() {
        this.fetchLocalDevice();
        this.fetchPeerStatus();
    },

    unmounted() {
        if (this.peerStatusInterval) {
            clearInterval(this.peerStatusInterval);
        }
    }
}).mount('#app');
