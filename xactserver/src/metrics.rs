use bb8_postgres::tokio_postgres::error::SqlState;
use lazy_static::lazy_static;
use prometheus::register_histogram_vec;
use prometheus::register_int_counter_vec;
use prometheus::HistogramVec;
use prometheus::IntCounterVec;

use crate::RollbackInfo;
use crate::RollbackReason;
use crate::XactStatus;

const DURATION_BUCKETS: &[f64] = &[
    0.000_001, 0.000_010, 0.000_100, // 1 us, 10 us, 100 us
    0.001_000, 0.010_000, 0.100_000, // 1 ms, 10 ms, 100 ms
    1.0, 10.0, 100.0, // 1 s, 10 s, 100 s
];

lazy_static! {
    pub static ref STARTED_XACTS: IntCounterVec = register_int_counter_vec!(
        "xactserver_started_xacts_total",
        "Total number of started transactions",
        &["region", "coordinator", "is_local"],
    )
    .unwrap();
    pub static ref FINISHED_XACTS: IntCounterVec = register_int_counter_vec!(
        "xactserver_finished_xacts_total",
        "Total number of finished transactions",
        &["region", "coordinator", "is_local", "rollback_reason"],
    )
    .unwrap();
    pub static ref TOTAL_DURATION: HistogramVec = register_histogram_vec!(
        "xactserver_commit_duration_seconds",
        "Time a transaction spent in the xactserver",
        &["region", "coordinator", "is_local"],
        DURATION_BUCKETS.into()
    )
    .unwrap();
    pub static ref EXECUTION_DURATION: HistogramVec = register_histogram_vec!(
        "xactserver_execution_duration_seconds",
        "Time spent for executing a transaction",
        &["region", "coordinator", "is_local"],
        DURATION_BUCKETS.into()
    )
    .unwrap();
}

pub fn get_rollback_reason_label(status: &XactStatus) -> Option<&'static str> {
    match status {
        XactStatus::Committed => Some("none"),
        XactStatus::Rollbacked(RollbackInfo(_, reason)) => match reason {
            RollbackReason::Db(err) => {
                let sql_state = SqlState::from_code(std::str::from_utf8(&err.code).unwrap());
                match sql_state {
                    SqlState::T_R_DEADLOCK_DETECTED => Some("deadlock"),
                    SqlState::T_R_SERIALIZATION_FAILURE => Some("serialization_failure"),
                    SqlState::T_R_STATEMENT_COMPLETION_UNKNOWN => {
                        Some("statement_completion_unknown")
                    }
                    _ => Some("other_sql"),
                }
            }
            RollbackReason::Other(_) => Some("other"),
        },
        _ => None,
    }
}
