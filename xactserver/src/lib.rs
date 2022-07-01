pub mod manager;
pub mod node;
pub mod pgwatcher;
pub mod transaction;
mod proto {
    tonic::include_proto!("xactserver");
}

pub use manager::XactManager;
pub use node::Node;
pub use pgwatcher::PgWatcher;

use bytes::Bytes;
use tokio::sync::oneshot;

pub type NodeId = u32;
pub type XactId = u64;

pub const NODE_ID_BITS: i32 = 10;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact {
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    },
    Prepare(proto::PrepareRequest),
}
