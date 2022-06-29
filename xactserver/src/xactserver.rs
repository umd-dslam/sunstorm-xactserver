use std::net::SocketAddr;
use tokio::sync::mpsc;

use crate::XsMessage;

pub struct XactServer {
    peers: Vec<SocketAddr>,
    dispatcher_tx: mpsc::Sender<XsMessage>,
}

impl XactServer {
    pub fn new(peers: &Vec<SocketAddr>, dispatcher_tx: mpsc::Sender<XsMessage>) -> XactServer {
        XactServer {
            peers: peers.to_owned(),
            dispatcher_tx,
        }
    }

    pub fn thread_main(
        &self,
        local_rx: mpsc::Receiver<XsMessage>,
        remote_rx: mpsc::Receiver<XsMessage>,
    ) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        rt.block_on(async move {
            println!("Hello from XactServer");
        });

        Ok(())
    }
}
