# tspair

Mutual connection tool between two devices over [Tailscale](https://tailscale.com).
Windows system-tray app (Python + [pystray](https://pystray.readthedocs.io/)).

## What it does

- Keeps a tray icon running that shows the pair's exit-node relationship at a glance
  (icon color + tooltip + menu).
- Finds the peer device by shelling out to the `tailscale` CLI (`tailscale status --json`) —
  the CLI is the single source of truth; no HTTP API is used. The peer is **not** stored in
  the config: it is detected on every cycle as the single other device in the tailnet
  (the *Refresh now* tray item re-runs detection immediately).
- Connects the two apps over a plain TCP socket inside the tailnet and keeps it alive
  with heartbeats.
- Manages the **exit-node state machine** so the two devices can never route through
  each other at the same time (that would leave outbound traffic with nowhere to go).

## Exit-node state machine

Each device is always in one of three states:

| State | Meaning | tailscale command behind it |
|---|---|---|
| `none` | no exit-node relationship | — |
| `using_other` | this device routes traffic via the peer | `tailscale set --exit-node=<peer-ip>` |
| `being_used` | the peer routes traffic via this device | `tailscale set --advertise-exit-node=true` |

Rules enforced by the app:

- Becoming `using_other` requires an `REQ_USE` → `GRANT` handshake over the TCP link.
  The peer refuses (`DENY`) if it is itself `using_other` — a routing loop can never form.
- If both users click "use peer" at the same instant, the request with the larger
  `device_id` yields automatically.
- When the routing side drops back to `none`, it broadcasts its state and the peer
  automatically stops advertising (`being_used` → `none`).
- Every state change is broadcast over the link (`STATE` messages), so both tray
  menus always show both sides' states.

## Server / client roles

Both ends run the same app, but each instance has a fixed role in its config:

- `"server"` — listens on the TCP port, never dials.
- `"client"` — dials the peer's Tailscale IP whenever `tailscale status` reports the
  peer as online.

Pick **one server and one client** for the pair. (Which physical device plays which
role doesn't matter; it only decides who dials whom.)

## Setup

1. Install Python 3.11+ and Tailscale on both devices; both devices must be in the
   same tailnet.
2. `pip install -r requirements.txt`
3. Start the app once (`python -m tspair`) — it writes a config template to
   `%APPDATA%\tspair\tspair.json` and exits.
4. Edit the config on **both** devices:

   ```json
   {
     "role": "client",     // "client" here, "server" on the other device
     "port": 52520,
     "device_id": "auto-generated"
   }
   ```

   No peer hostname is configured — the app expects the tailnet to contain
   exactly these two devices and finds the peer on its own.

5. Run `python -m tspair` again. Use the tray menu to
   *Use peer as exit node* / *Stop using peer as exit node*; *Refresh now*
   re-runs peer detection immediately instead of waiting for the next cycle.

### Tailnet-side note

Using a peer as exit node additionally requires the exit node's advertisement to be
approved by the tailnet — either approve it in the Tailscale admin console, or add an
auto-approver ACL for the two devices. The app only runs the CLI commands; it cannot
bypass that approval.

## Tests

`python tests/test_smoke.py` — loopback test that runs two coordinators against each
other with the tailscale CLI mocked (handshake, grant flow, loop guard, teardown).
