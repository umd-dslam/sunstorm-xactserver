use crate::transaction::Transaction;

use bytes::Bytes;
use log::{error, info};
use std::sync::{Arc, RwLock};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

mod proto {
    tonic::include_proto!("xactserver");
}
use proto::log_replication_server::{LogReplication, LogReplicationServer};
use proto::{LogEntry, Subscription};

/// A `LocalLogManager` receives transactions from the postgres backends in the [`PgWatcher`]
/// and appends them to the local log. It also exposes a subscription server to which other
/// peer servers can subscribe for the log.
///
/// [`PgWatcher`]: crate::PgWatcher
///
pub struct LocalLogManager {
    xact_log: Arc<RwLock<XactLog>>,
    addr: String,
}

impl LocalLogManager {
    pub fn new(addr: &str) -> LocalLogManager {
        LocalLogManager {
            xact_log: Arc::new(RwLock::new(XactLog::new())),
            addr: addr.to_owned(),
        }
    }

    pub fn thread_main(&self, pg_chan: mpsc::Receiver<Bytes>) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            // TODO: this channel is only used for signaling for now. The data sent through
            // the channel is ignored, but it can be used to carry extra information later.
            let (log_appended_tx, log_appended_rx) = watch::channel(1);

            let log_management_task = self.get_log_management_task(pg_chan, log_appended_tx);
            let replication_task = self.get_replication_task(log_appended_rx);
            let (log_management_task_res, replication_task_res) =
                tokio::try_join!(log_management_task, replication_task).unwrap();

            if let Err(e) = log_management_task_res {
                error!("\"log management task\" exited with error: {}", e);
            }

            if let Err(e) = replication_task_res {
                error!("\"replication task\" exited with error: {}", e);
            }
        });

        Ok(())
    }

    fn get_log_management_task(
        &self,
        mut pg_chan: mpsc::Receiver<Bytes>,
        xact_log_appended: watch::Sender<i32>,
    ) -> JoinHandle<anyhow::Result<()>> {
        let xact_log = Arc::clone(&self.xact_log);

        tokio::spawn(async move {
            // Continuously listen for new transactions from the postgres watcher
            while let Some(mut buf) = pg_chan.recv().await {
                let mut xact_log = xact_log.write().unwrap();

                xact_log.append(buf.to_vec());
                
                // TODO: This is temporary for testing
                match Transaction::parse(&mut buf) {
                    Ok(xact) => info!("received\n{:#?}", xact),
                    Err(e) => error!("parsing error: {:?}", e),
                }

                // Notify all replication tasks about the new transaction
                // TODO: send a more meaningful value
                xact_log_appended.send(1)?
            }
            Ok(())
        })
    }

    fn get_replication_task(
        &self,
        xact_log_appended: watch::Receiver<i32>,
    ) -> JoinHandle<anyhow::Result<()>> {
        let svc = LogReplicationServer::new(LogReplicationHandler {
            xact_log: Arc::clone(&self.xact_log),
            xact_log_appended,
        });
        let addr = self.addr.clone();

        tokio::spawn(async move {
            info!("local log replication server listens on {}", addr);
            Server::builder()
                .add_service(svc)
                .serve(addr.parse()?)
                .await?;
            Ok(())
        })
    }
}

struct LogReplicationHandler {
    xact_log: Arc<RwLock<XactLog>>,
    xact_log_appended: watch::Receiver<i32>,
}

impl LogReplicationHandler {
    // This function runs in a dedicated asynchronous task. Whenever a new log entry is appended
    // to the local log, the log management task sends a signal to this task, upon which this task
    // wakes up and streams new log entries to the remote subscribers.
    async fn replicate_xact_log(
        request: Request<Subscription>,
        xact_log: Arc<RwLock<XactLog>>,
        mut xact_log_appended: watch::Receiver<i32>,
        stream: mpsc::Sender<Result<LogEntry, Status>>,
    ) {
        let remote_addr = request
            .remote_addr()
            .map_or("[none]".into(), |a| a.to_string());

        loop {
            // Wake up when there is a new transaction appended to the log or when the
            // stream is closed by the subscriber.
            tokio::select! {
                res = xact_log_appended.changed() => {
                    // This happens when the sender side of xact_log_appended is closed
                    if res.is_err() {
                        break;
                    }

                    // TODO: send the tail of the log to the subscriber for now. What should
                    // happen is that we send from the point requested by the subscriber
                    let log_entry = {
                        let xact_log = xact_log.read().unwrap();
                        xact_log.tail().and_then(
                            |data| Some(LogEntry { data: data.clone() }
                        ))
                    };

                    // Send the log entry to the subscriber
                    if let Some(log_entry) = log_entry {
                        if let Err(e) = stream.send(Ok(log_entry)).await {
                            error!("failed to replicate log entry to {}: {}", remote_addr, e);
                            break;
                        }
                    }
                }
                _ = stream.closed() => {
                    break;
                }
            }
        }
        info!("connection to {} was closed", remote_addr);
    }
}

#[tonic::async_trait]
impl LogReplication for LogReplicationHandler {
    type SubscribeStream = ReceiverStream<Result<LogEntry, Status>>;

    async fn subscribe(
        &self,
        request: Request<Subscription>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let (stream_tx, stream_rx) = mpsc::channel(100);

        // Spawn a long running task to continuously send local log entries
        // to the subscriber
        tokio::spawn(Self::replicate_xact_log(
            request,
            Arc::clone(&self.xact_log),
            self.xact_log_appended.clone(),
            stream_tx,
        ));

        Ok(Response::new(ReceiverStream::new(stream_rx)))
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

    pub fn tail(&self) -> Option<&Vec<u8>> {
        self.log.last()
    }
}
