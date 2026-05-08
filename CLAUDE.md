# CLAUDE.md

Notes for Claude Code agents working in this repo. Keep this file short and
high-signal — it is loaded into context every session.

## What this is

`dnp3_master_io_v3` is a Rust DNP3 master / fleet monitor for lab and
engineering-validation use. It connects over TCP to one or more outstations,
Class-0 polls them once per second, and serves a tiny HTML UI + REST API on
port `9002`. RTUs are configured in `rtus.toml`.

Licensing: uses the Step Function I/O `dnp3` crate under their internal-use
clarification — internal demo, training, validation, lab. Not for shipping
products or customer-facing sales without a commercial license.

## Hard design rule

> **Telemetry is authoritative truth.**

Snapshot fields (`bi`, `ai0`) are only mutated inside `MasterReadHandler` on a
successful Class 0 read, or inside `mark_success` / `mark_failure`. Operator
commands are intent only — they never optimistically write into the snapshot.
The UI surfaces "Requested" alongside "Actual" so the operator can see when
the field hasn't caught up. Don't break this contract.

## Layout

```
src/main.rs            runtime, polling, REST/UI, command queueing
src/master/model.rs    RtuSnapshot — the public shape exposed via /api/rtus
ui/index.html          single-page UI; no build step
rtus.toml              fleet config (master_addr + [[rtu]] entries)
deploy/systemd/        dnp3_master_io_v3.service
```

## Key behaviors to know

- **One TCP per endpoint, not per RTU.** RTUs sharing an `endpoint` share a
  single `MasterChannel` with multiple `AssociationHandle`s. This is the
  terminal-server fix: duplicate sockets to the same `ip:port` get muxed
  together by some serial-over-IP devices and corrupt frames.
- `LinkErrorMode::Discard` (not `Close`) on shared channels — one bad frame
  must not tear down the whole TCP for everyone behind a terminal server.
- First-poll across associations on a shared channel is staggered evenly
  across `POLL_INTERVAL_SECS` via `tokio::time::interval_at`.
- A per-endpoint parent task holds the `MasterChannel` alive forever
  (`std::future::pending::<()>().await`); per-association child tasks own
  only the `AssociationHandle`. Dropping the channel kills the TCP task.

## REST surface

- `GET  /api/rtus`             — full snapshot list
- `GET  /api/health`           — `{total, online, offline, rtus:[…]}`, always 200
- `POST /api/rtus/{id}/bi/{i}/{true|false}` — `200`/`404 UNKNOWN_RTU`/`503 QUEUE_FULL`
- `POST /api/rtus/{id}/ai0/{f32}`           — same status code shape
- `GET  /`                     — serves `ui/index.html`

## Run / build

```
cargo build --release
./target/release/dnp3_master_io_v3
```

Logging: `MASTER_LOG=debug|trace|warn|error cargo run` (default `info`).
Per-point console prints: `MASTER_PRINT_AI=1` and/or `MASTER_PRINT_BI=1`.

systemd unit lives at `deploy/systemd/dnp3_master_io_v3.service`. After any
path or binary-name change, the user must `sudo cp` it to
`/etc/systemd/system/`, `daemon-reload`, then start. Don't try to edit unit
files under `/etc/` — no sudo here.

## rtus.toml

```toml
master_addr = 1            # optional, default 1

[[rtu]]
id        = "rtu-a"        # must be unique
endpoint  = "10.0.0.5:20000"
rtu_addr  = 1024
bi_count  = 8              # optional, default 3
```

Two RTUs with the same `endpoint` and different `rtu_addr` will share one
TCP connection. Different endpoints get their own channel.

## Conventions

- Quiet by default; library decode is `DecodeLevel::nothing()`. Our handler
  logs only on value change.
- Add new state to `RtuSnapshot` — it's the single source of truth and is
  serialised straight out via `/api/rtus`.
- The runtime is `tokio::runtime::Builder::new_current_thread` + `LocalSet`.
  Anything spawned under `tokio::task::spawn_local` does not need to be
  `Send`. The `dnp3` association handles are `!Send` so this matters.
- Snapshots use `parking_lot::RwLock`; keep critical sections short and never
  call `.await` while holding the lock.
- `mpsc::channel(64)` is per-RTU; commands beyond capacity surface as
  `503 QUEUE_FULL` to the caller.

## When making changes

- Run `cargo build --release` and confirm zero warnings before committing.
- The systemd service was inactive last time it was checked — rebuild does
  not auto-restart anything; the user starts the service manually.
- Don't bind extra sockets to port 9002; warp serves everything from there.

## Out of scope (don't add without explicit ask)

- Direct serial DNP3 master (TCP only today).
- Authentication on the REST/UI surface (lab-only deployment).
- Unsolicited DNP3 reporting (we are 100% Class 0 polling on purpose).
- Persistent history / time-series storage.
