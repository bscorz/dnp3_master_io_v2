# dnp3_master_io_v3 — Windows deployment

Cross-compiled `x86_64-pc-windows-gnu` build of the v3 master/fleet monitor.
Drop this folder onto the Windows server, configure `rtus.toml`, and either
double-click the `.exe` or install it as a service via NSSM.

## Folder contents

```
dnp3_master_io_v3.exe     the master service binary (PE32+ console, ~14 MB)
ui\index.html             single-page UI (served at  /  )
rtus.toml.example         sample fleet config — copy to rtus.toml and edit
install_service.bat       installs the .exe as a Windows service via NSSM
uninstall_service.bat     stops and removes the service
README.md                 this file
```

The binary expects to find `rtus.toml` and `ui\index.html` **relative to its
current working directory**. Always launch it from this folder (or set the
working directory in your service definition — `install_service.bat` does
this for you).

## Prerequisites

- Windows Server 2019 or newer, x64.
- TCP outbound to the outstation/terminal-server endpoints (typically
  `:20000`). TCP inbound on `9002` from whoever needs the UI/API.
- For service install: [NSSM](https://nssm.cc/download) — drop `nssm.exe`
  into this folder (or anywhere on `PATH`).

No Visual C++ runtime is required — the GNU toolchain links statically.

## First run (interactive)

1. Copy `rtus.toml.example` to `rtus.toml` and edit endpoints / `rtu_addr`s.
   Optional knobs in the same file:
   - `poll_interval_ms` (file level, default `1000`) — Class 0 poll cadence.
   - `offline_after_ms` (file level, default `10000`) — mark RTU offline
     after this gap with no successful poll.
   - `poll_interval_ms` under any `[[rtu]]` — per-RTU cadence override.
2. Open a `cmd.exe` here and run:
   ```
   dnp3_master_io_v3.exe
   ```
3. Browse to `http://<server-ip>:9002/`.
4. Health check: `http://<server-ip>:9002/api/health`.

Optional environment variables (set in the shell before launching, or in the
NSSM config):

| Var                 | Effect                                              |
|---------------------|-----------------------------------------------------|
| `MASTER_LOG`        | `error` / `warn` / `info` (default) / `debug` / `trace` |
| `MASTER_PRINT_AI=1` | print every AI0 read to the console                 |
| `MASTER_PRINT_BI=1` | print every BI read to the console                  |
| `RTUS_FILE`         | path to a config file other than `rtus.toml`        |

Poll cadence is **not** an environment variable — set it in `rtus.toml`
(see above). Edits require a service restart.

## Firewall

PowerShell, run as Administrator:

```powershell
New-NetFirewallRule -DisplayName "DNP3 Master UI" `
  -Direction Inbound -Protocol TCP -LocalPort 9002 -Action Allow
```

Restrict `-RemoteAddress` to your engineering subnet if the server is on a
shared network — the REST API is unauthenticated by design (lab use).

## Install as a Windows service (NSSM)

1. Download `nssm.exe` from <https://nssm.cc/download> and place it in this
   folder (or anywhere on `PATH`).
2. Open `cmd.exe` **as Administrator**, `cd` into this folder, and run:
   ```
   install_service.bat
   ```
3. Verify:
   ```
   sc query dnp3_master_io_v3
   ```
4. The service starts automatically on boot. Logs go to
   `logs\stdout.log` and `logs\stderr.log` next to the binary, rotated at
   10 MB.

To remove:

```
uninstall_service.bat
```

### Why NSSM and not `sc.exe` directly?

The binary is a plain console app — it does not implement the Windows
Service Control Manager protocol. `sc create` would launch it, but Windows
would mark the service as "failed to respond" after 30 seconds. NSSM wraps
the process, manages restarts, and gives us stdout/stderr capture for free.

## Updating

1. Stop the service: `net stop dnp3_master_io_v3` (or skip if running
   interactively).
2. Replace `dnp3_master_io_v3.exe` with the new build.
3. Start: `net start dnp3_master_io_v3`.

`rtus.toml` is read once at startup — edits require a service restart.

## Troubleshooting

- **Service starts then stops immediately.** Check `logs\stderr.log`.
  Almost always either `rtus.toml` is missing/invalid or port 9002 is in
  use. `netstat -ano | findstr :9002` will show the offender.
- **UI loads but RTUs all show offline.** Confirm TCP reachability from
  the server: `Test-NetConnection 10.0.0.5 -Port 20000`. Check `MASTER_LOG=debug`.
- **Two RTUs at the same `endpoint` corrupting each other's frames.** That
  is the exact bug v3's shared-channel-per-endpoint fix exists for. Make
  sure both RTUs really are listed under the same `endpoint` string —
  whitespace and `localhost` vs `127.0.0.1` count as different keys.
