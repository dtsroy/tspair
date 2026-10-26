# tspair v1 (Python) — archived

This is the original Python implementation, kept as the reference for the
Rust/Tauri 2 rewrite (see `plan.md` at the repo root). It is no longer
maintained; the Rust app in `src-tauri/` replaces it once feature parity is
verified on the device pair.

- `tspair/` — package: tray app (`app.py`, `tray.py`), coordinator
  (`coordinator.py`), TCP link (`link.py`, `protocol.py`), tailscale CLI
  wrapper (`tscli.py`), config (`config.py`).
- `tests/test_smoke.py` — smoke tests.
- `tspair.json.example` — sample config (the real `tspair.json` lived in the
  project root and is gitignored).

Run (if ever needed again): `pip install -r requirements.txt && python -m tspair`
