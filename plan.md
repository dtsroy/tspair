# tspair — Rewrite Plan (Python → Rust/Tauri 2)

Documented: 2026-10-26. **Status: implemented** (see `src-tauri/` + `src/`;
the Python v1 is archived under `archive/` and remains the behavioral
reference). Deviations from the plan: config persistence uses a plain JSON
file in the per-user config dir instead of `tauri-plugin-store` (same
guarantees, fewer moving parts); `device_id` is persisted on first
generation per §3.10.

---

## 1. Basic Project Info

**tspair** is a mutual-connection tool between exactly two paired devices over
a [Tailscale](https://tailscale.com) network. It lives in the system tray,
detects the peer by shelling out to the `tailscale` CLI (single source of
truth — never HTTP APIs), and maintains a persistent TCP link between the two
devices. Over that link it synchronizes a 3-state exit-node state machine
(`none` / `using_other` / `being_used`) with a hard guard against mutual
exit-node routing (a routing loop would kill connectivity for both).

| | Current (v1) | Target (v2) |
|---|---|---|
| Language | Python 3 | Rust (backend) + Web frontend (Vite + TS) |
| GUI | `pystray` tray icon only | **Tauri 2**: tray + full window UI |
| Target platforms | Windows 11 (primary) | Windows 11 **and macOS** (Linux explicitly out of scope) |
| Packaging | PyInstaller (ad-hoc) | `.exe` installer + signed/notarized `.dmg` via GitHub Actions |
| Footprint | ~80–150 MB | ~5–10 MB binary |

Distribution note: Linux is not required, which removes the AppIndicator /
tray-inconsistency concerns entirely — only two build targets.

---

## 2. Current Logic (to be preserved exactly)

### 2.1 Roles & discovery

- Each device has a configured **role** in `tspair.json`: `"server"` (listens
  on the TCP port, never dials) or `"client"` (dials the peer's Tailscale IP
  when the peer is online). Exactly one of each per pair.
- The peer is **never persisted** — it is re-detected every cycle from
  `tailscale status --json` as "the one other device in the tailnet". If
  `Peer` count ≠ 1, no peer is reported. Both roles need the peer's IP (as
  exit-node target), but only the client dials.
- A discovery loop ticks every `status_interval` (30 s default) or on manual
  refresh; it also records whether the peer advertises itself as an exit node
  (`AdvertisedExitNode`).

### 2.2 TCP link (transport-only)

- Newline-delimited JSON frames over TCP (default port **52520**).
- At most one live connection; duplicates (after HELLO) are rejected.
- Handshake: both sides send `HELLO {id}` immediately; non-HELLO first frames
  are a protocol error; handshake bounded by a 10 s timeout.
- Heartbeat: `PING`/`PONG` every `ping_interval` (15 s default); a failed
  send drops the connection.
- `device_id` is a random UUID generated at first run, used only for
  simultaneous-request tie-breaking.

### 2.3 Wire protocol

| Type | Payload | Meaning |
|---|---|---|
| `HELLO` | `{id}` | identity announcement, first frame |
| `PING` / `PONG` | — | heartbeat |
| `REQ_USE` | — | "I want to route via you" (request permission) |
| `GRANT` | — | provider approved |
| `DENY` | `{reason}` | provider refused |
| `USE_FAILED` | `{reason}` | requester granted but failed to apply the exit node |
| `STATE` | `{state}` | broadcast of sender's current ExitState |

### 2.4 Exit-node state machine (per device)

States: `NONE` → independent; `USING_OTHER` → this device routes via the peer
(`tailscale set --exit-node=peer`); `BEING_USED` → peer routes via this device
(`tailscale set --advertise-exit-node=true`).

Invariant: **at most one side may be `USING_OTHER` at any time** — both at
once is a routing loop. Guarded transitions:

- Entering `USING_OTHER` requires the `REQ_USE` → `GRANT`/`DENY` handshake
  (10 s timeout). The provider refuses if it is itself `USING_OTHER`.
- Simultaneous requests (both click at once): the side with the **larger
  `device_id` yields** (unknown peer id sorts last).
- `use_peer` applies with retries: 3 s settle delay after `GRANT` (peer's
  advertisement needs to propagate), up to 3 attempts total, 2 s between
  retries. If all fail → send `USE_FAILED` so the provider un-advertises.
- When the user side drops back to `NONE` (STATE message), the provider
  automatically stops advertising → `NONE` too.
- Provider: `REQ_USE` → advertise via CLI → `state = BEING_USED` → `GRANT`.
  Re-`REQ_USE` while already `BEING_USED` re-grants idempotently.

### 2.5 Tailscale CLI operations used

- `tailscale status --json` → peer list, online state, `AdvertisedExitNode`,
  `ExitNodeStatus`
- `tailscale set --exit-node=<ip>` / `--exit-node=` (empty = clear)
- `tailscale set --advertise-exit-node=true|false`

All with 15 s timeouts and stderr captured into error messages.

---

## 3. Things to Be Aware Of (porting gotchas & current-code notes)

### macOS-specific (the new hard requirement)

1. **Tailscale CLI path differs.** App Store Tailscale puts the binary at
   `/Applications/Tailscale.app/Contents/MacOS/Tailscale` (not on PATH);
   standalone installs use `/usr/local/bin/tailscale`. The Rust wrapper must
   resolve in order: `PATH` → `.app` bundle path → known install locations
   (~30 lines, resolves once at startup).
2. **Hide the Dock icon**: `app.set_activation_policy(Accessory)` at startup.
3. **Menu bar icons must be monochrome template images**
   (`icon_as_template: true`) to adapt to light/dark menu bars. Encode the
   3 exit-node states via icon *shape* on macOS; keep colored state icons on
   Windows.
4. **Firewall prompt**: the server role's listening socket triggers a one-time
   macOS "Allow incoming connections" dialog. The client must tolerate the
   connection being refused during that window (re-dial loop already does).
5. **Gatekeeper / signing**: unsigned apps are blocked on other Macs
   (bypassable per-machine with `xattr -cr`). Decide: pay for Apple Developer
   ($99/yr) + notarization, or live with the bypass for the two paired
   devices. **Open decision — owner to choose.**
6. **Cannot cross-compile macOS from Windows**: build via GitHub Actions
   `macos-latest` (free for public repos; minutes are cheap even at the 10×
   private-repo multiplier) with `tauri-apps/tauri-action`, or build on the
   Mac directly.

### Windows-specific

7. Hide console windows when spawning the CLI
   (`CREATE_NO_WINDOW` equivalent in Rust: `creation_flags(0x08000000)` on
   Windows via `std::os::windows::process::CommandExt` / tokio equivalent).

### Current-code quirks to NOT carry over

8. `coordinator.py` `stop_using_peer` contains **unreachable dead code** after
   its first `return` (a duplicated clear/broadcast block, lines 174–181).
   Do not port it.
9. Config currently lives in the **project root** on all platforms
   (`config.py` → `Path(__file__)/..`). For a packaged GUI app this must move
   to a proper per-user config dir — use `tauri-plugin-store` instead
   (opens the question of migrating existing `tspair.json`; the file only
   holds `role`, `port`, `device_id`, two intervals — cheap to re-create or
   auto-import).
10. `device_id` regeneration: `config.py` fills in a UUID if missing but does
    not persist it back. In v2, persist on first generation.

### Tauri-specific

11. First `cargo build` is slow (several minutes of deps); subsequent builds
    are fast. Two toolchains in the repo (`cargo` + `npm`).
12. Single-instance is mandatory (`tauri-plugin-single-instance`) — a second
    launch must not create a second tray icon / second TCP listener.
13. A Rust environment is already present on the dev machine — no provisioning
    needed.

---

## 4. New Rust Project Framework

### 4.1 Repository layout

```
tspair/
├── src/                          # frontend (Vite + TypeScript, no framework)
│   ├── index.html
│   ├── main.ts                   # event listeners + UI rendering
│   └── style.css
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json           # window config, tray config, bundle targets
│   ├── icons/                    # app icons + tray-state icons (see 4.5)
│   │   ├── tray-idle.png         # NONE (green on Win / shape A on macOS)
│   │   ├── tray-using.png        # USING_OTHER
│   │   ├── tray-serving.png      # BEING_USED
│   │   └── tray-offline.png      # peer offline / link down
│   └── src/
│       ├── main.rs               # tauri::Builder wiring, tray setup,
│       │                         #   activation policy (macOS), plugin registration
│       ├── config.rs             # load/save via tauri-plugin-store, first-run
│       │                         #   template, device_id persistence
│       ├── tscli.rs              # tailscale CLI wrapper (path resolver per-OS,
│       │                         #   status parsing, set/clear exit node, advertise)
│       ├── protocol.rs           # frame encode/decode: newline-delimited JSON,
│       │                         #   message enum (serde tagged on "type")
│       ├── link.rs               # tokio TCP link: listen/dial, HELLO handshake,
│       │                         #   single-connection admission, heartbeat, reconnect
│       ├── coordinator.rs        # 3-state machine + handshake guard (core logic)
│       ├── discovery.rs          # periodic `status --json` polling loop
│       └── commands.rs           # #[tauri::command] handlers (UI → backend IPC)
├── .github/workflows/
│   ├── build-windows.yml         # tauri-action → .exe/.msi artifact
│   └── build-macos.yml           # tauri-action → .app/.dmg (notarize if signed)
├── plan.md
└── (legacy Python kept under legacy/ or removed after parity — TBD)
```

### 4.2 Backend module responsibilities

- **`tscli.rs`** — resolves the CLI path once (PATH → macOS bundle paths →
  known locations), wraps `tokio::process::Command` with 15 s timeouts and
  `CREATE_NO_WINDOW` on Windows. Types: `PeerEntry { hostname, dns_name,
  ip, online, advertised_exit_node }`; `detect_peer() -> Option<PeerInfo>`
  preserving the "exactly one peer" rule. Errors: `TailscaleError` enum
  (NotFound / Timeout / Failed{stderr}).
- **`protocol.rs`** — `#[derive(Serialize, Deserialize)]` enum `Msg` with
  `#[serde(tag = "type")]` variants: `Hello{id}`, `Ping`, `Pong`, `ReqUse`,
  `Grant`, `Deny{reason}`, `UseFailed{reason}`, `State{state}`. `Codec`:
  `tokio_util::codec::Decoder` splitting on `\n`, skipping malformed lines
  (same lenient behavior as v1).
- **`link.rs`** — mirrors `PeerLink`: role-aware (server listens / client
  dials), HELLO handshake with 10 s bound, duplicate-connection rejection,
  ping loop, drop on send failure. Owned by a tokio task; state in
  `Arc<Mutex<LinkState>>` exposing `is_connected()`, `send(Msg)`,
  `last_peer_id`. Reconnection stays in `discovery.rs` (client dials on each
  tick when peer online), matching v1.
- **`coordinator.rs`** — the state machine, `Arc<Mutex<...>>`-guarded:
  `state: ExitState`, `peer_state: Option<ExitState>`, `peer_ip`,
  `pending_out`, a oneshot/watch channel per in-flight `REQ_USE` for the
  `GRANT`/`DENY` reply (replaces Python's `threading.Event`), and the same
  timing constants (10 s request timeout, 3 s settle, 2 retries, 2 s backoff).
  Transitions and the loop-guard / tie-break rules exactly per §2.4.
- **`discovery.rs`** — tokio interval loop (30 s default) + manual-refresh
  notify channel; updates peer IP / advertised flag, triggers client dials,
  emits UI events.
- **`commands.rs`** — the IPC surface (below).

### 4.3 IPC surface (frontend ↔ backend)

Tauri commands (`invoke` from TS):

| Command | Maps to |
|---|---|
| `get_status` | link state + my state + peer state + peer info → initial render |
| `use_peer` | `Coordinator::use_peer` → `(ok, message)` |
| `stop_using_peer` | `Coordinator::stop_using_peer` → `(ok, message)` |
| `refresh_now` | poke discovery loop |
| `get_config` / `set_config` | read/store config (role, port, intervals) |

Backend → frontend events (`app.emit`), listened with `@tauri-apps/api/event`:

| Event | Payload | UI reaction |
|---|---|---|
| `state-changed` | `{mine, peer, connected}` | update dashboard + tray icon + tray menu |
| `peer-info` | `{hostname, ip, online, advertised}` | peer card update |
| `notify` | `{title, body}` | toast / OS notification |

### 4.4 Plugins (all first-party, all support Windows + macOS)

- `tauri-plugin-single-instance` — mandatory (§3.12)
- `tauri-plugin-autostart` — launch at login toggle in the UI
- `tauri-plugin-store` — config persistence (replaces project-root JSON)
- `tauri-plugin-notification` — peer-online / state-change toasts

### 4.5 Tray & window behavior

- Tray always present: menu shows status line (`peer connected | Us: … |
  Peer: …`), "Use peer as exit node", "Stop using peer", "Refresh now",
  "Open dashboard", "Quit".
- Icon swaps on `state-changed`: 4 states (§4.1 icons). Windows: colored;
  macOS: `icon_as_template` monochrome with shape-distinct states.
- Window: close button **hides to tray** (does not exit); Quit only from the
  tray menu. On macOS also `set_activation_policy(Accessory)` so no Dock icon
  appears.
- Window UI (dashboard): connection status, both devices' states, peer info
  card, the two action buttons, config (role/port read-only + intervals),
  and a log/status view. Plain Vite + TS is sufficient — no framework needed
  for this size.

### 4.6 Build & CI

- Local dev on Windows: `cargo tauri dev` (needs Rust + Node; Rust already
  present).
- CI: two workflows using `tauri-apps/tauri-action` —
  `build-windows.yml` (windows-latest → installer artifact) and
  `build-macos.yml` (macos-latest → `.dmg` artifact; signing/notarization
  secrets wired in only if the owner opts into the $99/yr account).
- macOS builds cannot be produced from Windows — CI or a Mac is required (§3.6).

### 4.7 Migration order (risk-minimized)

1. **`protocol.rs` + `tscli.rs`** — pure logic, no GUI. Unit tests: frame
   round-trips, CLI path resolution, `status --json` parsing against a
   captured fixture.
2. **`link.rs`** — testable with two in-process tokio peers (HELLO exchange,
   duplicate rejection, heartbeat timeout).
3. **`coordinator.rs`** — port the state machine with a mocked `tscli` trait;
   unit-test the loop guard, simultaneous-request tie-break, auto-unwind,
   `USE_FAILED` path. This is the heart of the app and must match §2.4
   exactly.
4. **Tauri shell**: tray, single-instance, hide-to-tray, activation policy.
5. **`discovery.rs` + commands + events** — wire backend to tray.
6. **Window UI** last — dashboard, actions, config.
7. Parity check against the Python app (run both on the pair, verify all
   handshake paths), then retire the Python code.

### 4.8 Key dependency list (Cargo)

`tauri` (tray-icon feature), `tokio` (full), `tokio-util` (codec),
`serde`/`serde_json`, `async-trait` (mockable tscli), `log` + `env_logger`/
`tauri-plugin-log`, plus the four Tauri plugins from §4.4.
