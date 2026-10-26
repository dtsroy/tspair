//! Configuration loading / first-run template creation.
//!
//! Lives in the per-user config dir (v1 kept it in the project root, which
//! doesn't work for a packaged GUI app). The peer is never stored here — it
//! is detected fresh from `tailscale status` on every discovery cycle.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::Manager;

pub const DEFAULT_PORT: u16 = 52520;
const CONFIG_NAME: &str = "config.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// "server" (listens) or "client" (dials) — exactly one of each per pair
    pub role: String,
    pub port: u16,
    /// random UUID, used for simultaneous-request tie-breaking
    pub device_id: String,
    pub ping_interval: f64,
    pub status_interval: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            role: "client".into(),
            port: DEFAULT_PORT,
            device_id: String::new(),
            ping_interval: 15.0,
            status_interval: 30.0,
        }
    }
}

impl Config {
    pub fn role_valid(&self) -> bool {
        matches!(self.role.as_str(), "server" | "client")
    }
}

/// Where the config lives. `TSPAIR_CONFIG` overrides (v1 compat, handy for dev).
pub fn config_path(app: &tauri::AppHandle) -> PathBuf {
    if let Some(env) = std::env::var_os("TSPAIR_CONFIG") {
        return PathBuf::from(env);
    }
    app.path()
        .app_config_dir()
        .map(|dir| dir.join(CONFIG_NAME))
        .unwrap_or_else(|_| PathBuf::from(CONFIG_NAME))
}

/// Load the config; on first run, write a template with a freshly generated
/// (and persisted!) device_id. Returns `(config, created_now)`.
pub fn load_or_create(path: &Path) -> Result<(Config, bool), String> {
    if !path.exists() {
        let mut cfg = Config::default();
        cfg.device_id = uuid::Uuid::new_v4().to_string();
        save(path, &cfg)?;
        return Ok((cfg, true));
    }

    let raw: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(path).map_err(|e| format!("could not read {}: {e}", path.display()))?,
    )
    .map_err(|e| format!("could not parse {}: {e}", path.display()))?;

    let mut cfg = Config::default();
    cfg.role = raw
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("client")
        .to_lowercase();
    if !cfg.role_valid() {
        return Err(format!(
            "Invalid role {:?} in {}: must be \"server\" or \"client\".",
            cfg.role,
            path.display()
        ));
    }
    if let Some(p) = raw.get("port").and_then(|v| v.as_u64()) {
        cfg.port = p as u16;
    }
    cfg.device_id = raw
        .get("device_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if cfg.device_id.is_empty() {
        // v2 fix: v1 generated a UUID but never persisted it back.
        cfg.device_id = uuid::Uuid::new_v4().to_string();
        save(path, &cfg)?;
    }
    if let Some(p) = raw.get("ping_interval").and_then(|v| v.as_f64()) {
        cfg.ping_interval = p;
    }
    if let Some(p) = raw.get("status_interval").and_then(|v| v.as_f64()) {
        cfg.status_interval = p;
    }
    Ok((cfg, false))
}

pub fn save(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(cfg).expect("config serializes");
    fs::write(path, json + "\n")
        .map_err(|e| format!("could not write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_creates_template_with_persisted_device_id() {
        let dir = std::env::temp_dir().join(format!("tspair-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.json");

        let (cfg, created) = load_or_create(&path).unwrap();
        assert!(created);
        assert_eq!(cfg.role, "client");
        assert_eq!(cfg.port, DEFAULT_PORT);
        assert!(!cfg.device_id.is_empty());

        // Reload: same device_id, not created again.
        let (cfg2, created2) = load_or_create(&path).unwrap();
        assert!(!created2);
        assert_eq!(cfg2.device_id, cfg.device_id);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn invalid_role_rejected() {
        let dir = std::env::temp_dir().join(format!("tspair-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"role": "banana"}"#).unwrap();
        let err = load_or_create(&path).unwrap_err();
        assert!(err.contains("banana"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn round_trips_all_fields() {
        let dir = std::env::temp_dir().join(format!("tspair-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let cfg = Config {
            role: "server".into(),
            port: 12345,
            device_id: "abc".into(),
            ping_interval: 7.5,
            status_interval: 60.0,
        };
        save(&path, &cfg).unwrap();
        let (back, _) = load_or_create(&path).unwrap();
        assert_eq!(back.role, "server");
        assert_eq!(back.port, 12345);
        assert_eq!(back.device_id, "abc");
        assert_eq!(back.ping_interval, 7.5);
        assert_eq!(back.status_interval, 60.0);
        std::fs::remove_dir_all(dir).ok();
    }
}
