use tokio_postgres::{connect, NoTls};
// use tokio_postgres::binary_copy::BinaryCopyInWriter;
use tokio::sync::mpsc;
use bytes::{Buf, Bytes};

pub struct PgDispatcher {
    addr: String,
}

impl PgDispatcher {
    pub fn new(addr: &str) -> PgDispatcher {
        PgDispatcher {
            addr: addr.to_owned(),
        }
    }

    pub fn thread_main(&self, txn_rx: mpsc::Receiver<Bytes>) -> anyhow::Result<()> {
        // obviously wrong cannot make a connection as soon as xactserver starts this thread
        // must wait till first remote txn comes in
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            let mut txn_rx = txn_rx;
            // Continuously listen for new tuple data from the postgres watcher
            while let Some(mut buf) = txn_rx.recv().await {
                while let Some(tup) = get_tuple(&mut buf) {
                    println!("# {:?}", tup);
                }

                let ip_port: Vec<&str> = self.addr.split(":").collect();
                let conn_str = format!("host={} port={} user=postgres application_name=#remotexact", ip_port[0], ip_port[1]);
                
                println!("connecting to local pg, conn_str: {}", conn_str);
                let (_client, conn) = connect(&conn_str, NoTls).await.unwrap();
                // The connection object performs the actual communication with the database,
                // so spawn it off to run on its own.
                tokio::spawn(async move {
                    if let Err(e) = conn.await {
                        eprintln!("connection error: {}", e);
                    }
                });

                println!("connected to pg");

            }
        });
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