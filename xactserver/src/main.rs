use bytes::{ Bytes, Buf };
use std::net::TcpListener;
use std::thread;
use zenith_utils::postgres_backend::{self, PostgresBackend, AuthType};
use zenith_utils::pq_proto::{ BeMessage, FeMessage };

struct XactServerHandler {
}

impl XactServerHandler {
    fn new() -> Self {
        XactServerHandler{}
    }
}

impl postgres_backend::Handler for XactServerHandler {
    fn process_query(
        &mut self,
        pgb: &mut PostgresBackend,
        _query_string: Bytes,
    ) -> anyhow::Result<()> {
        // Switch to COPY BOTH mode
        pgb.write_message(&BeMessage::CopyBothResponse)?;

        println!("new connection created");

        loop {
            match pgb.read_message() {
                Ok(message) => {
                    if let Some(message) = message {
                        if let FeMessage::CopyData(mut bytes) = message {
                            while let Some(tup) = get_tuple(&mut bytes) {
                                println!("{:?}", tup);
                            }
                        } else {
                            continue
                        }
                    } else {
                        println!("connection closed");
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

fn main() {
    let listener = TcpListener::bind("127.0.0.1:8888").unwrap();
    println!("Listening on port 8888");

    let mut join_handles = Vec::new();
    for stream in listener.incoming() {
        let stream = stream.unwrap();
        let handle = thread::Builder::new()
            .name("transaction server".into())
            .spawn(move || {
                let pg_backend = PostgresBackend::new(stream, AuthType::Trust, None, true).unwrap();
                let mut handler = XactServerHandler::new();
                if let Err(err) = pg_backend.run(&mut handler) {
                    println!("Cannot spawn new backend thread: {}", err);
                }
            })
            .unwrap();
        join_handles.push(handle);
    }

    for handle in join_handles.into_iter() {
        handle.join().unwrap();
    }
}