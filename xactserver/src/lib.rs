pub mod node;
pub mod pg;
pub mod transaction;
pub mod xactserver;

pub use node::Node;
pub use xactserver::XactServer;

use bytes::Bytes;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact(Bytes),
    SurrogateXact(Bytes),
}
