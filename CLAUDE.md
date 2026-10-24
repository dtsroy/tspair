# tspair

Mutual connection tool between two devices over [Tailscale](https://tailscale.com).

## Goal

- Keep a persistent presence in the Windows system tray (a tray icon is sufficient for now — no full GUI).
- Detect peer devices that are online by invoking the Tailscale command-line tool (`tailscale` / `tailscale.exe`) rather than any HTTP API.
- Establish a direct connection between the two paired devices over the Tailscale network.

## Tech Stack

- **Language**: Python
- **Tray icon**: [pystray](https://pystray.readthedocs.io/) (single icon + basic menu; not a full UI framework)
- **Device discovery**: `tailscale` CLI (e.g. `tailscale status`, parsed from stdout)

## Current Status

Implemented (see README.md): tray app with TCP link between the two devices, peer
discovery via the `tailscale` CLI, and a 3-state exit-node state machine
(`none` / `using_other` / `being_used`) synchronized over the link with a hard guard
against mutual exit-node routing. Each instance has a configured role: `"server"`
listens, `"client"` dials — exactly one of each per pair.

## Notes / Conventions

- Target platform: Windows 11 (primary working environment).
- The Tailscale CLI is the single source of truth for online-device detection — shell out to it, don't scrape other sources.
- Keep the tray app lightweight; a proper UI may be added later but is out of scope for now.
