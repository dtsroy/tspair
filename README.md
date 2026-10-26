# tspair

Mutual connection tool between two paired devices over [Tailscale](https://tailscale.com).

tspair lives in the system tray, detects the peer by shelling out to the
`tailscale` CLI (single source of truth — never HTTP APIs), and maintains a
persistent TCP link between the two devices. Over that link it synchronizes a
3-state exit-node state machine (`none` / `using_other` / `being_used`) with a
hard guard against mutual exit-node routing (a routing loop would kill
connectivity for both).

## How it works

- **Roles**: each device has a configured role — `"server"` (listens on the
  TCP port, never dials) or `"client"` (dials the peer's Tailscale IP when
  the peer is online). Exactly one of each per pair.
- **Discovery**: the peer is never persisted; it is re-detected every cycle
  from `tailscale status --json` as "the one other device in the tailnet".
- **Link**: newline-delimited JSON frames over TCP (default port **52520**),
  one live connection, `HELLO` handshake, `PING`/`PONG` heartbeat.
- **Exit-node guard**: entering `using_other` requires a
  `REQ_USE` → `GRANT`/`DENY` handshake; the provider refuses if it is itself
  `using_other`, so a routing loop can never form. Simultaneous requests are
  broken by `device_id` (the larger id yields).

## States

| State | Meaning | Tailscale action |
|---|---|---|
| `none` | independent | — |
| `using_other` | this device routes via the peer | `tailscale set --exit-node=<peer>` |
| `being_used` | the peer routes via this device | `tailscale set --advertise-exit-node=true` |

## Build & run (development)

Requires Rust, Node.js and pnpm.

```bash
pnpm install
pnpm tauri dev      # or: cargo run --manifest-path src-tauri/Cargo.toml (no UI)
```

Rust tests (state machine, link, protocol, config, CLI parsing):

```bash
cd src-tauri && cargo test
```

## Configuration

Stored per-user (created on first run with a generated `device_id`):
`%APPDATA%/dev.dtsroy.tspair/config.json` on Windows,
`~/Library/Application Support/dev.dtsroy.tspair/config.json` on macOS.
Set `TSPAIR_CONFIG=<path>` to override (handy in development).

```json
{
  "role": "client",       // the peer's device must use "server"
  "port": 52520,
  "device_id": "<uuid>",
  "ping_interval": 15.0,
  "status_interval": 30.0
}
```

## Packaging

GitHub Actions build signed installers from this repo: `build-windows.yml`
(NSIS/MSI) and `build-macos.yml` (universal `.dmg`). macOS releases are
unsigned unless Apple Developer secrets are configured (see plan.md §3.5).

The original Python implementation is archived under
[archive/](archive/README.md).
