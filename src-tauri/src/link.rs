//! Peer TCP link: at most one live connection to the peer at any time.
//!
//! The two ends of the pair have asymmetric roles (set in the config):
//!   * "server" — listens on the port, never dials.
//!   * "client" — dials the peer's Tailscale IP whenever the peer is online.
//!
//! The link is transport-only; message semantics live in coordinator.rs.

use crate::protocol::{Msg, MsgCodec};
use futures_util::{SinkExt, StreamExt};
use log::{debug, info};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_util::codec::Framed;

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(3);
/// Outbound frame queue; a stalled peer fills this and gets dropped.
const SEND_QUEUE: usize = 64;

static NEXT_CONN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Server,
    Client,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Server => "server",
            Role::Client => "client",
        }
    }
}

/// Callbacks into the coordinator (v1's on_connected / on_msg / on_down).
/// Sync on purpose: implementations only touch non-blocking state and
/// spawn tasks for anything slow.
pub trait LinkEvents: Send + Sync {
    fn on_connected(&self);
    fn on_msg(&self, msg: Msg);
    fn on_down(&self);
}

struct LinkShared {
    device_id: String,
    role: Role,
    port: u16,
    ping_interval: Duration,
    /// Bound after construction (the coordinator's Arc doesn't exist yet
    /// while the link is being built).
    events: OnceLock<Weak<dyn LinkEvents>>,
    /// The live, HELLO-completed connection: (generation, frame sender).
    conn: Mutex<Option<(u64, mpsc::Sender<Msg>)>>,
    last_peer_id: Mutex<String>,
    /// Port actually bound by the listener (differs from `port` when 0).
    bound_port: AtomicU16,
    /// 0 = not started; start() spawns the background loops exactly once.
    started: AtomicU64,
}

/// Cloneable handle around the shared link state (v1's `PeerLink`).
#[derive(Clone)]
pub struct LinkHandle {
    shared: Arc<LinkShared>,
}

impl LinkHandle {
    pub fn new(device_id: impl Into<String>, role: Role, port: u16, ping_interval: Duration) -> Self {
        Self {
            shared: Arc::new(LinkShared {
                device_id: device_id.into(),
                role,
                port,
                ping_interval,
                events: OnceLock::new(),
                conn: Mutex::new(None),
                last_peer_id: Mutex::new(String::new()),
                bound_port: AtomicU16::new(0),
                started: AtomicU64::new(0),
            }),
        }
    }

    /// Attach the event sink (the coordinator). Must be called before the
    /// first connection is expected.
    pub fn set_events(&self, events: &Arc<dyn LinkEvents>) {
        let _ = self.shared.events.set(Arc::downgrade(events));
    }

    /// Spawn the listener (server role) and the heartbeat loop.
    pub fn start(&self) {
        if self.shared.started.swap(1, Ordering::SeqCst) != 0 {
            return;
        }
        if self.shared.role == Role::Server {
            let shared = Arc::clone(&self.shared);
            tokio::spawn(async move { listen_loop(shared).await });
        }
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move { ping_loop(shared).await });
    }

    pub fn is_connected(&self) -> bool {
        self.shared
            .conn
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(true) // contended == alive
    }

    pub async fn last_peer_id(&self) -> String {
        self.shared.last_peer_id.lock().await.clone()
    }

    pub fn role(&self) -> Role {
        self.shared.role
    }

    /// Port bound by the listener; 0 until the server has bound (used with
    /// port 0 in tests).
    pub fn bound_port(&self) -> u16 {
        self.shared.bound_port.load(Ordering::SeqCst)
    }

    /// Try to establish the outbound connection; safe to call repeatedly.
    /// Server-role links only listen — dialing is a no-op for them.
    pub async fn dial(&self, ip: &str) -> bool {
        if self.shared.role == Role::Server {
            return self.is_connected();
        }
        if self.is_connected() {
            return true;
        }
        let addr = format!("{ip}:{}", self.shared.port);
        let port = self.shared.port;
        let stream = match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(&addr)).await {
            Err(_) => {
                debug!("dial {addr} timed out");
                return false;
            }
            Ok(Err(e)) => {
                debug!("dial {addr} failed: {e}");
                return false;
            }
            Ok(Ok(s)) => s,
        };
        let shared = Arc::clone(&self.shared);
        tokio::spawn(serve(stream, shared, port, "outbound"));
        true
    }

    /// Send a message; a failed send drops the connection (v1 semantics).
    /// Non-blocking: frames queue in a bounded channel; a stalled peer
    /// overflows it and is dropped.
    pub fn send(&self, msg: Msg) -> bool {
        let guard = match self.shared.conn.try_lock() {
            Ok(g) => g,
            Err(_) => return false, // busy mid-admission/drop — treat as down
        };
        match guard.as_ref() {
            None => false,
            Some((gen, tx)) => {
                let gen = *gen;
                match tx.try_send(msg) {
                    Ok(()) => true,
                    Err(e) => {
                        debug!("send failed, dropping connection: {e}");
                        let shared = Arc::clone(&self.shared);
                        tokio::spawn(async move { drop_conn(&shared, gen).await });
                        false
                    }
                }
            }
        }
    }

    /// Close any live connection (app shutdown).
    pub async fn close(&self) {
        let gen = self.shared.conn.lock().await.take().map(|(gen, _)| gen);
        if let Some(gen) = gen {
            drop_conn(&self.shared, gen).await;
        }
    }
}

async fn listen_loop(shared: Arc<LinkShared>) {
    let listener = match TcpListener::bind(("0.0.0.0", shared.port)).await {
        Ok(l) => l,
        Err(e) => {
            log::error!("could not bind port {}: {e}", shared.port);
            return;
        }
    };
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(shared.port);
    shared.bound_port.store(port, Ordering::SeqCst);
    info!("listening on port {port}");
    loop {
        match listener.accept().await {
            Err(e) => {
                debug!("accept failed, stopping listener: {e}");
                break;
            }
            Ok((stream, addr)) => {
                let shared = Arc::clone(&shared);
                let port = shared.bound_port.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    debug!("inbound connection from {addr}");
                    serve(stream, shared, port, "inbound").await;
                });
            }
        }
    }
}

async fn ping_loop(shared: Arc<LinkShared>) {
    let mut tick = tokio::time::interval(shared.ping_interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick fires immediately; skip it
    loop {
        tick.tick().await;
        let connected = shared
            .conn
            .try_lock()
            .map(|g| g.is_some())
            .unwrap_or(true);
        if connected {
            let handle = LinkHandle { shared: Arc::clone(&shared) };
            if !handle.send(Msg::Ping) {
                info!("peer unreachable (ping failed)");
            }
        }
    }
}

/// Serve one connection: HELLO handshake, admission, then pump frames both
/// ways until the link drops.
async fn serve(stream: TcpStream, shared: Arc<LinkShared>, port: u16, kind: &'static str) {
    let events = match shared.events.get().and_then(Weak::upgrade) {
        Some(e) => e,
        None => return, // not wired or app is shutting down
    };
    debug!("{kind} connection on port {port}");
    let mut framed = Framed::new(stream, MsgCodec::new());

    // Handshake: both sides announce identity, bounded by HELLO_TIMEOUT.
    if let Err(e) = framed.send(Msg::Hello { id: shared.device_id.clone() }).await {
        debug!("{kind}: hello send failed: {e}");
        return;
    }
    let peer_id = loop {
        match tokio::time::timeout(HELLO_TIMEOUT, framed.next()).await {
            Err(_) => {
                debug!("{kind}: handshake timed out");
                return;
            }
            Ok(None) => {
                debug!("{kind}: closed during handshake");
                return;
            }
            Ok(Some(Err(e))) => {
                debug!("{kind}: handshake error: {e}");
                return;
            }
            Ok(Some(Ok(Msg::Hello { id }))) => break id,
            Ok(Some(Ok(_))) => {
                debug!("{kind}: protocol error: expected HELLO");
                return;
            }
        }
    };
    *shared.last_peer_id.lock().await = peer_id.clone();

    // Single-connection admission: drop duplicates.
    let (tx, mut rx) = mpsc::channel::<Msg>(SEND_QUEUE);
    let gen = NEXT_CONN.fetch_add(1, Ordering::SeqCst);
    {
        let mut conn = shared.conn.lock().await;
        if conn.is_some() {
            info!("duplicate connection rejected (already connected)");
            return;
        }
        *conn = Some((gen, tx.clone()));
    }
    info!(
        "link established with {} ({kind})",
        &peer_id[..peer_id.len().min(8)]
    );
    events.on_connected();

    // Writer pump: drains the frame queue into the socket. Ends when the
    // queue closes (reader done / dropped) or a write fails.
    let (mut sink, mut inbound) = framed.split();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                debug!("write failed, dropping connection");
                break;
            }
        }
    });

    // Reader loop.
    while let Some(item) = inbound.next().await {
        match item {
            Ok(msg) => events.on_msg(msg),
            Err(e) => {
                debug!("connection error ({kind}): {e}");
                break;
            }
        }
    }
    drop(tx); // unblock the writer pump
    let _ = writer.await;

    if drop_conn(&shared, gen).await {
        events.on_down();
    }
}

/// Clear the live connection if it is still `gen`. Returns true when this
/// call was the one that took the live connection down.
async fn drop_conn(shared: &Arc<LinkShared>, gen: u64) -> bool {
    let mut conn = shared.conn.lock().await;
    match conn.as_ref() {
        Some((live_gen, _)) if *live_gen == gen => {
            *conn = None;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;

    /// Records everything the link reports (v1 callback capture).
    #[derive(Default)]
    struct Collector {
        msgs: StdMutex<Vec<Msg>>,
        connected: AtomicUsize,
        down: AtomicUsize,
    }

    impl LinkEvents for Collector {
        fn on_connected(&self) {
            self.connected.fetch_add(1, Ordering::SeqCst);
        }
        fn on_msg(&self, msg: Msg) {
            self.msgs.lock().unwrap().push(msg);
        }
        fn on_down(&self) {
            self.down.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl Collector {
        fn received(&self) -> Vec<Msg> {
            self.msgs.lock().unwrap().clone()
        }
    }

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn make_link(
        device_id: &str,
        role: Role,
        port: u16,
        events: &Arc<Collector>,
    ) -> LinkHandle {
        let link = LinkHandle::new(device_id, role, port, Duration::from_millis(200));
        link.set_events(&(Arc::clone(events) as Arc<dyn LinkEvents>));
        link
    }

    #[tokio::test]
    async fn handshake_and_exchange() {
        let server_ev = Arc::new(Collector::default());
        let server = make_link("server-dev", Role::Server, 0, &server_ev);
        server.start();
        wait_until("server bound", || server.bound_port() != 0).await;

        let client_ev = Arc::new(Collector::default());
        let client = make_link("client-dev", Role::Client, server.bound_port(), &client_ev);
        client.start();

        assert!(client.dial("127.0.0.1").await);
        wait_until("connected", || server.is_connected() && client.is_connected()).await;
        wait_until("both handshook", || {
            server_ev.connected.load(Ordering::SeqCst) >= 1
                && client_ev.connected.load(Ordering::SeqCst) >= 1
        })
        .await;
        assert_eq!(server.last_peer_id().await, "client-dev");
        assert_eq!(client.last_peer_id().await, "server-dev");

        assert!(server.send(Msg::Ping));
        wait_until("client got PING", || {
            client_ev.received().contains(&Msg::Ping)
        })
        .await;
        assert!(client.send(Msg::Pong));
        wait_until("server got PONG", || {
            server_ev.received().contains(&Msg::Pong)
        })
        .await;
    }

    #[tokio::test]
    async fn duplicate_connection_rejected() {
        let server_ev = Arc::new(Collector::default());
        let server = make_link("srv", Role::Server, 0, &server_ev);
        server.start();
        wait_until("server bound", || server.bound_port() != 0).await;
        let port = server.bound_port();

        let first_ev = Arc::new(Collector::default());
        let first = make_link("c1", Role::Client, port, &first_ev);
        first.start();
        assert!(first.dial("127.0.0.1").await);
        wait_until("first connected", || server.is_connected() && first.is_connected()).await;

        // A second client dials; the server must keep only the first
        // connection. The duplicate completes its own local handshake (like
        // v1) but is closed by the server right after admission fails.
        let second_ev = Arc::new(Collector::default());
        let second = make_link("c2", Role::Client, port, &second_ev);
        second.start();
        second.dial("127.0.0.1").await;

        wait_until("duplicate dropped", || {
            second_ev.down.load(Ordering::SeqCst) >= 1
        })
        .await;
        assert_eq!(
            server_ev.connected.load(Ordering::SeqCst),
            1,
            "server must admit only the first connection"
        );
        assert!(first.is_connected(), "original link survives the duplicate");
        assert_eq!(server_ev.down.load(Ordering::SeqCst), 0);
    }
}
