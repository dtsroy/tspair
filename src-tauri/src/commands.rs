//! `#[tauri::command]` handlers — the UI → backend IPC surface.
//!
//! Events emitted backend → frontend: `state-changed`, `peer-info`, `notify`.

use crate::config::{self, Config};
use crate::exit_state::ExitState;
use crate::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Serialize)]
pub struct ActionResult {
    pub ok: bool,
    pub message: String,
}

/// Broadcast on every state transition; drives the dashboard, tray icon and
/// tray menu.
#[derive(Debug, Clone, Serialize)]
pub struct StateChangedEvent {
    pub mine: ExitState,
    pub peer: Option<ExitState>,
    pub connected: bool,
    pub status_line: String,
    pub peer_advertised: Option<bool>,
}

/// Peer card update, emitted by discovery.
#[derive(Debug, Clone, Serialize)]
pub struct PeerInfoEvent {
    pub hostname: String,
    pub ip: String,
    pub online: bool,
    pub advertised: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotifyEvent {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDto {
    pub role: String,
    pub port: u16,
    pub ping_interval: f64,
    pub status_interval: f64,
}

impl From<Config> for ConfigDto {
    fn from(c: Config) -> Self {
        Self {
            role: c.role,
            port: c.port,
            ping_interval: c.ping_interval,
            status_interval: c.status_interval,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatusInfo {
    pub connected: bool,
    pub role: String,
    pub port: u16,
    pub mine: ExitState,
    pub peer: Option<ExitState>,
    pub peer_ip: String,
    pub peer_advertised: Option<bool>,
    pub status_line: String,
}

#[tauri::command]
pub async fn get_status(state: State<'_, AppState>) -> Result<StatusInfo, String> {
    let (role, port) = {
        let cfg = state.config.lock().expect("config lock");
        (cfg.role.clone(), cfg.port)
    };
    let coord = &state.coord;
    Ok(StatusInfo {
        connected: coord.link.is_connected(),
        role,
        port,
        mine: coord.state().await,
        peer: coord.peer_state().await,
        peer_ip: coord.peer_ip().await,
        peer_advertised: coord.peer_advertised().await,
        status_line: coord.status_line().await,
    })
}

#[tauri::command]
pub async fn use_peer(state: State<'_, AppState>) -> Result<ActionResult, String> {
    let (ok, message) = state.coord.use_peer().await;
    Ok(ActionResult { ok, message })
}

#[tauri::command]
pub async fn stop_using_peer(state: State<'_, AppState>) -> Result<ActionResult, String> {
    let (ok, message) = state.coord.stop_using_peer().await;
    Ok(ActionResult { ok, message })
}

#[tauri::command]
pub async fn refresh_now(state: State<'_, AppState>) -> Result<(), String> {
    state.refresh.poke();
    Ok(())
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<ConfigDto, String> {
    let cfg = state.config.lock().expect("config lock");
    Ok(ConfigDto::from(cfg.clone()))
}

#[tauri::command]
pub fn set_config(state: State<'_, AppState>, config: ConfigDto) -> Result<ActionResult, String> {
    let mut new_cfg = Config {
        role: config.role.to_lowercase(),
        port: config.port,
        // device_id is not editable; keep the stored one.
        device_id: String::new(),
        ping_interval: config.ping_interval,
        status_interval: config.status_interval,
    };
    if !new_cfg.role_valid() {
        return Err(format!(
            "Invalid role {:?}: must be \"server\" or \"client\".",
            new_cfg.role
        ));
    }
    if new_cfg.ping_interval <= 0.0 || new_cfg.status_interval <= 0.0 {
        return Err("Intervals must be positive.".into());
    }
    {
        let mut stored = state.config.lock().expect("config lock");
        new_cfg.device_id = stored.device_id.clone();
        *stored = new_cfg.clone();
    }
    config::save(&state.config_path, &new_cfg)?;
    Ok(ActionResult {
        ok: true,
        message: "Saved. Role/port changes take effect after restarting the app.".into(),
    })
}
