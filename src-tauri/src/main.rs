//! Application entry point: wires config, discovery loop, link, coordinator,
//! tray and the dashboard window.

mod commands;
mod config;
mod coordinator;
mod discovery;
mod exit_state;
mod link;
mod protocol;
mod tscli;

use commands::NotifyEvent;
use coordinator::{Coordinator, Timings};
use discovery::RefreshSignal;
use exit_state::ExitState;
use link::Role;
use log::info;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tscli::TsCli;

/// Everything the IPC commands and the tray need.
pub struct AppState {
    pub coord: Arc<Coordinator>,
    pub cli: Arc<dyn TsCli>,
    pub refresh: RefreshSignal,
    pub config_path: std::path::PathBuf,
    pub config: StdMutex<config::Config>,
}

/// The tray handle lives here because the tray is built after `AppState` is
/// managed, while tray updates need `AppState`.
pub struct TrayState(StdMutex<TrayIcon>);

/// The four tray icons (plan §4.1), loaded once at startup.
struct TrayIcons {
    idle: tauri::image::Image<'static>,
    using: tauri::image::Image<'static>,
    serving: tauri::image::Image<'static>,
    offline: tauri::image::Image<'static>,
}

fn tray_icons() -> TrayIcons {
    // Windows keeps v1's colored dots; macOS uses monochrome template
    // images whose shapes distinguish the states.
    #[cfg(target_os = "macos")]
    {
        TrayIcons {
            idle: tauri::include_image!("icons/tray-idle-mono.png"),
            using: tauri::include_image!("icons/tray-using-mono.png"),
            serving: tauri::include_image!("icons/tray-serving-mono.png"),
            offline: tauri::include_image!("icons/tray-offline-mono.png"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        TrayIcons {
            idle: tauri::include_image!("icons/tray-idle.png"),
            using: tauri::include_image!("icons/tray-using.png"),
            serving: tauri::include_image!("icons/tray-serving.png"),
            offline: tauri::include_image!("icons/tray-offline.png"),
        }
    }
}

fn icon_for(icons: &TrayIcons, connected: bool, mine: ExitState) -> tauri::image::Image<'static> {
    if !connected && mine == ExitState::None {
        icons.offline.clone()
    } else {
        match mine {
            ExitState::UsingOther => icons.using.clone(),
            ExitState::BeingUsed => icons.serving.clone(),
            ExitState::None => icons.idle.clone(),
        }
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Second launch: just reveal the existing dashboard.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .setup(|app| {
            // macOS: tray-only presence, no Dock icon.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();

            let cfg_path = config::config_path(&handle);
            let (cfg, created) = match config::load_or_create(&cfg_path) {
                Ok(r) => r,
                Err(e) => {
                    // Invalid config is fatal, as in v1.
                    log::error!("{e}");
                    return Err(e.into());
                }
            };
            if created {
                info!("config template written to {}", cfg_path.display());
                let _ = handle.emit(
                    "notify",
                    NotifyEvent {
                        title: "tspair".into(),
                        body: format!(
                            "A config was written to {}. On the peer's device, set \"role\" to \"server\".",
                            cfg_path.display()
                        ),
                    },
                );
            }

            let cli: Arc<dyn TsCli> = Arc::new(tscli::Cli::resolve());
            let role = if cfg.role == "server" { Role::Server } else { Role::Client };
            let coord = Coordinator::new(
                &cfg.device_id,
                role,
                cfg.port,
                Duration::from_secs_f64(cfg.ping_interval),
                cli.clone(),
                Timings::default(),
            );
            coord.start();
            info!(
                "tspair started (device_id={}, role={})",
                &cfg.device_id[..cfg.device_id.len().min(8)],
                cfg.role
            );

            let refresh = RefreshSignal::default();
            app.manage(AppState {
                coord: coord.clone(),
                cli,
                refresh: refresh.clone(),
                config_path: cfg_path,
                config: StdMutex::new(cfg),
            });

            // State transitions → dashboard event + tray refresh.
            {
                let h = handle.clone();
                coord.set_on_change(Box::new(move || {
                    let h = h.clone();
                    tauri::async_runtime::spawn(async move {
                        update_tray_and_emit(&h).await;
                    });
                }));
            }

            app.manage(tray_icons());
            let icons = app.state::<TrayIcons>().inner();
            let tray = build_tray(&handle, icons)?;
            app.manage(TrayState(StdMutex::new(tray)));

            // Discovery loop: peer detection + client dialing.
            {
                let st = app.state::<AppState>();
                let (coord, cli) = (st.coord.clone(), st.cli.clone());
                let (role, status_interval) = {
                    let cfg = st.config.lock().unwrap();
                    (cfg.role.clone(), cfg.status_interval)
                };
                tauri::async_runtime::spawn(discovery::run(
                    handle.clone(),
                    coord,
                    cli,
                    refresh,
                    role,
                    Duration::from_secs_f64(status_interval),
                ));
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close button hides to tray; Quit only from the tray menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::use_peer,
            commands::stop_using_peer,
            commands::refresh_now,
            commands::get_config,
            commands::set_config,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tspair");
}

fn build_tray(app: &AppHandle, icons: &TrayIcons) -> Result<TrayIcon, tauri::Error> {
    #[allow(unused_mut)] // mut only used on macOS (icon_as_template)
    let mut builder = TrayIconBuilder::with_id("main")
        .icon(icon_for(icons, false, ExitState::None))
        .tooltip("tspair")
        .menu(&tray_menu(app, "starting…", false, ExitState::None, None, "client")?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_tray_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            // Always-clickable: left click opens the dashboard.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_dashboard(tray.app_handle());
            }
        });
    // Monochrome template icons adapt to light/dark menu bars.
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(true);
    }
    builder.build(app)
}

fn show_dashboard(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

fn tray_menu(
    app: &AppHandle,
    status_line: &str,
    connected: bool,
    mine: ExitState,
    peer_advertised: Option<bool>,
    role: &str,
) -> Result<Menu<tauri::Wry>, tauri::Error> {
    let mut peer_txt = format!("peer: {}", if connected { "connected" } else { "offline" });
    if peer_advertised == Some(true) {
        peer_txt += " (exit node advertised)";
    }
    let status = MenuItem::with_id(app, "status", status_line, false, None::<&str>)?;
    let peer = MenuItem::with_id(app, "peer", peer_txt, false, None::<&str>)?;
    let mode = MenuItem::with_id(app, "mode", format!("mode: {role}"), false, None::<&str>)?;
    let use_item = MenuItem::with_id(app, "use", "Use peer as exit node", true, None::<&str>)?;
    let stop_item = MenuItem::with_id(
        app,
        "stop",
        "Stop using peer as exit node",
        mine == ExitState::UsingOther,
        None::<&str>,
    )?;
    let refresh_item = MenuItem::with_id(app, "refresh", "Refresh now", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    Menu::with_items(
        app,
        &[
            &status,
            &peer,
            &mode,
            &PredefinedMenuItem::separator(app)?,
            &use_item,
            &stop_item,
            &refresh_item,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

/// Tray menu actions (v1 tray.py). Actions run off the menu thread and
/// report via a `notify` event.
fn on_tray_menu(app: &AppHandle, id: &str) {
    match id {
        "use" | "stop" => {
            let app = app.clone();
            let action = id.to_string();
            tauri::async_runtime::spawn(async move {
                let (ok, msg) = {
                    let st = app.state::<AppState>();
                    if action == "use" {
                        st.coord.use_peer().await
                    } else {
                        st.coord.stop_using_peer().await
                    }
                };
                info!("[tray] action {action} finished: ok={ok} ({msg})");
                let _ = app.emit("notify", NotifyEvent { title: "tspair".into(), body: msg });
            });
        }
        "refresh" => {
            info!("[tray] button 'refresh' clicked -> triggering peer re-detection");
            app.state::<AppState>().refresh.poke();
        }
        "open" => show_dashboard(app),
        "quit" => app.exit(0),
        _ => {}
    }
}

/// Broadcast the state to the dashboard and refresh tray icon + menu.
async fn update_tray_and_emit(app: &AppHandle) {
    let event = {
        let st = app.state::<AppState>();
        let role = st.config.lock().unwrap().role.clone();
        let event = commands::StateChangedEvent {
            mine: st.coord.state().await,
            peer: st.coord.peer_state().await,
            connected: st.coord.link.is_connected(),
            status_line: st.coord.status_line().await,
            peer_advertised: st.coord.peer_advertised().await,
        };
        (event, role)
    };
    let (event, role) = event;
    let _ = app.emit("state-changed", event.clone());

    let icons = app.state::<TrayIcons>().inner();
    let tray = {
        let state = app.state::<TrayState>();
        let guard = state.inner().0.lock().unwrap();
        guard.clone()
    };
    let _ = tray.set_icon(Some(icon_for(icons, event.connected, event.mine)));
    let _ = tray.set_tooltip(Some(format!("tspair — {}", event.mine)));
    let menu = tray_menu(
        app,
        &event.status_line,
        event.connected,
        event.mine,
        event.peer_advertised,
        &role,
    );
    if let Ok(menu) = menu {
        let _ = tray.set_menu(Some(menu));
    }
}
