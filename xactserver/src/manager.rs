use log::info;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

use crate::proto::xact_coordination_client::XactCoordinationClient;
use crate::proto::PrepareRequest;
use crate::{NodeId, XactId, XsMessage, NODE_ID_BITS};

struct SharedState {
    node_id: NodeId,
    peers: Vec<SocketAddr>,
}

struct XactState {
    id: XactId,
    data: Vec<u8>,
    commit_tx: Option<oneshot::Sender<bool>>,
}

pub struct XactManager {
    shared: Arc<SharedState>,
    xact_state: HashMap<XactId, Arc<Mutex<XactState>>>,
    xact_id_counter: XactId,
}

impl XactManager {
    pub fn new(node_id: NodeId, peers: &Vec<SocketAddr>) -> XactManager {
        Self {
            shared: Arc::new(SharedState {
                node_id,
                peers: peers.to_owned(),
            }),
            xact_state: HashMap::new(),
            xact_id_counter: 1,
        }
    }

    pub fn thread_main(
        &mut self,
        mut local_rx: mpsc::Receiver<XsMessage>,
        mut remote_rx: mpsc::Receiver<XsMessage>,
    ) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        rt.block_on(async move {
            loop {
                tokio::select! {
                    Some(msg) = local_rx.recv() => {
                        self.process_message(msg).unwrap();
                    }
                    Some(msg) = remote_rx.recv() => {
                        self.process_message(msg).unwrap();
                    }
                    else => {
                        break;
                    }
                }
            }
        });

        Ok(())
    }

    fn next_xact_id(&mut self) -> XactId {
        let xact_id = (self.xact_id_counter << NODE_ID_BITS) | (self.shared.node_id as u64);
        self.xact_id_counter += 1;
        xact_id
    }

    fn process_message(&mut self, msg: XsMessage) -> anyhow::Result<()> {
        match msg {
            XsMessage::LocalXact { data, commit_tx } => {
                let new_xact_id = self.next_xact_id();
                let new_xact_state = Arc::new(Mutex::new(XactState {
                    id: new_xact_id,
                    data: data.into_iter().collect(),
                    commit_tx: Some(commit_tx),
                }));
                // TODO: assert that the returned value is None
                self.xact_state.insert(new_xact_id, new_xact_state.clone());

                tokio::spawn(XactManager::handle_local_xact_msg(
                    self.shared.clone(),
                    new_xact_state,
                ));
            }
            XsMessage::Prepare(prepare_req) => {
                let new_xact_state = Arc::new(Mutex::new(XactState {
                    id: prepare_req.xact_id,
                    data: prepare_req.data,
                    commit_tx: None,
                }));

                self.xact_state
                    .entry(prepare_req.xact_id)
                    .or_insert(new_xact_state.clone());

                tokio::spawn(XactManager::handle_prepare_msg(
                    self.shared.clone(),
                    new_xact_state,
                ));
            }
        }
        Ok(())
    }

    async fn handle_local_xact_msg(shared: Arc<SharedState>, xact: Arc<Mutex<XactState>>) {
        let request = {
            let xact = xact.lock().unwrap();
            PrepareRequest {
                xact_id: xact.id,
                data: xact.data.clone(),
            }
        };

        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        for (i, peer) in shared.peers.iter().enumerate() {
            if i as NodeId != shared.node_id {
                let peer_url = format!("http://{}", peer.to_string());
                let mut client = XactCoordinationClient::connect(peer_url).await.unwrap();
                client
                    .prepare(tonic::Request::new(request.clone()))
                    .await
                    .unwrap();
            }
        }
    }

    async fn handle_prepare_msg(_shared: Arc<SharedState>, xact: Arc<Mutex<XactState>>) {
        let mut xact = xact.lock().unwrap();
        let parsed = crate::transaction::Transaction::parse(&mut bytes::Bytes::copy_from_slice(
            xact.data.as_mut_slice(),
        ))
        .unwrap();
        info!("{:#?}", parsed);
    }
}
