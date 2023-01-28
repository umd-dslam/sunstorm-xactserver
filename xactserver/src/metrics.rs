use lazy_static::lazy_static;
use prometheus::register_histogram_vec;
use prometheus::register_int_counter_vec;
use prometheus::HistogramVec;
use prometheus::IntCounterVec;

lazy_static! {
    pub static ref XACTS: IntCounterVec = register_int_counter_vec!(
        "xactserver_xacts_total",
        "Total number of transactions",
        &["region", "coordinator"],
    )
    .unwrap();
    pub static ref TOTAL_DURATION: HistogramVec = register_histogram_vec!(
        "xactserver_commit_duration_seconds",
        "Time a transaction spent in the xactserver",
        &["region", "coordinator"],
    )
    .unwrap();
    pub static ref EXECUTION_DURATION: HistogramVec = register_histogram_vec!(
        "xactserver_execution_duration_seconds",
        "Time spent for executing a transaction",
        &["region", "coordinator"],
    )
    .unwrap();
}
