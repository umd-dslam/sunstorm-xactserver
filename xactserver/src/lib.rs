pub mod node;
pub mod pg;
pub mod transaction;
pub mod xactserver;

pub use node::Node;
pub use xactserver::XactServer;

use bytes::Bytes;
use tokio::sync::oneshot;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact {
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    },
    SurrogateXact {
        data: Bytes,
        vote_tx: oneshot::Sender<bool>,
    },
}
