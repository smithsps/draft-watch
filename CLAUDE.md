# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run

Cargo and rustup are at `%USERPROFILE%\.cargo\bin` — ensure that's on PATH.

```
cargo build                  # debug build
cargo build --release        # release build (also uncomment #![windows_subsystem = "windows"] in main.rs)
cargo run                    # run with console output
cargo check                  # fast type-check without linking
```

Toolchain: `stable-x86_64-pc-windows-gnu` (MinGW linker). Managed via mise (`.mise.toml` pins `rust = "1.96.0"`).

## Architecture

Two-thread model:

- **Main thread** — Windows message pump (`PeekMessage` loop at 50ms) + system tray icon. No async.
- **Background thread** — `tokio` current-thread runtime inside a `LocalSet`. Owns all async work: LCU WebSocket, SQLite storage, HTTP upload.

Communication from background → main is via `std::sync::mpsc`. The background thread sends `AppMsg::Lcu(LcuEvent)` variants to drive tray state changes.

### Data flow

1. `lcu::monitor` polls for `%LEAGUE_DIR%\lockfile` every 5s (process scan + common path fallback).
2. On find: connects `wss://127.0.0.1:{port}/` with Basic auth (`riot:{token}`) and self-signed cert accepted.
3. Subscribes to `OnJsonApiEvent_lol-champ-select_v1_session` (LCU WAMP opcode 5).
4. Incoming events (opcode 8): `Update`/`Create` → store latest snapshot; `Delete` or `timer.phase == "GAME_STARTING"` → commit session.
5. Commit: serialize raw `serde_json::Value` → SQLite (`%LOCALAPPDATA%\ChampSelect\sessions.db`) → POST to `upload_url` if configured.

### Key design decisions

- Sessions are stored as **raw LCU JSON** — no lossy struct round-trip. This preserves all fields the stats API might need.
- `rusqlite::Connection` is `!Send`; kept on the background thread and never crossed to main. `LocalSet` + `spawn_local` avoids the `Send` bound entirely.
- Upload failures are non-fatal: session stays in SQLite with `uploaded=0` for future retry (retry not yet wired up — see `ideas.md`).

### Config

Auto-created at `%APPDATA%\ChampSelect\config.toml` on first run:

```toml
# upload_url = "https://your-site.com/api/sessions"
# upload_api_key = "your-key"
# league_path = "C:\\Riot Games\\League of Legends"   # override if non-standard
```

`upload_api_key` is sent as `X-Api-Key` header. `league_path` is defined but not yet wired into lockfile discovery (see `ideas.md`).
