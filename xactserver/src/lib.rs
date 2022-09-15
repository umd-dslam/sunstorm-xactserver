pub mod manager;
pub mod node;
pub mod pg;
pub mod xact;
mod proto {
    tonic::include_proto!("xactserver");
}

pub use manager::XactManager;
pub use node::Node;

use bytes::Bytes;
use lazy_static::lazy_static;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::sync::oneshot;

pub type NodeId = usize;
pub type XactId = u64;

lazy_static! {
    pub static ref DUMMY_ADDRESS: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
}
pub const NODE_ID_BITS: i32 = 10;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact {
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    },
    Prepare(proto::PrepareRequest),
    Vote(proto::VoteRequest),
}
