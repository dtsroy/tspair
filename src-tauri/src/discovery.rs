//! Periodic peer detection via the tailscale CLI.
//!
//! The peer is whatever single other device shows up in `tailscale status` —
//! nothing about it is persisted. Both roles need its IP (as the exit-node
//! target), but only the client dials.

use crate::commands::{NotifyEvent, PeerInfoEvent};
use crate::tscli::{detect_peer, TsCli};
use crate::Coordinator;
use log::{info, warn};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

/// How the UI asks for an immediate re-detection ("Refresh now").
#[derive(Clone, Default)]
pub struct RefreshSignal(Arc<Notify>);

impl RefreshSignal {
    pub fn poke(&self) {
        self.0.notify_one();
    }
    async fn wait(&self, period: Duration) -> bool {
        let manual = tokio::select! {
            _ = tokio::time::sleep(period) => false,
            _ = self.0.notified() => true,
        };
        manual
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    app: AppHandle,
    coord: Arc<Coordinator>,
    cli: Arc<dyn TsCli>,
    refresh: RefreshSignal,
    role: String,
    status_interval: Duration,
) {
    // The CLI-reported exit-node advertisement, remembered across ticks so
    // the manual-refresh summary can mention it. Assigned on every loop
    // iteration before it is read.
    let mut last_summary: Option<PeerInfoEvent>;
    let mut warned_missing = false;

    loop {
        let mut peer = None;
        match cli.list_peers().await {
            Ok(peers) => peer = detect_peer(peers),
            Err(e) => warn!("[discovery] `tailscale status` failed: {e}"),
        }
        match peer {
            None => {
                info!("[discovery] no single other device found in the tailnet");
                if !warned_missing {
                    let _ = app.emit(
                        "notify",
                        NotifyEvent {
                            title: "tspair".into(),
                            body: "No other device found in the tailnet.".into(),
                        },
                    );
                    warned_missing = true;
                }
                last_summary = None;
            }
            Some(p) => {
                info!(
                    "[discovery] peer detected: {} ({}), {}, exit-node advertised={}",
                    p.hostname, p.ip,
                    if p.online { "online" } else { "offline" },
                    p.advertised_exit_node
                );
                warned_missing = false;
                coord.set_peer_ip(p.ip.clone()).await;
                coord.set_peer_advertised(p.advertised_exit_node).await;
                let summary = PeerInfoEvent {
                    hostname: p.hostname.clone(),
                    ip: p.ip.clone(),
                    online: p.online,
                    advertised: p.advertised_exit_node,
                };
                let _ = app.emit("peer-info", summary.clone());
                last_summary = Some(summary);
                if p.online && role == "client" {
                    info!("[discovery] role=client, peer online -> dialing {}", p.ip);
                    coord.link.dial(&p.ip).await;
                }
            }
        }

        // Sleep until the next tick, or return early on a manual refresh.
        if refresh.wait(status_interval).await {
            info!("[discovery] manual refresh: peer re-detection done, updating UI");
            let body = match &last_summary {
                Some(p) => format!(
                    "Peer {} ({}) is {}{}.",
                    p.hostname,
                    p.ip,
                    if p.online { "online" } else { "offline" },
                    if p.advertised {
                        ", advertising as exit node"
                    } else {
                        ""
                    }
                ),
                None => "No other device found in the tailnet.".into(),
            };
            let _ = app.emit("notify", NotifyEvent { title: "tspair".into(), body });
        }
    }
}
