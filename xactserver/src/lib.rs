pub mod decoder;
pub mod manager;
pub mod metrics;
pub mod node;
pub mod pg;
pub mod xact;
mod proto {
    tonic::include_proto!("xactserver");
}

use bytes::Bytes;
use lazy_static::lazy_static;
use proto::{vote_request, DbError};
use tokio::sync::oneshot;

pub use manager::Manager;
pub use node::Node;

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

#[derive(Clone, Debug, PartialEq)]
pub enum XactStatus {
    Unexecuted,
    Waiting,
    Committed,
    Rollbacked(RollbackInfo),
}

impl XactStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, XactStatus::Committed | XactStatus::Rollbacked(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RollbackInfo(NodeId, RollbackReason);

#[derive(Clone, Debug, PartialEq)]
pub enum RollbackReason {
    Db(DbError),
    Other(String),
}

// Must use the tokio_postgres module in the bb8_postgres crate
impl From<&bb8_postgres::tokio_postgres::Error> for RollbackReason {
    fn from(err: &bb8_postgres::tokio_postgres::Error) -> Self {
        match err.as_db_error() {
            Some(err) => RollbackReason::Db(DbError {
                code: err.code().code().as_bytes().to_vec(),
                severity: err.severity().to_string(),
                message: err.message().to_string(),
                detail: err.detail().unwrap_or_default().to_string(),
                hint: err.hint().unwrap_or_default().to_string(),
            }),
            None => RollbackReason::Other(err.to_string()),
        }
    }
}

impl From<vote_request::RollbackReason> for RollbackReason {
    fn from(rollback_reason: vote_request::RollbackReason) -> Self {
        match rollback_reason {
            vote_request::RollbackReason::Db(db_err) => RollbackReason::Db(db_err),
            vote_request::RollbackReason::Other(err) => RollbackReason::Other(err),
        }
    }
}

impl From<&RollbackReason> for vote_request::RollbackReason {
    fn from(rollback_reason: &RollbackReason) -> Self {
        match rollback_reason {
            RollbackReason::Db(db_error) => vote_request::RollbackReason::Db(db_error.clone()),
            RollbackReason::Other(error) => vote_request::RollbackReason::Other(error.clone()),
        }
    }
}
