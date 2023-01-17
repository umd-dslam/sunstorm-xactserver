//! # PostgreSQL watcher
//!
//! This module contains [`PgWatcher`] which watches for new transaction data from the
//! `remotexact` plugin in postgres.
//!  
use crate::XsMessage;
use anyhow::Context;
use bytes::{BufMut, BytesMut};
use log::{debug, error};
use neon_pq_proto::{BeMessage, FeMessage};
use neon_utils::postgres_backend::{self, AuthType, PostgresBackend};
use neon_utils::postgres_backend_async::QueryError;
use std::net::{SocketAddr, TcpListener};
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

    pub fn thread_main(self) -> anyhow::Result<()> {
        let listener =
            TcpListener::bind(self.listen_pg).context("Failed to start postgres watcher")?;

        let mut join_handles = Vec::new();
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    error!("Failed to establish a new postgres connection: {}", e);
                    continue;
                }
            };

            // Create a new sender to the xactserver for the new postgres backend
            let xact_manager_tx = self.xact_manager_tx.clone();

            // Create a new postgres backend for each new connection from postgres
            let handle = std::thread::Builder::new()
                .spawn(move || {
                    let mut handler = PgWatcherHandler { xact_manager_tx };
                    let pg_backend =
                        PostgresBackend::new(stream, AuthType::Trust, None, true).unwrap();

                    if let Err(e) = pg_backend.run(&mut handler) {
                        error!("Postgres backend exited with error: {}", e);
                    }
                })
                .unwrap();

            join_handles.push(handle);
        }

        for handle in join_handles {
            handle.join().unwrap();
        }

        Ok(())
    }
}

struct PgWatcherHandler {
    xact_manager_tx: mpsc::Sender<XsMessage>,
}

impl postgres_backend::Handler for PgWatcherHandler {
    fn process_query(
        &mut self,
        pgb: &mut PostgresBackend,
        _query_string: &str,
    ) -> anyhow::Result<(), QueryError> {
        // Switch to COPY BOTH mode
        pgb.write_message(&BeMessage::CopyBothResponse)?;

        debug!("New postgres connection established");

        loop {
            match pgb.read_message() {
                Ok(message) => {
                    if let Some(message) = message {
                        if let FeMessage::CopyData(buf) = message {
                            let (commit_tx, commit_rx) = oneshot::channel();
                            // Pass the transaction buffer to the xactserver.
                            // This is a blocking send because we're not inside an
                            // asynchronous environment
                            self.xact_manager_tx
                                .blocking_send(XsMessage::LocalXact {
                                    data: buf,
                                    commit_tx,
                                })
                                .map_err(|e| QueryError::Other(anyhow::anyhow!(e)))?;

                            let mut bytes = BytesMut::new();

                            if commit_rx
                                .blocking_recv()
                                .map_err(|e| QueryError::Other(anyhow::anyhow!(e)))?
                            {
                                bytes.put_u8(1);
                            } else {
                                bytes.put_u8(0);
                            }

                            pgb.write_message(&BeMessage::CopyData(&bytes.freeze()))?;
                        } else {
                            continue;
                        }
                    } else {
                        debug!("Postgres connection closed");
                        break;
                    }
                }
                Err(e) => {
                    if let QueryError::Other(e) = &e {
                        if postgres_backend::is_socket_read_timed_out(e) {
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }
}
