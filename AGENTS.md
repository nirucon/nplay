# Notes for agents working on NPLAY

NPLAY 2.x is Rust. Read `README.md`, `ARCHITECTURE.md` and `docs/PROTOCOL.md`
first; each `src/` module starts with a doc comment listing its API.

Invariants:
- NPLAY is a complete standalone terminal player. It needs no X11, NIRUX or NIRUAMP.
- One backend session has one authoritative state and one MPRIS owner. No double
  audio and no duplicate listening events.
- User data, configuration and secrets are preserved on upgrade (`INSTALL.md`).
  Never delete or rewrite the database or the statistics queue; schema changes are
  additive and backed up.
- Credentials never reach argv or logs (`src/redact.rs`); logs 0600, state 0700.
- Idle means idle: with nothing playing and no client the service and the terminal
  UI use 0 CPU (no polling loops; threads block).
- Control protocol: bump the minor version only for additions and keep existing
  requests byte compatible. `tests/golden/protocol.txt` (recorded from 1.8.2) must
  match `tests/protocol_compat.py` output; extend both together.

Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, the protocol dump, `tests/tui.sh`, `tests/install.sh`, and NIRUAMP's
`tests/xvfb.sh` against the release binary.
