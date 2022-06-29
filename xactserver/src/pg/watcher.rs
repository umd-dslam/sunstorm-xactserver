//! # PostgreSQL watcher
//!
//! This module contains [`PgWatcher`] which watches for new transaction data from the
//! `remotexact` plugin in postgres.
//!  
use crate::XsMessage;
use anyhow::Context;
use log::{debug, error, info};
use std::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use zenith_utils::postgres_backend::{self, AuthType, PostgresBackend};
use zenith_utils::pq_proto::{BeMessage, FeMessage};

/// A `PgWatcher` listens for new connections from a postgres instance. For each
/// new connection, a [`PostgresBackend`] is created in a new thread. This postgres
/// backend will receive the transaction read/write set and forward this data to
/// [`XactServer`].
///
/// [`PostgresBackend`]: zenith_utils::postgres_backend::PostgresBackend
/// [`XactServer`]: crate::XactServer
///
pub struct PgWatcher {
    addr: String,
    xactserver_tx: mpsc::Sender<XsMessage>,
}

impl PgWatcher {
    pub fn new(addr: &str, xactserver_tx: mpsc::Sender<XsMessage>) -> PgWatcher {
        PgWatcher {
            addr: addr.to_owned(),
            xactserver_tx,
        }
    }

    pub fn thread_main(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.addr).context("failed to start postgres watcher")?;

        info!("watching postgres on {}", self.addr);

        let mut join_handles = Vec::new();
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    error!("failed to establish a new postgres connection: {}", e);
                    continue;
                }
            };

            // Create a new sender to the xactserver for the new postgres backend
            let xactserver_tx = self.xactserver_tx.clone();

            // Create a new postgres backend for each new connection from postgres
            let handle = std::thread::Builder::new()
                .spawn(move || {
                    let mut handler = PgWatcherHandler { xactserver_tx };
                    let pg_backend =
                        PostgresBackend::new(stream, AuthType::Trust, None, true).unwrap();

                    if let Err(e) = pg_backend.run(&mut handler) {
                        error!("postgres backend exited with error: {}", e);
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
    xactserver_tx: mpsc::Sender<XsMessage>,
}

impl postgres_backend::Handler for PgWatcherHandler {
    fn process_query(
        &mut self,
        pgb: &mut PostgresBackend,
        _query_string: &str,
    ) -> anyhow::Result<()> {
        // Switch to COPY BOTH mode
        pgb.write_message(&BeMessage::CopyBothResponse)?;

        debug!("new postgres connection established");

        loop {
            match pgb.read_message() {
                Ok(message) => {
                    if let Some(message) = message {
                        if let FeMessage::CopyData(buf) = message {
                            let (commit_tx, _) = oneshot::channel();
                            // Pass the transaction buffer to the xactserver.
                            // This is a blocking send because we're not inside an
                            // asynchronous environment
                            self.xactserver_tx.blocking_send(XsMessage::LocalXact {
                                data: buf,
                                commit_tx,
                            })?;
                        } else {
                            continue;
                        }
                    } else {
                        debug!("postgres connection closed");
                        break;
                    }
                }
                Err(e) => {
                    if !postgres_backend::is_socket_read_timed_out(&e) {
                        return Err(e);
                    }
                }
            }
        }

        Ok(())
    }
}
