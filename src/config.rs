use serde::Deserialize;
use std::fs;
use anyhow::Result;

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Server,
    Client,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub mode: Mode,
    pub server_addr: String,
    pub token: String,
    pub tun_name: Option<String>,

    // Server specific
    pub cidr: Option<String>,

    // Client specific
    #[serde(default = "default_webui")]
    pub webui: bool,
}

fn default_webui() -> bool {
    // Linux 默认关闭 WebUI，macOS/Windows 默认开启
    #[cfg(target_os = "linux")]
    {
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }
}
