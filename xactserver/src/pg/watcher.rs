//! # PostgreSQL watcher
//!
//! This module contains [`PgWatcher`] which watches for new transaction data from the
//! `remotexact` plugin in postgres.
//!  
use crate::{RollbackInfo, RollbackReason, XsMessage};
use anyhow::Context;
use bytes::{BufMut, Bytes, BytesMut};
use log::{debug, error};
use neon_pq_proto::{BeMessage, FeMessage};
use neon_utils::postgres_backend::AuthType;
use neon_utils::postgres_backend_async::{self, PostgresBackend, QueryError};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

/// A `PgWatcher` listens for new connections from a postgres instance. For each
/// new connection, a [`PostgresBackend`] is created in a new thread. This postgres
/// backend will receive the transaction read/write set and forward this data to
/// [`XactServer`].
///
/// [`PostgresBackend`]: neon_utils::postgres_backend::PostgresBackend
/// [`XactServer`]: crate::XactServer
///
pub struct PgWatcher {
    listen_pg: SocketAddr,
    xact_manager_tx: mpsc::Sender<XsMessage>,
}

impl PgWatcher {
    pub fn new(listen_pg: SocketAddr, xact_manager_tx: mpsc::Sender<XsMessage>) -> Self {
        Self {
            listen_pg,
            xact_manager_tx,
        }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.listen_pg)
            .await
            .context("Failed to start postgres watcher")?;

        loop {
            match listener.accept().await {
                Ok((socket, peer_addr)) => {
                    debug!("accepted connection from {}", peer_addr);
                    tokio::spawn(Self::conn_main(self.xact_manager_tx.clone(), socket));
                }
                Err(err) => {
                    error!("accept() failed: {:?}", err);
                }
            }
        }
    }

    async fn conn_main(
        xact_manager_tx: mpsc::Sender<XsMessage>,
        socket: tokio::net::TcpStream,
    ) -> anyhow::Result<()> {
        let mut handler = PgWatcherHandler { xact_manager_tx };
        let pgbackend = PostgresBackend::new(socket, AuthType::Trust, None)?;
        pgbackend
            .run(&mut handler, std::future::pending::<()>)
            .await?;
        Ok(())
    }
}

struct PgWatcherHandler {
    xact_manager_tx: mpsc::Sender<XsMessage>,
}

#[async_trait::async_trait]
impl postgres_backend_async::Handler for PgWatcherHandler {
    fn startup(
        &mut self,
        _pgb: &mut PostgresBackend,
        _sm: &neon_pq_proto::FeStartupPacket,
    ) -> Result<(), QueryError> {
        Ok(())
    }

    async fn process_query(
        &mut self,
        pgb: &mut PostgresBackend,
        _query_string: &str,
    ) -> Result<(), QueryError> {
        // Switch to COPYBOTH
        pgb.write_message(&BeMessage::CopyBothResponse)?;
        pgb.flush().await?;

        debug!("new postgres connection established");

        loop {
            let msg = pgb.read_message().await?;

            let copy_data_bytes = match msg {
                Some(FeMessage::CopyData(bytes)) => bytes,
                Some(FeMessage::Terminate) => break,
                Some(m) => {
                    return Err(QueryError::Other(anyhow::anyhow!(
                        "unexpected message: {m:?} during COPY",
                    )));
                }
                None => break, // client disconnected
            };

            let (commit_tx, commit_rx) = oneshot::channel();

            // Pass the transaction buffer to the xact manager
            self.xact_manager_tx
                .send(XsMessage::LocalXact {
                    data: copy_data_bytes,
                    commit_tx,
                })
                .await
                .map_err(|e| QueryError::Other(anyhow::anyhow!(e)))?;

            // Receive the commit/rollback data from the xact manager
            let rollback_info = commit_rx
                .await
                .map_err(|e| QueryError::Other(anyhow::anyhow!(e)))?;

            pgb.write_message(&BeMessage::CopyData(&serialize_rollback_info(
                rollback_info,
            )))?;
            pgb.flush().await?;
        }

        Ok(())
    }
}

fn serialize_rollback_info(info: Option<RollbackInfo>) -> Bytes {
    let mut buf = BytesMut::new();
    if let Some(info) = info {
        match info {
            RollbackInfo(by, RollbackReason::Db(db_err)) => {
                // Plus 1 to distinguish between region 0 and the end of message byte
                buf.put_u8((by + 1).try_into().unwrap());

                // Error message
                buf.put_slice(db_err.message.as_bytes());
                buf.put_u8(b'\0');

                // SQL error data
                buf.put_u8(1);
                buf.put_slice(&db_err.code);
                buf.put_slice(db_err.severity.as_bytes());
                buf.put_u8(b'\0');
                buf.put_slice(db_err.detail.as_bytes());
                buf.put_u8(b'\0');
                buf.put_slice(db_err.hint.as_bytes());
                buf.put_u8(b'\0');
            }
            RollbackInfo(by, RollbackReason::Other(err)) => {
                // Plus 1 to distinguish between region 0 and the end of message byte
                buf.put_u8((by + 1).try_into().unwrap());

                // Error message
                buf.put_slice(err.as_bytes());
                buf.put_u8(b'\0');
            }
        }
    }
    // End of message
    buf.put_u8(0);

    buf.freeze()
}
