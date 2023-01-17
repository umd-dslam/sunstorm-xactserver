use lazy_static::lazy_static;
use prometheus::register_int_counter;
use prometheus::IntCounter;

lazy_static! {
    pub static ref NUM_LOCAL_XACTS: IntCounter =
        register_int_counter!("local_xacts", "Number of local transactions").unwrap();
}
