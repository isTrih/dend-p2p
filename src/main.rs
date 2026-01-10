mod config;
pub mod protocol;
mod server;
pub mod client;
pub mod cache;
mod client_webui;

use anyhow::Result;
use clap::Parser;
use log::{info, error};
use std::env;

#[derive(Parser, Debug)]
#[command(author = "HUA Haohui", version, about = "P2P 直连组网工具", long_about = None)]
struct Args {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    // Windows: 检查 wintun.dll
    #[cfg(target_os = "windows")]
    {
        if !std::path::Path::new("wintun.dll").exists() {
            error!("错误: 未找到 wintun.dll!");
            error!("请从 https://www.wintun.net/ 下载并将其放在此程序同一目录下。");
            return Ok(());
        }
    }

    let args = Args::parse();
    info!("dend-p2p v{} - P2P 直连组网工具", env!("CARGO_PKG_VERSION"));
    info!("正在加载配置: {}", args.config);

    let config = match config::Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            error!("加载配置失败: {}", e);
            return Ok(());
        }
    };

    match config.mode {
        config::Mode::Server => {
            info!("【服务器模式】启动中...");
            server::run(config).await?;
        }
        config::Mode::Client => {
            info!("【客户端模式】启动中...");
            client::run(config).await?;
        }
    }

    Ok(())
}
