//! Thin wrapper around the `tailscale` CLI — the single source of truth
//! for peer discovery and exit-node state. Never any HTTP API.

use async_trait::async_trait;

use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const CLI_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum TailscaleError {
    NotFound,
    Timeout(String),
    Failed(String),
}

impl fmt::Display for TailscaleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TailscaleError::NotFound => write!(f, "tailscale CLI not found"),
            TailscaleError::Timeout(cmd) => write!(f, "tailscale {cmd} timed out"),
            TailscaleError::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for TailscaleError {}

/// One *other* device in the tailnet (from `status --json`).
#[derive(Debug, Clone, PartialEq)]
pub struct PeerEntry {
    pub hostname: String,
    pub dns_name: String,
    pub ip: String, // peer's Tailscale IP (100.x.y.z)
    pub online: bool,
    pub advertised_exit_node: bool,
}

/// All operations the app needs from the CLI. An enum-free trait so tests
/// can mock the state machine without shelling out.
#[async_trait]
pub trait TsCli: Send + Sync {
    async fn list_peers(&self) -> Result<Vec<PeerEntry>, TailscaleError>;
    async fn set_exit_node(&self, ip: &str) -> Result<(), TailscaleError>;
    async fn clear_exit_node(&self) -> Result<(), TailscaleError>;
    async fn advertise_exit_node(&self, on: bool) -> Result<(), TailscaleError>;
}

/// The real CLI wrapper: resolves the binary once, runs with timeouts.
/// A missing binary is not fatal at startup — every operation then fails
/// with `NotFound`, which discovery reports like any other CLI error.
pub struct Cli {
    path: Option<PathBuf>,
}

impl Cli {
    /// Resolve the tailscale binary, once, at startup.
    ///
    /// Order: `PATH` → macOS .app bundle → known install locations
    /// (App Store Tailscale on macOS is not on PATH at all).
    pub fn resolve() -> Self {
        Self {
            path: resolve_cli_path(),
        }
    }

    /// Run the CLI with args, capturing stdout; 15 s timeout, stderr folded
    /// into errors. On Windows the spawned console window is hidden.
    async fn run(&self, args: &[&str]) -> Result<String, TailscaleError> {
        let path = match &self.path {
            Some(p) => p,
            None => return Err(TailscaleError::NotFound),
        };
        let joined = args.join(" ");
        let mut cmd = Command::new(path);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TailscaleError::NotFound
            } else {
                TailscaleError::Failed(format!("tailscale {joined} failed to spawn: {e}"))
            }
        })?;
        let out = match tokio::time::timeout(CLI_TIMEOUT, child.wait_with_output()).await {
            Err(_) => return Err(TailscaleError::Timeout(joined)),
            Ok(Err(e)) => return Err(TailscaleError::Failed(format!("tailscale {joined}: {e}"))),
            Ok(Ok(out)) => out,
        };
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            let detail = String::from_utf8_lossy(&out.stderr);
            let detail = detail.trim();
            let detail = if detail.is_empty() {
                stdout.trim()
            } else {
                detail
            };
            return Err(TailscaleError::Failed(format!(
                "tailscale {joined} failed: {detail}"
            )));
        }
        Ok(stdout)
    }
}

#[async_trait]
impl TsCli for Cli {
    async fn list_peers(&self) -> Result<Vec<PeerEntry>, TailscaleError> {
        let out = self.run(&["status", "--json"]).await?;
        parse_status(&out)
    }

    async fn set_exit_node(&self, ip: &str) -> Result<(), TailscaleError> {
        self.run(&["set", &format!("--exit-node={ip}")])
            .await
            .map(drop)
    }

    async fn clear_exit_node(&self) -> Result<(), TailscaleError> {
        self.run(&["set", "--exit-node="]).await.map(drop)
    }

    async fn advertise_exit_node(&self, on: bool) -> Result<(), TailscaleError> {
        self.run(&["set", &format!("--advertise-exit-node={}", if on { "true" } else { "false" })])
            .await
            .map(drop)
    }
}

/// PATH → macOS .app bundle → known install locations.
fn resolve_cli_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "tailscale.exe" } else { "tailscale" };

    // 1. PATH
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 2. Known locations, per platform.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".into());
        candidates.push("/usr/local/bin/tailscale".into());
        candidates.push("/opt/homebrew/bin/tailscale".into());
    } else if cfg!(windows) {
        if let Some(pf) = std::env::var_os("ProgramFiles") {
            candidates.push(Path::new(&pf).join("Tailscale").join("tailscale.exe"));
        }
        if let Some(pf) = std::env::var_os("ProgramFiles(x86)") {
            candidates.push(Path::new(&pf).join("Tailscale").join("tailscale.exe"));
        }
        if let Some(lb) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(
                Path::new(&lb)
                    .join("Microsoft")
                    .join("WindowsApps")
                    .join("tailscale.exe"),
            );
        }
    } else {
        candidates.push("/usr/bin/tailscale".into());
        candidates.push("/usr/local/bin/tailscale".into());
    }
    candidates.into_iter().find(|c| c.is_file())
}

/// The one other device in the tailnet, from `tailscale status --json`.
///
/// The pair setup assumes this device plus exactly one peer; returns None
/// when that doesn't hold (no other device, or more than one).
pub fn detect_peer(peers: Vec<PeerEntry>) -> Option<PeerEntry> {
    if peers.len() != 1 {
        return None;
    }
    peers.into_iter().next()
}

/// Parse the output of `tailscale status --json` into the peer list.
pub fn parse_status(json: &str) -> Result<Vec<PeerEntry>, TailscaleError> {
    let data: Value = serde_json::from_str(json)
        .map_err(|e| TailscaleError::Failed(format!("could not parse `status --json` output: {e}")))?;
    let mut peers = Vec::new();
    if let Some(map) = data.get("Peer").and_then(|p| p.as_object()) {
        for p in map.values() {
            let ips: Vec<&str> = p
                .get("TailscaleIPs")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                .unwrap_or_default();
            peers.push(PeerEntry {
                hostname: p.get("HostName").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                dns_name: p
                    .get("DNSName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim_end_matches('.')
                    .to_string(),
                ip: ips.first().copied().unwrap_or("").to_string(),
                online: p.get("Online").and_then(|v| v.as_bool()).unwrap_or(false),
                advertised_exit_node: p
                    .get("AdvertisedExitNode")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            });
        }
    }
    Ok(peers)
}

/// True if this device currently routes via any exit node. Best-effort:
/// any error means "no" (v1 behaved the same). Kept from v1 for parity;
/// currently only exercised by tests.
#[allow(dead_code)]
pub fn exit_node_status_from(json: &str) -> bool {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|data| {
            data.get("ExitNodeStatus")
                .map(|s| !s.is_null() && s.as_object().map(|o| !o.is_empty()).unwrap_or(true))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixture shaped like a real `tailscale status --json` (abridged).
    const FIXTURE_ONE_PEER: &str = r#"{
      "Version": "1.80.0", "TailscaleIPs": ["100.64.0.1", "fd7a:115c:a1e0::1"],
      "Peer": {
        "key-1": {
          "HostName": "work-laptop", "DNSName": "work-laptop.tail1234.ts.net.",
          "TailscaleIPs": ["100.101.102.103", "fd7a:115c:a1e0::ab"],
          "Online": true, "AdvertisedExitNode": true
        }
      }
    }"#;

    #[test]
    fn parse_status_single_peer() {
        let peers = parse_status(FIXTURE_ONE_PEER).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0],
            PeerEntry {
                hostname: "work-laptop".into(),
                dns_name: "work-laptop.tail1234.ts.net".into(), // trailing dot stripped
                ip: "100.101.102.103".into(),
                online: true,
                advertised_exit_node: true,
            }
        );
        let peer = detect_peer(peers).unwrap();
        assert_eq!(peer.ip, "100.101.102.103");
    }

    #[test]
    fn detect_peer_requires_exactly_one() {
        assert!(detect_peer(vec![]).is_none(), "no peer -> None");
        let two = vec![
            PeerEntry {
                hostname: "a".into(),
                dns_name: String::new(),
                ip: "100.0.0.1".into(),
                online: true,
                advertised_exit_node: false,
            },
            PeerEntry {
                hostname: "b".into(),
                dns_name: String::new(),
                ip: "100.0.0.2".into(),
                online: false,
                advertised_exit_node: false,
            },
        ];
        assert!(detect_peer(two).is_none(), "more than one peer -> None");
    }

    #[test]
    fn parse_status_empty_and_missing_peer_map() {
        assert!(parse_status("{}").unwrap().is_empty());
        assert!(parse_status(r#"{"Peer": null}"#).unwrap().is_empty());
        assert!(parse_status("not json at all").is_err());
    }

    #[test]
    fn exit_node_status_variants() {
        assert!(!exit_node_status_from("{}"));
        assert!(!exit_node_status_from(r#"{"ExitNodeStatus": null}"#));
        assert!(exit_node_status_from(
            r#"{"ExitNodeStatus": {"TailscaleIPs": ["100.1.2.3"]}}"#
        ));
    }
}
