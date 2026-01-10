use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use anyhow::Result;
use uuid::Uuid;
use log::{info, warn};

#[derive(Serialize, Deserialize, Debug)]
pub struct ClientCache {
    pub client_id: String,
}

impl ClientCache {
    pub fn load_or_create() -> Self {
        let path = "dend-p2p-cache.json";
        if Path::new(path).exists() {
            match fs::read_to_string(path) {
                Ok(content) => match serde_json::from_str::<ClientCache>(&content) {
                    Ok(cache) => {
                        info!("Loaded client ID from cache: {}", cache.client_id);
                        return cache;
                    }
                    Err(e) => warn!("Failed to parse cache: {}", e),
                },
                Err(e) => warn!("Failed to read cache file: {}", e),
            }
        }

        // Create new
        let new_id = Uuid::new_v4().to_string();
        let cache = ClientCache { client_id: new_id };
        
        if let Ok(content) = serde_json::to_string_pretty(&cache) {
            if let Err(e) = fs::write(path, content) {
                warn!("Failed to save cache file: {}", e);
            } else {
                info!("Generated and saved new client ID: {}", cache.client_id);
            }
        }
        
        cache
    }
}
