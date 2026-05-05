use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct RtuSnapshot {
    pub id: String,
    pub endpoint: String,

    // Data
    pub bi: Vec<bool>,
    pub ai0: f64,

    // Comms / health telemetry
    pub online: bool,              // derived from last_success_ms window
    pub last_update_ms: u64,       // last time a value changed in handler
    pub last_poll_ms: u64,         // last poll attempt time
    pub last_success_ms: u64,      // last successful poll time
    pub last_rtt_ms: u32,          // RTT of last successful poll
    pub consecutive_failures: u32, // fail streak
    pub poll_ok_count: u64,        // total successful poll attempts
    pub poll_fail_count: u64,      // total failed poll attempts
    pub last_error: String,        // last poll error string
}

impl RtuSnapshot {
    pub fn new(id: &str, endpoint: &str) -> Self {
        Self {
            id: id.to_string(),
            endpoint: endpoint.to_string(),

            bi: vec![false, false, false],
            ai0: 0.0,

            online: false,
            last_update_ms: 0,
            last_poll_ms: 0,
            last_success_ms: 0,
            last_rtt_ms: 0,
            consecutive_failures: 0,
            poll_ok_count: 0,
            poll_fail_count: 0,
            last_error: String::new(),
        }
    }
}
