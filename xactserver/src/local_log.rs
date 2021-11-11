use bytes::{Buf, Bytes};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// A `LocalLogManager` receives transactions from the postgres backends in the [`PgWatcher`]
/// and appends them to the local log.
///
/// [`PgWatcher`]: crate::PgWatcher
///
pub struct LocalLogManager {
    xact_log: Arc<Mutex<XactLog>>,
}

impl LocalLogManager {
    pub fn new() -> LocalLogManager {
        LocalLogManager {
            xact_log: Arc::new(Mutex::new(XactLog::new())),
        }
    }

    pub fn thread_main(&self, mut local_log_rx: mpsc::Receiver<Bytes>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();

        let xact_log = Arc::clone(&self.xact_log);
        rt.block_on(async move {
            while let Some(mut buf) = local_log_rx.recv().await {
                // TODO: The xact_log is wrapped in a mutex because it will be shared by
                // different asynchronous tasks responsible for log replication
                let mut log = xact_log.lock().unwrap();
                log.append(buf.to_vec());

                while let Some(tup) = get_tuple(&mut buf) {
                    println!("{:?}", tup);
                }
            }
        });

        Ok(())
    }
}

struct XactLog {
    log: Vec<Vec<u8>>,
}

impl XactLog {
    pub fn new() -> XactLog {
        XactLog { log: Vec::new() }
    }

    pub fn append(&mut self, entry: Vec<u8>) {
        self.log.push(entry)
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
