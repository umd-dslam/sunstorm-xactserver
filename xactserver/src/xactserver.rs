use tokio::sync::{mpsc, oneshot};

use crate::XsMessage;

pub struct XactServer {
    peers: Vec<String>,
    dispatcher_tx: mpsc::Sender<(XsMessage, oneshot::Sender<bool>)>,
}

impl XactServer {
    pub fn new(
        peers: Vec<&str>,
        dispatcher_tx: mpsc::Sender<(XsMessage, oneshot::Sender<bool>)>,
    ) -> XactServer {
        XactServer {
            peers: peers.iter().map(|s| (*s).to_owned()).collect(),
            dispatcher_tx,
        }
    }

    pub fn thread_main(
        &self,
        watcher_rx: mpsc::Receiver<(XsMessage, oneshot::Sender<bool>)>,
        node_rx: mpsc::Receiver<XsMessage>,
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
