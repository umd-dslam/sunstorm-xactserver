pub mod manager;
pub mod node;
pub mod pg;
pub mod xact;
mod proto {
    tonic::include_proto!("xactserver");
}

use lazy_static::lazy_static;
pub use manager::XactManager;
pub use node::Node;

use bytes::Bytes;
use tokio::sync::oneshot;

pub type NodeId = usize;
pub type XactId = u64;

lazy_static! {
    pub static ref DUMMY_URL: url::Url = url::Url::parse("http://0.0.0.0").unwrap();
}
pub const NODE_ID_BITS: i32 = 10;
pub const DEFAULT_NODE_PORT: u16 = 23000;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact {
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    },
    Prepare(proto::PrepareRequest),
    Vote(proto::VoteRequest),
}
