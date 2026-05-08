use anyhow::{anyhow, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio::time::{interval_at, Instant};

use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use warp::http::StatusCode;
use warp::Filter;

// ---------------- DNP3 ----------------
use dnp3::app::{ConnectStrategy, MaybeAsync, NullListener, ResponseHeader};
use dnp3::app::control::{Group12Var1, Group41Var3, OpType};
use dnp3::app::measurement::{AnalogInput, BinaryInput};
use dnp3::decode::DecodeLevel;
use dnp3::link::{EndpointAddress, LinkErrorMode};
use dnp3::master::{
    AssociationConfig, AssociationHandle, AssociationHandler, AssociationInformation,
    Classes, CommandBuilder, CommandMode, CommandSupport,
    HeaderInfo, ReadHandler, ReadRequest, ReadType,
};
use dnp3::tcp::{spawn_master_tcp_client, EndpointList};

// ---------------- MODEL ----------------
mod master;
use master::model::RtuSnapshot;

// ---------------- CONFIG ----------------
// Default master link address if not overridden in rtus.toml.
const DEFAULT_MASTER_ADDR: u16 = 1;

// Class 0 truth poll cadence
const POLL_INTERVAL_SECS: u64 = 1;

// RED if no successful poll in > 10 seconds
const OFFLINE_AFTER_MS: u64 = 10_000;

// Default BI vector size when an RTU does not specify one.
fn default_bi_count() -> usize {
    3
}

// ---------------- RTU CONFIG ----------------
#[derive(Debug, Deserialize, Clone)]
struct RtusFile {
    /// Optional master DNP3 link address. Defaults to DEFAULT_MASTER_ADDR.
    #[serde(default)]
    master_addr: Option<u16>,
    rtu: Vec<RtuConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct RtuConfig {
    id: String,
    endpoint: String,
    rtu_addr: u16,
    /// Number of binary inputs to size the snapshot vector for.
    /// Indices beyond this are ignored on read.
    #[serde(default = "default_bi_count")]
    bi_count: usize,
}

// ---------------- COMMANDS ----------------
#[derive(Debug)]
enum RtuCommand {
    SetBi { index: u16, value: bool },
    SetAi0 { value: f32 },
}

// ---------------- TIME ----------------
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn compute_online(last_success_ms: u64) -> bool {
    last_success_ms != 0 && now_ms().saturating_sub(last_success_ms) <= OFFLINE_AFTER_MS
}

// ---------------- TELEMETRY HELPERS ----------------
fn mark_success(s: &mut RtuSnapshot, start_ms: u64) {
    let end = now_ms();
    s.last_success_ms = end;
    s.last_rtt_ms = end.saturating_sub(start_ms) as u32;
    s.consecutive_failures = 0;
    s.poll_ok_count += 1;
    s.last_error.clear();
    s.online = compute_online(s.last_success_ms);
}

fn mark_failure(s: &mut RtuSnapshot, err: &str) {
    s.poll_fail_count += 1;
    s.consecutive_failures = s.consecutive_failures.saturating_add(1);
    s.last_error = err.to_string();
    s.online = compute_online(s.last_success_ms);
}

// ---------------- READ HANDLER ----------------
// Authoritative state updates ONLY from telemetry.
// Logging is "on change only" to avoid spam.
struct MasterReadHandler {
    snapshot: Arc<RwLock<RtuSnapshot>>,
    last_ai0: Option<f64>,
    last_bi: Vec<Option<bool>>,
    print_ai: bool,
    print_bi: bool,
}

impl MasterReadHandler {
    fn new(snapshot: Arc<RwLock<RtuSnapshot>>) -> Self {
        let n = snapshot.read().bi.len();
        let print_ai = std::env::var("MASTER_PRINT_AI").ok().as_deref() == Some("1");
        let print_bi = std::env::var("MASTER_PRINT_BI").ok().as_deref() == Some("1");
        Self {
            snapshot,
            last_ai0: None,
            last_bi: vec![None; n],
            print_ai,
            print_bi,
        }
    }
}

impl ReadHandler for MasterReadHandler {
    fn begin_fragment(&mut self, _ty: ReadType, _hdr: ResponseHeader) -> MaybeAsync<()> {
        MaybeAsync::ready(())
    }

    fn handle_binary_input(
        &mut self,
        _: HeaderInfo,
        iter: &mut dyn Iterator<Item = (BinaryInput, u16)>,
    ) {
        let mut snap = self.snapshot.write();
        for (bi, idx) in iter {
            let i = idx as usize;
            if i < snap.bi.len() {
                // update authoritative telemetry
                snap.bi[i] = bi.value;

                // log only on change
                let prev = self.last_bi.get(i).copied().flatten();
                if prev.map(|p| p != bi.value).unwrap_or(true) {
                    debug!("RTU {} BI[{}] = {}", snap.id, idx, bi.value);
                    if self.print_bi {
                        println!("MASTER BI[{}] = {}", idx, bi.value);
                    }
                    if i < self.last_bi.len() {
                        self.last_bi[i] = Some(bi.value);
                    }
                }
            }
        }
        snap.last_update_ms = now_ms();
    }

    fn handle_analog_input(
        &mut self,
        _: HeaderInfo,
        iter: &mut dyn Iterator<Item = (AnalogInput, u16)>,
    ) {
        let mut snap = self.snapshot.write();
        for (ai, idx) in iter {
            if idx == 0 {
                // update authoritative telemetry
                snap.ai0 = ai.value;

                // log only on change
                let prev = self.last_ai0;
                if prev.map(|p| p != ai.value).unwrap_or(true) {
                    debug!("RTU {} AI0 = {:.6} (display {:.1})", snap.id, ai.value, ai.value);
                    if self.print_ai {
                        println!("MASTER AI0 = {:.1}", ai.value);
                    }
                    self.last_ai0 = Some(ai.value);
                }
            }
        }
        snap.last_update_ms = now_ms();
    }
}

#[derive(Copy, Clone)]
struct AssocHandler;
impl AssociationHandler for AssocHandler {}

struct AssocInfo;
impl AssociationInformation for AssocInfo {}

// ---------------- LOAD CONFIG ----------------
fn load_rtus() -> Result<RtusFile> {
    let path = std::env::var("RTUS_FILE").unwrap_or_else(|_| "rtus.toml".to_string());
    let data = fs::read_to_string(&path)
        .map_err(|e| anyhow!("reading {}: {}", path, e))?;
    let cfg: RtusFile = toml::from_str(&data)
        .map_err(|e| anyhow!("parsing {}: {}", path, e))?;

    // Reject duplicate `id`s — they would silently overwrite snapshot/cmd
    // map entries and produce confusing UI behavior.
    let mut seen: HashSet<&str> = HashSet::new();
    for rtu in &cfg.rtu {
        if !seen.insert(rtu.id.as_str()) {
            return Err(anyhow!("duplicate RTU id in {}: {}", path, rtu.id));
        }
        if rtu.bi_count == 0 {
            return Err(anyhow!("RTU {} has bi_count = 0", rtu.id));
        }
    }
    Ok(cfg)
}

// ---------------- MAIN ----------------
fn main() -> Result<()> {
    // Quiet by default. Turn up with:
    //   MASTER_LOG=debug cargo run
    //   MASTER_LOG=trace cargo run
    let lvl = match std::env::var("MASTER_LOG").ok().as_deref() {
        Some("trace") => Level::TRACE,
        Some("debug") => Level::DEBUG,
        Some("warn") => Level::WARN,
        Some("error") => Level::ERROR,
        _ => Level::INFO,
    };

    let subscriber = FmtSubscriber::builder()
        .with_max_level(lvl)
        .finish();
    // Global default so logs from background tokio tasks (warp, dnp3 channel
    // task, etc.) all go through this subscriber.
    tracing::subscriber::set_global_default(subscriber).ok();

    let cfg = load_rtus()?;
    info!("Loaded {} RTUs from rtus.toml", cfg.rtu.len());
    debug!("MASTER_LOG={:?}", lvl);

    let local = LocalSet::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(local.run_until(async move { run(cfg).await }))?;
    Ok(())
}

// ---------------- RUN ----------------
async fn run(cfg: RtusFile) -> Result<()> {
    let master_addr = cfg.master_addr.unwrap_or(DEFAULT_MASTER_ADDR);

    let mut snapshots: HashMap<String, Arc<RwLock<RtuSnapshot>>> = HashMap::new();
    let mut cmd_txs: HashMap<String, mpsc::Sender<RtuCommand>> = HashMap::new();

    // Group RTUs by endpoint so any outstations sharing a TCP path
    // (e.g. multiple serial RTUs behind the same terminal server)
    // share ONE socket. Opening duplicate TCP sessions to the same
    // ip:port confuses terminal-server stacks and produces tangled
    // streams; this grouping avoids that by attaching multiple DNP3
    // associations to a single MasterChannel keyed by endpoint.
    let mut by_endpoint: HashMap<String, Vec<RtuConfig>> = HashMap::new();
    for rtu in cfg.rtu.iter().cloned() {
        by_endpoint.entry(rtu.endpoint.clone()).or_default().push(rtu);
    }

    for (endpoint, rtus) in by_endpoint {
        // Per-RTU snapshot + command channel; all share one TCP channel.
        let mut per_rtu: Vec<(RtuConfig, Arc<RwLock<RtuSnapshot>>, mpsc::Receiver<RtuCommand>)> =
            Vec::new();
        for rtu in rtus {
            let snapshot = Arc::new(RwLock::new(
                RtuSnapshot::new(&rtu.id, &rtu.endpoint, rtu.rtu_addr, rtu.bi_count),
            ));
            snapshots.insert(rtu.id.clone(), snapshot.clone());
            let (tx, rx) = mpsc::channel::<RtuCommand>(64);
            cmd_txs.insert(rtu.id.clone(), tx);
            per_rtu.push((rtu, snapshot, rx));
        }

        tokio::task::spawn_local(async move {
            let mut chan_cfg = dnp3::master::MasterChannelConfig::new(
                EndpointAddress::try_new(master_addr).unwrap(),
            );
            // Keep library decode quiet; we log changes ourselves in the handler.
            chan_cfg.decode_level = DecodeLevel::nothing();

            // LinkErrorMode::Discard: a single bad frame from one outstation
            // does not tear down a shared TCP socket. With Close (the old
            // single-RTU default), a glitch on one association would also
            // disconnect every other association on the same connection.
            let mut channel = spawn_master_tcp_client(
                LinkErrorMode::Discard,
                chan_cfg,
                EndpointList::new(endpoint.clone(), &[]),
                ConnectStrategy::default(),
                NullListener::create(),
            );

            // Add an association per RTU before enabling the channel.
            let mut prepared: Vec<(
                RtuConfig,
                Arc<RwLock<RtuSnapshot>>,
                mpsc::Receiver<RtuCommand>,
                AssociationHandle,
            )> = Vec::new();
            for (rtu, snapshot, rx) in per_rtu {
                let assoc = channel
                    .add_association(
                        EndpointAddress::try_new(rtu.rtu_addr).unwrap(),
                        AssociationConfig::default(),
                        Box::new(MasterReadHandler::new(snapshot.clone())),
                        Box::new(AssocHandler),
                        Box::new(AssocInfo),
                    )
                    .await
                    .expect("failed to add association");
                prepared.push((rtu, snapshot, rx, assoc));
            }

            channel.enable().await.expect("failed to enable channel");
            let n_assocs = prepared.len();
            info!(
                "MASTER: shared TCP channel to {} ({} association(s))",
                endpoint, n_assocs
            );

            // Spawn one poll/cmd loop per association on the shared channel.
            // Stagger first-poll across associations so n outstations behind
            // one TCP path don't all queue their first read at t=0; they get
            // evenly spread across one POLL_INTERVAL_SECS window.
            let period = Duration::from_secs(POLL_INTERVAL_SECS);
            let now = Instant::now();
            for (i, (rtu, snapshot, mut rx, mut assoc)) in prepared.into_iter().enumerate() {
                let stagger_ms = if n_assocs > 1 {
                    (POLL_INTERVAL_SECS * 1000) as usize * i / n_assocs
                } else {
                    0
                };
                let first_tick = now + Duration::from_millis(stagger_ms as u64);
                tokio::task::spawn_local(async move {
                    info!(
                        "MASTER: association {} link_addr={} on {} (offset {}ms)",
                        rtu.id, rtu.rtu_addr, rtu.endpoint, stagger_ms
                    );
                    let mut poll = interval_at(first_tick, period);
                    loop {
                        tokio::select! {
                            _ = poll.tick() => {
                                let start = now_ms();
                                {
                                    let mut s = snapshot.write();
                                    s.last_poll_ms = start;
                                }
                                debug!("CLASS 0 POLL → {}", rtu.id);
                                match assoc.read(ReadRequest::class_scan(Classes::class0())).await {
                                    Ok(_) => {
                                        let mut s = snapshot.write();
                                        mark_success(&mut s, start);
                                        debug!("POLL OK ← {} rtt={}ms", rtu.id, s.last_rtt_ms);
                                    }
                                    Err(e) => {
                                        let mut s = snapshot.write();
                                        mark_failure(&mut s, &format!("{:?}", e));
                                        warn!("POLL FAIL {} {:?}", rtu.id, e);
                                    }
                                }
                            }

                            Some(cmd) = rx.recv() => {
                                // Commands are intent ONLY; snapshot remains Class 0 truth.
                                match cmd {
                                    RtuCommand::SetBi { index, value } => {
                                        debug!("CMD BO {}[{}] = {}", rtu.id, index, value);
                                        let op = if value { OpType::LatchOn } else { OpType::LatchOff };
                                        let headers = CommandBuilder::single_header_u16(
                                            Group12Var1::from_op_type(op),
                                            index,
                                        );
                                        let _ = assoc.operate(CommandMode::SelectBeforeOperate, headers).await;
                                    }

                                    RtuCommand::SetAi0 { value } => {
                                        debug!("CMD AO {} = {:.1}", rtu.id, value);
                                        let headers = CommandBuilder::single_header_u16(
                                            Group41Var3::new(value),
                                            0,
                                        );
                                        let _ = assoc.operate(CommandMode::SelectBeforeOperate, headers).await;
                                    }
                                }
                            }
                        }
                    }
                });
            }

            // Hold `channel` alive forever; child tasks only own AssociationHandles.
            // Dropping the MasterChannel here would tear down the background TCP task.
            std::future::pending::<()>().await;
        });
    }

    let snapshots = Arc::new(snapshots);
    let cmd_txs = Arc::new(cmd_txs);

    // ---------- REST API ----------
    let api_rtus = {
        let snapshots = Arc::clone(&snapshots);
        warp::path!("api" / "rtus")
            .and(warp::get())
            .map(move || {
                let list: Vec<RtuSnapshot> =
                    snapshots.values().map(|s| s.read().clone()).collect();
                warp::reply::json(&list)
            })
    };

    // Fleet-level health summary for monitors / liveness probes.
    // Always returns 200; consumers should check `offline > 0`.
    let api_health = {
        let snapshots = Arc::clone(&snapshots);
        warp::path!("api" / "health")
            .and(warp::get())
            .map(move || {
                #[derive(Serialize)]
                struct Entry {
                    id: String,
                    rtu_addr: u16,
                    online: bool,
                    last_success_ms: u64,
                    consecutive_failures: u32,
                }
                #[derive(Serialize)]
                struct Health {
                    total: usize,
                    online: usize,
                    offline: usize,
                    rtus: Vec<Entry>,
                }
                let mut rtus: Vec<Entry> = snapshots
                    .values()
                    .map(|s| {
                        let s = s.read();
                        Entry {
                            id: s.id.clone(),
                            rtu_addr: s.rtu_addr,
                            online: s.online,
                            last_success_ms: s.last_success_ms,
                            consecutive_failures: s.consecutive_failures,
                        }
                    })
                    .collect();
                rtus.sort_by(|a, b| a.id.cmp(&b.id));
                let total = rtus.len();
                let online = rtus.iter().filter(|e| e.online).count();
                let offline = total - online;
                warp::reply::json(&Health { total, online, offline, rtus })
            })
    };

    // No optimistic updates: queue command only.
    // Surfaces real status codes so the UI / scripts can distinguish
    // queue-full pressure from unknown RTUs.
    let api_bi = {
        let cmd_txs_bi = Arc::clone(&cmd_txs);
        warp::path!("api" / "rtus" / String / "bi" / u16 / bool)
            .and(warp::post())
            .map(move |rtu_id: String, index, value| match cmd_txs_bi.get(&rtu_id) {
                None => warp::reply::with_status(
                    warp::reply::json(&"UNKNOWN_RTU"),
                    StatusCode::NOT_FOUND,
                ),
                Some(tx) => match tx.try_send(RtuCommand::SetBi { index, value }) {
                    Ok(_) => warp::reply::with_status(warp::reply::json(&"OK"), StatusCode::OK),
                    Err(_) => warp::reply::with_status(
                        warp::reply::json(&"QUEUE_FULL"),
                        StatusCode::SERVICE_UNAVAILABLE,
                    ),
                },
            })
    };

    let api_ai0 = {
        let cmd_txs_ai = Arc::clone(&cmd_txs);
        warp::path!("api" / "rtus" / String / "ai0" / f32)
            .and(warp::post())
            .map(move |rtu_id: String, value| match cmd_txs_ai.get(&rtu_id) {
                None => warp::reply::with_status(
                    warp::reply::json(&"UNKNOWN_RTU"),
                    StatusCode::NOT_FOUND,
                ),
                Some(tx) => match tx.try_send(RtuCommand::SetAi0 { value }) {
                    Ok(_) => warp::reply::with_status(warp::reply::json(&"OK"), StatusCode::OK),
                    Err(_) => warp::reply::with_status(
                        warp::reply::json(&"QUEUE_FULL"),
                        StatusCode::SERVICE_UNAVAILABLE,
                    ),
                },
            })
    };

    // ---------- UI ----------
    let ui_root = warp::path::end().and(warp::fs::file("ui/index.html"));
    let ui_static = warp::path("ui").and(warp::fs::dir("ui"));

    let routes = ui_root
        .or(ui_static)
        .or(api_rtus)
        .or(api_health)
        .or(api_bi)
        .or(api_ai0);

    info!("Master UI/API running on http://0.0.0.0:9002");
    warp::serve(routes).run(([0, 0, 0, 0], 9002)).await;

    Ok(())
}
