use bytes::{Buf, Bytes};
use log::{error, info};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

mod proto {
    tonic::include_proto!("xactserver");
}
use proto::log_replication_client::LogReplicationClient;
use proto::Subscription;

/// A `RemoteLogsManager` subscribes to all peer servers to receives their transaction logs.
pub struct RemoteLogsManager {
    peers: Vec<String>,
}

impl RemoteLogsManager {
    pub fn new(peers: Vec<&str>) -> RemoteLogsManager {
        RemoteLogsManager {
            peers: peers.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    pub fn thread_main(&self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let mut tasks = vec![];
            // Create a log management task for each peer
            for p in &self.peers {
                tasks.push(self.get_log_management_task(p));
            }
            let tasks_res = futures::future::join_all(tasks).await;
            for res in tasks_res {
                if let Err(e) = res.unwrap() {
                    error!("task exited with error: {}", e);
                }
            }
        });

        Ok(())
    }

    fn get_log_management_task(&self, peer: &str) -> JoinHandle<anyhow::Result<()>> {
        let peer = peer.to_owned();

        tokio::spawn(async move {
            let peer = peer;
            loop {
                info!("trying to connect to {}", peer);
                let client = LogReplicationClient::connect(peer.clone()).await;

                if let Ok(mut client) = client {
                    let stream = client.subscribe(Subscription {}).await;

                    if let Ok(stream) = stream {
                        info!("subscribed to transaction log on {}", peer);
                        let mut stream = stream.into_inner();

                        while let Ok(Some(log_entry)) = stream.message().await {
                            let mut data = Bytes::from(log_entry.data);
                            while let Some(tup) = get_tuple(&mut data) {
                                println!("{:?}", tup);
                            }
                        }
                    }
                }

                sleep(Duration::from_secs(1)).await;
            }
        })
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
