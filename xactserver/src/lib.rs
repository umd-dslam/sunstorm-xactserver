pub mod decoder;
pub mod pg;

mod id;
mod manager;
mod metrics;
mod node;
mod xact;
mod proto {
    tonic::include_proto!("xactserver");
}

pub use id::{NodeId, XactId};
pub use manager::Manager;
pub use node::Node;
pub use xact::XactStatus;

use crate::proto::VoteMessage;
use bytes::Bytes;
use lazy_static::lazy_static;
use proto::{vote_message, DbError};
use tokio::sync::oneshot;
use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    prelude::*,
};

lazy_static! {
    pub static ref DUMMY_URL: url::Url = url::Url::parse("http://0.0.0.0").unwrap();
}
pub const DEFAULT_NODE_PORT: u16 = 23000;

#[derive(Debug)]
pub enum XsMessage {
    LocalXact {
        data: Bytes,
        commit_tx: oneshot::Sender<Option<RollbackInfo>>,
    },
    Prepare(proto::PrepareMessage),
    Vote(proto::VoteMessage),
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

impl From<vote_message::RollbackReason> for RollbackReason {
    fn from(rollback_reason: vote_message::RollbackReason) -> Self {
        match rollback_reason {
            vote_message::RollbackReason::Db(db_err) => RollbackReason::Db(db_err),
            vote_message::RollbackReason::Other(err) => RollbackReason::Other(err),
        }
    }
}

impl From<&RollbackReason> for vote_message::RollbackReason {
    fn from(rollback_reason: &RollbackReason) -> Self {
        match rollback_reason {
            RollbackReason::Db(db_error) => vote_message::RollbackReason::Db(db_error.clone()),
            RollbackReason::Other(error) => vote_message::RollbackReason::Other(error.clone()),
        }
    }
}

pub struct Vote {
    from: NodeId,
    rollback_reason: Option<RollbackReason>,
}

impl Vote {
    pub fn yes(node_id: NodeId) -> Self {
        Self {
            from: node_id,
            rollback_reason: None,
        }
    }

    pub fn no(node_id: NodeId, reason: RollbackReason) -> Self {
        Self {
            from: node_id,
            rollback_reason: Some(reason),
        }
    }
}

impl From<VoteMessage> for Vote {
    fn from(msg: VoteMessage) -> Self {
        Self {
            from: msg.from.into(),
            rollback_reason: msg.rollback_reason.map(RollbackReason::from),
        }
    }
}

pub fn init_tracing(default_log_level: LevelFilter) -> anyhow::Result<()> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(default_log_level.into())
        .from_env_lossy();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_test_writer();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .try_init()?;

    Ok(())
}
