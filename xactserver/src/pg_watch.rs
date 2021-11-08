//! # PostgreSQL watcher
//!
//! This module contains [`PgWatcher`] which watches for new transaction data from the
//! `remotexact` plugin in postgres.
//!  
use anyhow::Context;
use bytes::Bytes;
use log::{debug, error, info};
use std::net::TcpListener;
use tokio::sync::mpsc;
use zenith_utils::postgres_backend::{self, AuthType, PostgresBackend};
use zenith_utils::pq_proto::{BeMessage, FeMessage};

/// A `PgWatcher` listens for new connections from a postgres instance. For each
/// new connection, a [`PostgresBackend`] is created in a new thread. This postgres
/// backend will receive the transaction read/write set and forward this data to
/// [`LocalLogManager`].
///
/// [`PostgresBackend`]: zenith_utils::postgres_backend::PostgresBackend
/// [`LocalLogManager`]: crate::LocalLogManager
///
pub struct PgWatcher {
    addr: String,
    local_log_chan: mpsc::Sender<Bytes>,
}

impl PgWatcher {
    pub fn new(addr: &str, local_log_chan: mpsc::Sender<Bytes>) -> PgWatcher {
        PgWatcher {
            addr: addr.to_owned(),
            local_log_chan,
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

            // Create a new sender for the local log channel for the new postgres backend
            let local_log_chan = self.local_log_chan.clone();

            // Create a new postgres backend for each new connection from postgres
            let handle = std::thread::Builder::new()
                .spawn(move || {
                    let mut handler = PgWatcherHandler { local_log_chan };
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
    local_log_chan: mpsc::Sender<Bytes>,
}

impl postgres_backend::Handler for PgWatcherHandler {
    fn process_query(
        &mut self,
        pgb: &mut PostgresBackend,
        _query_string: Bytes,
    ) -> anyhow::Result<()> {
        // Switch to COPY BOTH mode
        pgb.write_message(&BeMessage::CopyBothResponse)?;

        debug!("new postgres connection established");

        loop {
            match pgb.read_message() {
                Ok(message) => {
                    if let Some(message) = message {
                        if let FeMessage::CopyData(buf) = message {
                            // Pass the transaction buffer to the local log manager.
                            // This is a blocking send because we're not inside an
                            // asynchronous environment
                            self.local_log_chan.blocking_send(buf)?;
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
