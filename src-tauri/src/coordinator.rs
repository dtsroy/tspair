//! Exit-node coordinator: the 3-state machine plus the tailscale actions
//! and peer messages that drive it.
//!
//! States (ExitState):
//!   None        no exit-node relationship
//!   UsingOther  this device routes via the peer  (tailscale set --exit-node=peer)
//!   BeingUsed   the peer routes via this device  (tailscale set --advertise-exit-node)
//!
//! Rules:
//!   * Entering UsingOther requires an REQ_USE/GRANT handshake; the provider
//!     refuses if it is itself UsingOther, so a routing loop can never form.
//!   * Simultaneous "use" requests (both sides click at once) are broken by
//!     device_id: the side with the larger id yields.
//!   * When the user side drops back to None (STATE message), the provider
//!     automatically stops advertising and returns to None too.

use crate::exit_state::ExitState;
use crate::link::{LinkEvents, LinkHandle, Role};
use crate::protocol::Msg;
use crate::tscli::TsCli;
use log::{error, info, warn};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::{oneshot, Mutex};

/// Timings kept as data (not consts) so tests can shrink them; the defaults
/// are exactly v1's constants.
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    /// seconds to wait for GRANT/DENY
    pub request_timeout: Duration,
    /// the peer's exit-node advertisement needs time to propagate after GRANT
    pub grant_settle_delay: Duration,
    /// extra attempts when tailscale doesn't see the peer's advertisement yet
    pub exit_node_retries: u32,
    /// pause between exit-node apply retries
    pub retry_wait: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            grant_settle_delay: Duration::from_secs(3),
            exit_node_retries: 2,
            retry_wait: Duration::from_secs(2),
        }
    }
}

/// GRANT/DENY reply to an in-flight REQ_USE (replaces v1's threading.Event).
struct Reply {
    granted: bool,
    reason: String,
}

struct Inner {
    state: ExitState,
    peer_state: Option<ExitState>,
    peer_ip: String,
    /// peer advertises as exit node (from `tailscale status`); unset until
    /// the CLI reported it
    peer_advertised: Option<bool>,
    /// an outbound REQ_USE is in flight
    pending_out: bool,
    reply_tx: Option<oneshot::Sender<Reply>>,
}

pub struct Coordinator {
    pub link: LinkHandle,
    cli: Arc<dyn TsCli>,
    device_id: String,
    timings: Timings,
    inner: Mutex<Inner>,
    /// tray refresh hook (v1's on_change)
    on_change: StdMutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// set right after construction; lets LinkEvents spawn tasks on self
    self_weak: OnceLock<Weak<Coordinator>>,
}

impl Coordinator {
    /// Build the coordinator; call [`Coordinator::start`] once the returned
    /// Arc is in place (callbacks need it).
    pub fn new(
        device_id: &str,
        role: Role,
        port: u16,
        ping_interval: Duration,
        cli: Arc<dyn TsCli>,
        timings: Timings,
    ) -> Arc<Self> {
        Arc::new(Self {
            link: LinkHandle::new(device_id, role, port, ping_interval),
            cli,
            device_id: device_id.to_string(),
            timings,
            inner: Mutex::new(Inner {
                state: ExitState::None,
                peer_state: None,
                peer_ip: String::new(),
                peer_advertised: None,
                pending_out: false,
                reply_tx: None,
            }),
            on_change: StdMutex::new(None),
            self_weak: OnceLock::new(),
        })
    }

    /// Wire callbacks and spawn the background link tasks. Must be called
    /// after `new` and before any peer traffic is expected.
    pub fn start(self: &Arc<Self>) {
        let _ = self.self_weak.set(Arc::downgrade(self));
        self.link
            .set_events(&(Arc::clone(self) as Arc<dyn LinkEvents>));
        self.link.start();
    }

    /// Register the UI refresh hook.
    pub fn set_on_change(&self, f: Box<dyn Fn() + Send + Sync>) {
        *self.on_change.lock().unwrap() = Some(f);
    }

    fn notify_change(&self) {
        if let Some(f) = self.on_change.lock().unwrap().as_ref() {
            f();
        }
    }

    // ---- properties --------------------------------------------------

    pub async fn state(&self) -> ExitState {
        self.inner.lock().await.state
    }

    pub async fn peer_state(&self) -> Option<ExitState> {
        self.inner.lock().await.peer_state
    }

    pub async fn peer_advertised(&self) -> Option<bool> {
        self.inner.lock().await.peer_advertised
    }

    pub async fn status_line(&self) -> String {
        let inner = self.inner.lock().await;
        let peer = if self.link.is_connected() {
            "peer connected"
        } else {
            "peer offline"
        };
        let theirs = inner
            .peer_state
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".into());
        format!("{peer} | Us: {} | Peer: {theirs}", inner.state)
    }

    /// Record the peer's real exit-node advertisement from `tailscale status`.
    pub async fn set_peer_advertised(&self, advertised: bool) {
        {
            let mut inner = self.inner.lock().await;
            if inner.peer_advertised != Some(advertised) {
                info!(
                    "peer exit-node advertisement: {:?} -> {advertised}",
                    inner.peer_advertised
                );
            }
            inner.peer_advertised = Some(advertised);
        }
        if advertised {
            self.notify_change();
        }
    }

    pub async fn set_peer_ip(&self, ip: String) {
        self.inner.lock().await.peer_ip = ip;
    }

    pub async fn peer_ip(&self) -> String {
        self.inner.lock().await.peer_ip.clone()
    }

    // ---- user actions (tray / UI) --------------------------------------

    /// Try to route this device's traffic via the peer (None -> UsingOther).
    pub async fn use_peer(self: &Arc<Self>) -> (bool, String) {
        let rx = {
            let mut inner = self.inner.lock().await;
            if inner.state != ExitState::None {
                warn!("[use_peer] step 1/5 refused: this device is {}", inner.state);
                return (
                    false,
                    format!("Cannot start: this device is {}.", inner.state),
                );
            }
            if !self.link.is_connected() {
                warn!("[use_peer] step 1/5 refused: link is down");
                return (false, "Peer is not connected.".into());
            }
            info!(
                "[use_peer] step 1/5 preconditions ok (state={}, peer_state={})",
                inner.state,
                inner
                    .peer_state
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
            let (tx, rx) = oneshot::channel();
            inner.reply_tx = Some(tx);
            inner.pending_out = true;
            rx
        };
        let result = self.request_and_apply(rx).await;
        // `finally` from v1: the request is over either way.
        {
            let mut inner = self.inner.lock().await;
            inner.pending_out = false;
            inner.reply_tx = None;
        }
        result
    }

    async fn request_and_apply(self: &Arc<Self>, rx: oneshot::Receiver<Reply>) -> (bool, String) {
        if !self.link.send(Msg::ReqUse) {
            warn!("[use_peer] step 2/5 failed: could not send REQ_USE (link dropped)");
            return (false, "Peer is not connected.".into());
        }
        info!(
            "[use_peer] step 2/5 REQ_USE sent; waiting for peer reply (timeout {:?})",
            self.timings.request_timeout
        );
        let reply = match tokio::time::timeout(self.timings.request_timeout, rx).await {
            Err(_) => {
                warn!(
                    "[use_peer] step 3/5 timed out: no GRANT/DENY within {:?}",
                    self.timings.request_timeout
                );
                return (false, "Peer did not answer the request (timeout).".into());
            }
            Ok(Err(_)) => {
                // Reply channel dropped without a verdict: connection lost.
                warn!("[use_peer] step 3/5 connection lost while waiting for the peer");
                return (false, "Peer refused: connection lost".into());
            }
            Ok(Ok(reply)) => reply,
        };
        if !reply.granted {
            let reason = if reply.reason.is_empty() {
                "unknown reason"
            } else {
                &reply.reason
            };
            warn!("[use_peer] step 3/5 peer DENYed: {reason}");
            return (false, format!("Peer refused: {reason}"));
        }
        info!("[use_peer] step 3/5 peer GRANTed the request");

        let peer_ip = self.inner.lock().await.peer_ip.clone();
        if peer_ip.is_empty() {
            warn!("[use_peer] step 4/5 aborted: peer Tailscale IP not discovered yet");
            return (false, "Peer Tailscale IP unknown yet; try again shortly.".into());
        }
        // The peer has just advertised itself as an exit node; give the
        // tailscale backend time to propagate that before we point at it.
        info!(
            "[use_peer] step 4/5 waiting {:?} for the peer's advertisement to propagate, then applying exit node (target {peer_ip})",
            self.timings.grant_settle_delay
        );
        tokio::time::sleep(self.timings.grant_settle_delay).await;
        let mut last_err = None;
        for attempt in 0..=self.timings.exit_node_retries {
            match self.cli.set_exit_node(&peer_ip).await {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    if attempt < self.timings.exit_node_retries {
                        warn!(
                            "[use_peer] step 4/5 attempt {} failed; retrying in {:?} (peer's advertisement may not be visible yet)",
                            attempt + 1,
                            self.timings.retry_wait
                        );
                        tokio::time::sleep(self.timings.retry_wait).await;
                    }
                }
            }
        }
        if let Some(e) = last_err {
            // Provider is left advertising with no user — tell it to unwind.
            error!(
                "[use_peer] step 4/5 failed after {} attempts: {e}",
                self.timings.exit_node_retries + 1
            );
            self.link.send(Msg::UseFailed { reason: e.to_string() });
            return (false, format!("tailscale rejected the exit node: {e}"));
        }
        info!("[use_peer] step 4/5 exit node applied");

        self.inner.lock().await.state = ExitState::UsingOther;
        info!("[use_peer] step 5/5 state -> USING_OTHER; broadcasting to peer");
        self.broadcast_state().await;
        (true, format!("Now using peer ({peer_ip}) as exit node."))
    }

    /// Stop routing via the peer (UsingOther -> None). Provider unwinds on
    /// its own.
    pub async fn stop_using_peer(&self) -> (bool, String) {
        {
            let inner = self.inner.lock().await;
            if inner.state != ExitState::UsingOther {
                warn!(
                    "[stop_using] refused: this device is {}, not USING_OTHER",
                    inner.state
                );
                return (
                    false,
                    "This device is not using the peer as exit node.".into(),
                );
            }
            info!("[stop_using] step 1/3 state is USING_OTHER; clearing exit node");
        }
        if let Err(e) = self.cli.clear_exit_node().await {
            error!("[stop_using] step 2/3 failed: could not clear exit node: {e}");
            return (false, format!("Failed to clear exit node: {e}"));
        }
        info!("[stop_using] step 2/3 exit node cleared via tailscale CLI");
        self.inner.lock().await.state = ExitState::None;
        info!("[stop_using] step 3/3 state -> NONE; broadcasting to peer");
        self.broadcast_state().await; // peer sees NONE and un-advertises by itself
        (true, "Back to normal routing.".into())
    }

    // ---- state transitions ----------------------------------------------

    async fn apply_peer_state(self: &Arc<Self>, new: ExitState) {
        let mut unwind = false;
        {
            let mut inner = self.inner.lock().await;
            let old = inner.peer_state;
            inner.peer_state = Some(new);
            // Peer stopped routing via us -> stop advertising, return to None.
            if new == ExitState::None && inner.state == ExitState::BeingUsed {
                info!("peer returned to NONE; un-advertising exit node");
                unwind = true;
            }
            if old != Some(new) {
                info!("peer state: {:?} -> {}", old, new);
            }
        }
        if unwind {
            self.unadvertise().await;
        } else {
            self.notify_change();
        }
    }

    /// BeingUsed -> None.
    async fn unadvertise(self: &Arc<Self>) {
        self.inner.lock().await.state = ExitState::None;
        if let Err(e) = self.cli.advertise_exit_node(false).await {
            error!("failed to stop advertising exit node: {e}");
        }
        self.broadcast_state().await;
    }

    async fn broadcast_state(&self) {
        let state = self.inner.lock().await.state;
        self.link.send(Msg::State { state });
        self.notify_change();
    }

    // ---- provider side of the handshake ---------------------------------

    async fn handle_request(self: &Arc<Self>) {
        info!("[provider] REQ_USE received from peer");
        {
            let mut inner = self.inner.lock().await;
            // I'm routing via the peer: granting would create a loop.
            if inner.state == ExitState::UsingOther {
                warn!("[provider] denying: routing loop (this device is USING_OTHER)");
                self.link.send(Msg::Deny {
                    reason: "routing loop: both devices would use each other".into(),
                });
                return;
            }
            if inner.state == ExitState::BeingUsed {
                // Already advertising for the peer — keep advertising and grant again.
                info!("[provider] already BEING_USED; re-granting (advertisement unchanged)");
            }

            // Simultaneous requests: larger device_id yields.
            if inner.pending_out && inner.reply_tx.is_some() {
                let peer_id = self.link.last_peer_id().await;
                let peer_id: &str = if peer_id.is_empty() { "zzzzzzzz" } else { peer_id.as_str() };
                if self.device_id.as_str() > peer_id {
                    info!("[provider] simultaneous request: this device yields (larger id)");
                    self.link.send(Msg::Deny {
                        reason: "simultaneous request; I yielded to you".into(),
                    });
                    return;
                }
                // They will yield to us (their id is larger) and grant ours.
                info!("[provider] simultaneous request: peer yields to this device");
            }

            info!("[provider] advertising this device as exit node via tailscale CLI");
            if let Err(e) = self.cli.advertise_exit_node(true).await {
                error!("[provider] denying: advertise failed: {e}");
                self.link
                    .send(Msg::Deny { reason: format!("tailscale error: {e}") });
                return;
            }
            inner.state = ExitState::BeingUsed;
        }
        self.link.send(Msg::Grant);
        info!("[provider] GRANT sent; state -> BEING_USED");
        self.broadcast_state().await;
    }

    // ---- inbound messages (LinkEvents) -----------------------------------

    async fn handle_msg(self: &Arc<Self>, msg: Msg) {
        match msg {
            Msg::Ping => {
                self.link.send(Msg::Pong);
            }
            Msg::Pong => {}
            Msg::State { state } => self.apply_peer_state(state).await,
            Msg::ReqUse => self.handle_request().await,
            Msg::Grant => {
                let mut inner = self.inner.lock().await;
                if let Some(tx) = inner.reply_tx.take() {
                    let _ = tx.send(Reply {
                        granted: true,
                        reason: String::new(),
                    });
                }
            }
            Msg::Deny { reason } => {
                let mut inner = self.inner.lock().await;
                if let Some(tx) = inner.reply_tx.take() {
                    let _ = tx.send(Reply {
                        granted: false,
                        reason,
                    });
                }
            }
            Msg::UseFailed { reason } => {
                // We granted, but the peer failed to apply the exit node.
                self.unadvertise().await;
                let mut inner = self.inner.lock().await;
                if let Some(tx) = inner.reply_tx.take() {
                    let _ = tx.send(Reply {
                        granted: false,
                        reason,
                    });
                }
            }
            Msg::Hello { .. } => {} // handshake-only, never reaches here
        }
    }
}

impl LinkEvents for Coordinator {
    fn on_connected(&self) {
        // Sync state to the peer right after the handshake.
        let me = self.spawnable();
        tokio::spawn(async move { me.broadcast_state().await });
    }

    fn on_msg(&self, msg: Msg) {
        let me = self.spawnable();
        tokio::spawn(async move { me.handle_msg(msg).await });
    }

    fn on_down(&self) {
        let me = self.spawnable();
        tokio::spawn(async move {
            {
                let mut inner = me.inner.lock().await;
                inner.peer_state = None;
                if let Some(tx) = inner.reply_tx.take() {
                    let _ = tx.send(Reply {
                        granted: false,
                        reason: "connection lost".into(),
                    });
                }
            }
            me.notify_change();
        });
    }
}

impl Coordinator {
    fn spawnable(&self) -> Arc<Self> {
        self.self_weak
            .get()
            .and_then(Weak::upgrade)
            .expect("coordinator started before link traffic")
    }

    // ---- test hooks ------------------------------------------------------

    /// Force the local state (test scenarios that can't be driven over the
    /// wire, e.g. the provider-side loop guard).
    #[cfg(test)]
    pub async fn force_state(&self, state: ExitState) {
        self.inner.lock().await.state = state;
    }

    /// Pretend an outbound REQ_USE is in flight (tie-break scenarios).
    #[cfg(test)]
    pub async fn arm_pending(&self) {
        let (tx, _rx) = oneshot::channel();
        let mut inner = self.inner.lock().await;
        inner.pending_out = true;
        inner.reply_tx = Some(tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tscli::{PeerEntry, TailscaleError};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// CLI stub: records every call, fails on demand.
    #[derive(Default)]
    struct MockCli {
        calls: StdMutex<Vec<String>>,
        fail_set_exit_node: AtomicBool,
    }

    impl MockCli {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TsCli for MockCli {
        async fn list_peers(&self) -> Result<Vec<PeerEntry>, TailscaleError> {
            Ok(vec![])
        }
        async fn set_exit_node(&self, ip: &str) -> Result<(), TailscaleError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("set_exit_node({ip})"));
            if self.fail_set_exit_node.load(Ordering::SeqCst) {
                return Err(TailscaleError::Failed("mock: advertisement not visible".into()));
            }
            Ok(())
        }
        async fn clear_exit_node(&self) -> Result<(), TailscaleError> {
            self.calls.lock().unwrap().push("clear_exit_node".into());
            Ok(())
        }
        async fn advertise_exit_node(&self, on: bool) -> Result<(), TailscaleError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("advertise({on})"));
            Ok(())
        }
    }

    const FAST: Timings = Timings {
        request_timeout: Duration::from_secs(2),
        grant_settle_delay: Duration::from_millis(30),
        exit_node_retries: 1,
        retry_wait: Duration::from_millis(30),
    };

    async fn wait_until(what: &str, cond: impl AsyncFnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut cond = cond;
        loop {
            if cond().await {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn wait_state(coord: &Coordinator, expected: ExitState) {
        wait_until("state change", || async { coord.state().await == expected }).await;
    }

    /// Client (id "1111…") + server (id "2222…") pair over a real loopback
    /// TCP link, with mock CLIs. The client knows the peer's IP.
    async fn make_pair() -> (Arc<Coordinator>, Arc<Coordinator>, Arc<MockCli>, Arc<MockCli>) {
        let cli_client = Arc::new(MockCli::default());
        let cli_server = Arc::new(MockCli::default());
        // Server first so the client can dial its (ephemeral) bound port.
        let server = Coordinator::new(
            "2222-server-id",
            Role::Server,
            0,
            Duration::from_millis(100),
            cli_server.clone(),
            FAST,
        );
        server.start();
        wait_until("server bound", || async { server.link.bound_port() != 0 }).await;
        let client = Coordinator::new(
            "1111-client-id",
            Role::Client,
            server.link.bound_port(),
            Duration::from_millis(100),
            cli_client.clone(),
            FAST,
        );
        client.start();
        client.link.dial("127.0.0.1").await;
        wait_until("pair connected", || async {
            client.link.is_connected() && server.link.is_connected()
        })
        .await;
        client.set_peer_ip("100.1.2.3".into()).await;
        (client, server, cli_client, cli_server)
    }

    #[tokio::test]
    async fn use_and_stop_flow() {
        let (client, server, cli_client, cli_server) = make_pair().await;

        let (ok, msg) = client.use_peer().await;
        assert!(ok, "use_peer should succeed: {msg}");

        wait_state(&client, ExitState::UsingOther).await;
        wait_state(&server, ExitState::BeingUsed).await;
        assert!(cli_client.calls().contains(&"set_exit_node(100.1.2.3)".into()));
        assert!(cli_server.calls().contains(&"advertise(true)".into()));

        let (ok, msg) = client.stop_using_peer().await;
        assert!(ok, "stop_using_peer should succeed: {msg}");
        wait_state(&client, ExitState::None).await;
        // Auto-unwind: the provider saw NONE and stopped advertising.
        wait_until("server un-advertised", || async {
            server.state().await == ExitState::None
                && cli_server.calls().contains(&"advertise(false)".into())
        })
        .await;
        assert!(cli_client.calls().contains(&"clear_exit_node".into()));
    }

    #[tokio::test]
    async fn loop_guard_denies_when_provider_is_using_other() {
        let (client, server, _cc, _cs) = make_pair().await;

        // Establish the normal direction: client uses server.
        let (ok, msg) = client.use_peer().await;
        assert!(ok, "{msg}");
        wait_state(&client, ExitState::UsingOther).await;

        // Pretend the server fell back to NONE (state sync lost), so its
        // use_peer preconditions pass and a REQ_USE actually reaches the
        // client — which is USING_OTHER and must deny.
        server.force_state(ExitState::None).await;
        let (ok, msg) = server.use_peer().await;
        assert!(!ok);
        assert!(
            msg.contains("routing loop"),
            "expected the loop-guard denial, got: {msg}"
        );
    }

    #[tokio::test]
    async fn simultaneous_requests_larger_id_yields() {
        // Server id "2222…" > client id "1111…": when both are pending, the
        // server (larger id) yields and the client is refused.
        let (client, server, _cc, _cs) = make_pair().await;

        server.arm_pending().await;
        let (ok, msg) = client.use_peer().await;
        assert!(!ok, "client must be refused when the larger id yields");
        assert!(
            msg.contains("simultaneous request"),
            "unexpected message: {msg}"
        );
    }

    #[tokio::test]
    async fn simultaneous_requests_smaller_id_grants() {
        // Mirror image: the provider has the *smaller* id, so it grants and
        // the requester proceeds to USING_OTHER.
        let cli_client = Arc::new(MockCli::default());
        let cli_server = Arc::new(MockCli::default());
        let server = Coordinator::new(
            "0500-server-id", // smaller than the client
            Role::Server,
            0,
            Duration::from_millis(100),
            cli_server.clone(),
            FAST,
        );
        server.start();
        wait_until("server bound", || async { server.link.bound_port() != 0 }).await;
        let client = Coordinator::new(
            "0999-client-id",
            Role::Client,
            server.link.bound_port(),
            Duration::from_millis(100),
            cli_client.clone(),
            FAST,
        );
        client.start();
        client.link.dial("127.0.0.1").await;
        wait_until("pair connected", || async { client.link.is_connected() }).await;
        client.set_peer_ip("100.1.2.3".into()).await;

        server.arm_pending().await;
        let (ok, msg) = client.use_peer().await;
        assert!(ok, "client should win the tie-break: {msg}");
        wait_state(&client, ExitState::UsingOther).await;
        wait_state(&server, ExitState::BeingUsed).await;
    }

    #[tokio::test]
    async fn use_failed_unwinds_provider() {
        let (client, server, cli_client, cli_server) = make_pair().await;
        cli_client.fail_set_exit_node.store(true, Ordering::SeqCst);

        let (ok, msg) = client.use_peer().await;
        assert!(!ok, "use_peer must fail when the CLI rejects the exit node");
        assert!(
            msg.contains("tailscale rejected the exit node"),
            "unexpected message: {msg}"
        );

        // Provider granted, then got USE_FAILED and unwound.
        wait_until("server granted then un-advertised", || async {
            let calls = cli_server.calls();
            calls.contains(&"advertise(true)".into())
                && calls.contains(&"advertise(false)".into())
                && server.state().await == ExitState::None
        })
        .await;
        assert_eq!(client.state().await, ExitState::None);
    }

    #[tokio::test]
    async fn refused_when_link_down() {
        let client = Coordinator::new(
            "1111-client-id",
            Role::Client,
            59999, // nothing listening
            Duration::from_millis(100),
            Arc::new(MockCli::default()),
            FAST,
        );
        client.start();
        let (ok, msg) = client.use_peer().await;
        assert!(!ok);
        assert_eq!(msg, "Peer is not connected.");
    }
}
