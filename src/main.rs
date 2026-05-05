use anyhow::Result;
use parking_lot::RwLock;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio::time::interval;

use tracing::{debug, info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use warp::Filter;

// ---------------- DNP3 ----------------
use dnp3::app::{ConnectStrategy, MaybeAsync, NullListener, ResponseHeader};
use dnp3::app::control::{Group12Var1, Group41Var3, OpType};
use dnp3::app::measurement::{AnalogInput, BinaryInput};
use dnp3::decode::DecodeLevel;
use dnp3::link::{EndpointAddress, LinkErrorMode};
use dnp3::master::{
    AssociationConfig, AssociationHandler, AssociationInformation, Classes,
    CommandBuilder, CommandMode, CommandSupport,
    HeaderInfo, ReadHandler, ReadRequest, ReadType,
};
use dnp3::tcp::{spawn_master_tcp_client, EndpointList};

// ---------------- MODEL ----------------
mod master;
use master::model::RtuSnapshot;

// ---------------- CONFIG ----------------
const MASTER_ADDR: u16 = 1;

// Class 0 truth poll cadence
const POLL_INTERVAL_SECS: u64 = 1;

// RED if no successful poll in > 10 seconds
const OFFLINE_AFTER_MS: u64 = 10_000;

// ---------------- RTU CONFIG ----------------
#[derive(Debug, Deserialize, Clone)]
struct RtusFile {
    rtu: Vec<RtuConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct RtuConfig {
    id: String,
    endpoint: String,
    rtu_addr: u16,
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
fn load_rtus() -> RtusFile {
    let path = std::env::var("RTUS_FILE").unwrap_or_else(|_| "rtus.toml".to_string());
    let data = fs::read_to_string(&path).expect("missing rtus.toml");
    toml::from_str(&data).expect("invalid rtus.toml")
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
    let _ = tracing::subscriber::set_default(subscriber);

    let cfg = load_rtus();
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
    let mut snapshots: HashMap<String, Arc<RwLock<RtuSnapshot>>> = HashMap::new();
    let mut cmd_txs: HashMap<String, mpsc::Sender<RtuCommand>> = HashMap::new();

    for rtu in cfg.rtu.iter().cloned() {
        let snapshot = Arc::new(RwLock::new(RtuSnapshot::new(&rtu.id, &rtu.endpoint)));
        snapshots.insert(rtu.id.clone(), snapshot.clone());

        let (tx, mut rx) = mpsc::channel::<RtuCommand>(64);
        cmd_txs.insert(rtu.id.clone(), tx);

        tokio::task::spawn_local(async move {
            let mut chan_cfg = dnp3::master::MasterChannelConfig::new(
                EndpointAddress::try_new(MASTER_ADDR).unwrap(),
            );

            // Keep library decode quiet; we log changes ourselves in the handler.
            chan_cfg.decode_level = DecodeLevel::nothing();

            let mut channel = spawn_master_tcp_client(
                LinkErrorMode::Close,
                chan_cfg,
                EndpointList::new(rtu.endpoint.clone(), &[]),
                ConnectStrategy::default(),
                NullListener::create(),
            );

            let mut assoc = channel
                .add_association(
                    EndpointAddress::try_new(rtu.rtu_addr).unwrap(),
                    AssociationConfig::default(),
                    Box::new(MasterReadHandler::new(snapshot.clone())),
                    Box::new(AssocHandler),
                    Box::new(AssocInfo),
                )
                .await
                .expect("failed to add association");

            channel.enable().await.expect("failed to enable channel");

            info!("MASTER: association enabled for RTU {} at {}", rtu.id, rtu.endpoint);

            let mut poll = interval(Duration::from_secs(POLL_INTERVAL_SECS));

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

    // No optimistic updates: queue command only
    let api_bi = {
        let cmd_txs_bi = Arc::clone(&cmd_txs);
        warp::path!("api" / "rtus" / String / "bi" / u16 / bool)
            .and(warp::post())
            .map(move |rtu_id, index, value| {
                if let Some(tx) = cmd_txs_bi.get(&rtu_id) {
                    let _ = tx.try_send(RtuCommand::SetBi { index, value });
                    return warp::reply::json(&"OK");
                }
                warp::reply::json(&"UNKNOWN_RTU")
            })
    };

    // No optimistic updates: queue command only
    let api_ai0 = {
        let cmd_txs_ai = Arc::clone(&cmd_txs);
        warp::path!("api" / "rtus" / String / "ai0" / f32)
            .and(warp::post())
            .map(move |rtu_id, value| {
                if let Some(tx) = cmd_txs_ai.get(&rtu_id) {
                    let _ = tx.try_send(RtuCommand::SetAi0 { value });
                    return warp::reply::json(&"OK");
                }
                warp::reply::json(&"UNKNOWN_RTU")
            })
    };

    // ---------- UI ----------
    let ui_root = warp::path::end().and(warp::fs::file("ui/index.html"));
    let ui_static = warp::path("ui").and(warp::fs::dir("ui"));

    let routes = ui_root.or(ui_static).or(api_rtus).or(api_bi).or(api_ai0);

    info!("Master UI/API running on http://0.0.0.0:9002");
    warp::serve(routes).run(([0, 0, 0, 0], 9002)).await;

    Ok(())
}
