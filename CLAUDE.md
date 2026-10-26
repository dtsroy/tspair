# tspair

Mutual connection tool between two devices over [Tailscale](https://tailscale.com).

## Goal

- Persistent presence in the Windows system tray (plus a dashboard window).
- Detect peer devices online by invoking the Tailscale command-line tool
  (`tailscale` / `tailscale.exe`) rather than any HTTP API.
- Establish a direct TCP connection between the two paired devices over the
  Tailscale network and synchronize a 3-state exit-node state machine with a
  hard guard against mutual exit-node routing.

## Tech Stack (v2 — Rust rewrite, see plan.md)

- **Backend**: Rust + Tauri 2 (`src-tauri/`), tokio, tokio-util codec
- **Frontend**: Vite + TypeScript, no framework (`src/`), pnpm for packages
- **Device discovery**: `tailscale` CLI, shelled out with 15 s timeouts
- **Plugins**: single-instance (mandatory), autostart, notification, log
- **Packaging**: GitHub Actions → NSIS/MSI (Windows) and universal `.dmg` (macOS)

## Layout

- `src-tauri/src/protocol.rs` — wire protocol (newline-delimited JSON, serde-tagged)
- `src-tauri/src/link.rs` — TCP link: server listens / client dials, HELLO
  handshake (10 s bound), single-connection admission, ping heartbeat
- `src-tauri/src/coordinator.rs` — the 3-state exit-node machine
  (`none`/`using_other`/`being_used`) + REQ_USE/GRANT/DENY handshake guard.
  Invariant: at most one side may be `using_other` (larger device_id yields
  on simultaneous requests)
- `src-tauri/src/tscli.rs` — CLI wrapper (PATH → .app bundle → known paths),
  mockable via the `TsCli` trait
- `src-tauri/src/discovery.rs` — 30 s polling loop + manual refresh
- `src-tauri/src/config.rs` — per-user config dir, first-run template,
  device_id persisted on first generation
- `src-tauri/src/commands.rs` — `#[tauri::command]` IPC + event payloads
- `src/` — dashboard UI (status, peer card, actions, config, log)
- `archive/` — the original Python implementation (v1, reference only)
- `plan.md` — the rewrite plan (logic spec §2 is normative)

## Notes / Conventions

- Target platforms: Windows 11 (primary) and macOS (Linux out of scope).
- The Tailscale CLI is the single source of truth for online-device
  detection — shell out to it, don't scrape other sources.
- Keep the wire protocol byte-compatible with v1 (see
  `protocol.rs::tests::wire_format_matches_v1`).
- Frontend package management is pnpm (`pnpm install`, `pnpm tauri dev`).
- Rust tests: `cd src-tauri && cargo test` (state machine uses a mocked CLI
  over real loopback TCP).
