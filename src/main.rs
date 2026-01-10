mod config;
mod protocol;
mod server;
mod client;
mod cache;

use anyhow::Result;
use clap::Parser;
use log::{info, error};
use std::env;

#[derive(Parser, Debug)]
#[command(author = "HUA Haohui", version, about = "A P2P connection tool", long_about = None)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    // Windows: Check wintun.dll
    #[cfg(target_os = "windows")]
    {
        if !std::path::Path::new("wintun.dll").exists() {
            error!("错误: 未找到 wintun.dll!");
            error!("请从 https://www.wintun.net/ 下载并将其放在此程序同一目录下。");
            error!("Error: wintun.dll not found! Please download from https://www.wintun.net/ and place it here.");
            // 可以在此添加自动下载逻辑，但为了保持简洁，仅提示用户。
        }
    }

    let args = Args::parse();
    info!("Loading config from {}", args.config);

    let config = match config::Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load config: {}", e);
            return Ok(());
        }
    };

    match config.mode {
        config::Mode::Server => {
            info!("Starting in SERVER mode");
            server::run(config).await?;
        }
        config::Mode::Client => {
            info!("Starting in CLIENT mode");
            client::run(config).await?;
        }
    }

    Ok(())
}
