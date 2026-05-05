# dnp3_master_io_v2

Raspberry Pi / Linux-based DNP3 master fleet monitor with:

- DNP3 TCP master connections to one or more RTUs
- Class 0 polling for authoritative field truth
- local HTTP UI / REST API
- operator command capability for binary and analog outputs
- fleet-style monitoring across multiple configured RTUs
- actual-vs-requested UI behavior for control visibility

---

## Important Licensing Note

This project uses the Step Function `dnp3` Rust crate.

Per written clarification from Step Function, the current intended use of this project is acceptable for:

- internal protocol demo and education
- network equipment testing and validation
- ad hoc integration testing
- internal training
- engineering evaluation
- internal CoE lab scaling

This approval applies as long as the project remains in **internal, non-production, non-customer-facing use**.

Examples that would require commercial licensing include:

- customer-facing sales demos
- bundling with shipped hardware
- production deployment
- broader commercial use

If usage changes, contact Step Function I/O for a commercial license.

---

## Overview

`dnp3_master_io_v2` is a DNP3 master-side monitoring and control application intended for lab, demo, kiosk, and engineering validation environments.

It is designed to connect to one or more DNP3 outstations/RTUs over TCP and present:

- current point values
- RTU communication health
- poll timing / last success
- operator-issued commands
- visible distinction between **actual telemetry** and **requested control values**

This makes it useful for:

- master-side validation against RTU simulators
- protocol demo environments
- fleet-style RTU visibility
- operator HMI demonstrations
- testing command and telemetry loops

---

## Core Design

The key design rule in this project is:

> **Telemetry is authoritative truth.**

That means:

- RTU snapshot values are updated only by successful DNP3 reads
- UI commands do **not** immediately overwrite displayed values
- operator writes are shown as **requested**
- actual values only change when Class 0 telemetry confirms them

This avoids false optimism and makes the UI reflect real protocol behavior.

---

## Features

- DNP3 TCP master for multiple RTUs
- RTU list loaded from `rtus.toml`
- 1-second Class 0 polling
- communication health calculation
- RTT tracking
- poll success/failure counters
- binary control support
- analog output support
- browser UI on port `9002`
- REST API for snapshots and commands
- requested-vs-actual command feedback
- timeout indication when command results do not arrive

---

## Configuration

RTUs are defined in `rtus.toml`.

### Example
```toml
[[rtu]]
id = "rtu1-tcp"
endpoint = "172.30.1.77:20000"
rtu_addr = 1024

[[rtu]]
id = "rtu1-serial-via-TS"
endpoint = "172.30.1.4:20000"
rtu_addr = 1024
Fields
id
Friendly name shown in the UI

endpoint
TCP endpoint of the outstation, in ip:port form

rtu_addr
DNP3 outstation link address

DNP3 Configuration
Master Address
The master uses:

master address: 1
Per-RTU Address
Each configured RTU provides:

outstation address: rtu_addr
Polling
Class 0 scan interval: 1 second
Offline Threshold
An RTU is considered offline if no successful poll has occurred for more than:

10 seconds
Supported Controls
Binary Control
The master sends binary controls using:

Group12Var1
OpType::LatchOn
OpType::LatchOff
Analog Control
The master sends analog controls using:

Group41Var3
Current analog control target:

AI0 at index 0
Transport Model
This project currently acts as a DNP3 TCP master.

Even if a target RTU is reached through terminal server infrastructure or serial-over-IP equipment, this application still connects using TCP sockets.

It does not currently open a local DNP3 serial master session directly.

Ports / Access
Local UI / API
HTTP UI / REST API: 9002
Example access
http://127.0.0.1:9002
UI Behavior
The UI displays one row per configured RTU with:

health state
RTU ID
endpoint
last RTT
consecutive failures
binary inputs
analog input
polling details
last error information
Status Colors
OK = RTU healthy and polling successfully
WARN = RTU online but poll failures accumulating
BAD = RTU considered offline
Actual vs Requested
The UI intentionally separates:

Actual = confirmed telemetry from RTU
Requested = last control command sent by the operator
This allows users to see whether:

a command was issued
telemetry matched the request
or the request timed out
Command Indicators
Possible command states:

SETTING RTU
TIMEOUT
no indicator once telemetry matches the requested value
REST API
List RTUs / current snapshot
curl -s http://127.0.0.1:9002/api/rtus
Send binary command
Example:

curl -s -X POST http://127.0.0.1:9002/api/rtus/rtu1-tcp/bi/0/true
Format:

/api/rtus//bi//
Where:

rtu_id = RTU id from rtus.toml
index = binary index
value = true or false
Send analog command
Example:

curl -s -X POST http://127.0.0.1:9002/api/rtus/rtu1-tcp/ai0/7.5
Format:

/api/rtus//ai0/
Build
Debug Build
cargo build
Release Build
cargo build --release
Run
Run with Cargo
cargo run
Run release binary
./target/release/dnp3_master_io_v2
Logging
Logging level is controlled by MASTER_LOG.

Default
If not set, the project defaults to:

INFO
Examples
bash
MASTER_LOG=debug cargo run
MASTER_LOG=trace cargo run
MASTER_LOG=warn cargo run
Optional Console Prints
Two environment variables allow printing point changes directly:

Print analog changes
MASTER_PRINT_AI=1 cargo run
Print binary changes
MASTER_PRINT_BI=1 cargo run
Combined
MASTER_LOG=debug MASTER_PRINT_AI=1 MASTER_PRINT_BI=1 cargo run
Data Model
Each RTU snapshot includes:

id
endpoint
bi
ai0
online
last_update_ms
last_poll_ms
last_success_ms
last_rtt_ms
consecutive_failures
poll_ok_count
poll_fail_count
last_error
These are exposed via /api/rtus.

Runtime Model
For each RTU:

A TCP master channel is opened
An association is created
The channel is enabled
A Class 0 poll runs every second
Telemetry updates the authoritative snapshot
Commands are queued and sent independently
UI reflects actual telemetry plus request intent
Project Structure
text
dnp3_master_io_v2/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── rtus.toml
├── src/
│   ├── main.rs
│   └── master/
│       ├── mod.rs
│       └── model.rs
├── ui/
│   └── index.html
└── target/
Key Files
src/main.rs
Runtime, polling, command queueing, REST API, UI serving

src/master/model.rs
RTU snapshot model

rtus.toml
RTU fleet configuration

ui/index.html
Browser UI

Typical Usage
Start the master
cargo run
Open the UI
http://127.0.0.1:9002
Confirm RTU telemetry
check online state
check RTT
verify BI and AI values
Issue controls
toggle BI lamps
move AI0 slider
Observe behavior
command shows as requested
actual changes only after telemetry confirms
Troubleshooting
UI/API not reachable
bash
curl -s http://127.0.0.1:9002/api/rtus
sudo ss -lntp | egrep ':9002'
RTU stays offline
Check:

endpoint IP/port
RTU address
network path
DNP3 outstation is running
master/outstation link addressing
No telemetry updates
Verify:

Class 0 polling is succeeding
RTU supports the expected point indices
outstation address matches rtu_addr
Commands do not take effect
Check:

RTU supports the command type
binary index is valid
analog output target is supported
telemetry eventually reflects the command
timeout indicator appears if command result never shows up
Enable deeper logs
MASTER_LOG=debug cargo run
Enable live point prints
MASTER_PRINT_AI=1 MASTER_PRINT_BI=1 cargo run
Future Improvements
Potential next steps:

configurable poll intervals
additional point support
structured alarm/health summaries
better command result tracking
per-RTU control permissions
deployment templates / service files
optional historical telemetry retention
direct serial master support if needed later
Attribution
This project uses the Step Function DNP3 Rust crate for internal demo, training, validation, and engineering evaluation use. Attribution to Step Function I/O is appreciated where protocol simulation or DNP3 stack components are referenced in internal demo materials.
