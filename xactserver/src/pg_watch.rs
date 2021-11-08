//! # PostgreSQL watcher
//!
//! This module contains [`PgWatcher`] which watches for new transaction data from the
//! `remotexact` plugin in postgres.
//!  
use anyhow::Context;
use bytes::{Bytes, Buf};
use log::{info, error, debug};
use std::net::TcpListener;
use zenith_utils::postgres_backend::{self, AuthType, PostgresBackend};
use zenith_utils::pq_proto::{BeMessage, FeMessage};

/// This component listens for new connections from a postgres instance. For each
/// new connection, a [`PostgresBackend`] is created in a new thread, which will receive
/// the transaction read/write set.
/// 
/// [`PostgresBackend`]: zenith_utils::postgres_backend::PostgresBackend    
///
pub struct PgWatcher {
    addr: String,
}

impl PgWatcher {
    pub fn new(addr: &str) -> PgWatcher {
        PgWatcher {
            addr: String::from(addr),
        }
    }

    pub fn thread_main(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.addr)
            .with_context(|| "failed to start postgres watcher")?;

        info!("watching postgres at {}", self.addr);

        let mut join_handles = Vec::new();
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    error!("failed to establish a new postgres connection: {:#}", e);
                    continue;
                }
            };

            // Create a new PostgresBackend for each new connection from postgres
            let handle = std::thread::Builder::new()
                .spawn(move || {
                    let mut handler = PgWatcherHandler {};
                    let pg_backend =
                        PostgresBackend::new(stream, AuthType::Trust, None, true).unwrap();

                    if let Err(err) = pg_backend.run(&mut handler) {
                        error!("postgres backend exited with error: {:#}", err);
                    }
                })
                .unwrap();

            join_handles.push(handle);
        }

        for handle in join_handles.into_iter() {
            handle.join().unwrap();
        }

        Ok(())
    }
}

struct PgWatcherHandler {
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
                        if let FeMessage::CopyData(mut data) = message {
                            // TODO: just print the tuples for now
                            while let Some(tup) = get_tuple(&mut data) {
                                println!("{:?}", tup);
                            }
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

fn get_tuple(buf: &mut Bytes) -> Option<(i32, i32, i32, i16)> {
    if buf.remaining() == 0 {
        return None;
    }
    let dbid = buf.get_i32();
    let rid = buf.get_i32();
    let blockno = buf.get_i32();
    let offset = buf.get_i16();

    Some((dbid, rid, blockno, offset))
}
