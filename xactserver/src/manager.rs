use anyhow::{anyhow, bail, Context};
use bytes::Bytes;
use log::{debug, info};
use std::collections::HashMap;
use std::convert::TryInto;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::node::client;
use crate::pg::{LocalXactController, SurrogateXactController};
use crate::proto::{PrepareRequest, Vote, VoteRequest};
use crate::xact::{XactState, XactStatus};
use crate::{NodeId, XactId, XsMessage, DUMMY_ADDRESS, NODE_ID_BITS};

pub struct XactManager {
    node_id: NodeId,
    connect_pg: SocketAddr,
    peer_addrs: Vec<SocketAddr>,
    local_rx: mpsc::Receiver<XsMessage>,
    remote_rx: mpsc::Receiver<XsMessage>,
    xact_state_msg_tx: HashMap<XactId, mpsc::Sender<XsMessage>>,
    xact_id_counter: XactId,
    peers: Option<Arc<client::Nodes>>,
}

impl XactManager {
    pub fn new(
        node_id: NodeId,
        connect_pg: SocketAddr,
        peer_addrs: Vec<SocketAddr>,
        local_rx: mpsc::Receiver<XsMessage>,
        remote_rx: mpsc::Receiver<XsMessage>,
    ) -> Self {
        Self {
            node_id,
            peer_addrs,
            connect_pg,
            local_rx,
            remote_rx,
            xact_state_msg_tx: HashMap::new(),
            xact_id_counter: 1,
            peers: None,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.peers = Some(Arc::new(
            client::Nodes::connect(
                self.peer_addrs
                    .iter()
                    .map(|addr| {
                        if addr == &*DUMMY_ADDRESS {
                            String::default()
                        } else {
                            format!("http://{}", addr.to_string())
                        }
                    })
                    .collect(),
            )
            .await?,
        ));

        loop {
            tokio::select! {
                Some(msg) = self.local_rx.recv() => {
                    self.process_message(msg).await?;
                }
                Some(msg) = self.remote_rx.recv() => {
                    self.process_message(msg).await?;
                }
                else => {
                    break;
                }
            }
        }

        Ok(())
    }

    fn next_xact_id(&mut self) -> XactId {
        let xact_id = (self.xact_id_counter << NODE_ID_BITS) | (self.node_id as u64);
        self.xact_id_counter += 1;
        xact_id
    }

    async fn process_message(&mut self, msg: XsMessage) -> anyhow::Result<()> {
        // Get the sender for the xact state manager
        let msg_tx = match msg {
            XsMessage::LocalXact { .. } => {
                let xact_id = self.next_xact_id();
                self.new_xact_state_manager(xact_id)?
            }
            XsMessage::Prepare(ref prepare_req) => {
                self.new_xact_state_manager(prepare_req.xact_id)?
            }
            XsMessage::Vote(ref vote_req) => {
                self.xact_state_msg_tx
                    .get(&vote_req.xact_id)
                    .ok_or(anyhow!(
                        "No xact state manager running for xact id {}",
                        vote_req.xact_id
                    ))?
            }
        };
        // Forward the message to the xact state manager
        msg_tx.send(msg).await?;
        Ok(())
    }

    fn new_xact_state_manager(&mut self, id: XactId) -> anyhow::Result<&mpsc::Sender<XsMessage>> {
        if self.xact_state_msg_tx.contains_key(&id) {
            bail!("Xact state manager already exists for xact {}", id);
        }
        let (msg_tx, msg_rx) = mpsc::channel(2);
        // Start a new xact state manager
        let xact_state_man = XactStateManager::new(
            self.node_id,
            self.connect_pg,
            self.peers.as_ref().unwrap().clone(),
        );
        tokio::spawn(xact_state_man.run(id, msg_rx));
        // Save and return the sender of the new xact state manager
        Ok(self.xact_state_msg_tx.entry(id).or_insert(msg_tx))
    }
}

enum XactType {
    Local(XactState<LocalXactController>),
    Surrogate(XactState<SurrogateXactController>),
}

struct XactStateManager {
    node_id: NodeId,
    connect_pg: SocketAddr,
    peers: Arc<client::Nodes>,
    xact_state: Option<XactType>,
}

impl XactStateManager {
    fn new(node_id: NodeId, connect_pg: SocketAddr, peers: Arc<client::Nodes>) -> Self {
        Self {
            node_id,
            connect_pg,
            peers,
            xact_state: None,
        }
    }

    async fn run(mut self, id: XactId, mut msg_rx: mpsc::Receiver<XsMessage>) {
        while let Some(msg) = msg_rx.recv().await {
            debug!("New message received");
            match msg {
                XsMessage::LocalXact { data, commit_tx } => {
                    self.handle_local_xact_msg(id, data, commit_tx).await
                }
                XsMessage::Prepare(prepare_req) => self.handle_prepare_msg(prepare_req).await,
                XsMessage::Vote(vote_req) => self.handle_vote_msg(vote_req).await,
            }
            .context(format!("Xact id: {}", id))
            .unwrap();
        }
    }

    async fn handle_local_xact_msg(
        &mut self,
        xact_id: XactId,
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    ) -> anyhow::Result<()> {
        if self.xact_state.is_some() {
            bail!("Xact state is not empty");
        }

        // Create and initialize a new xact state
        let mut new_xact_state =
            XactState::<LocalXactController>::new(xact_id, data.clone(), commit_tx)?;

        debug!("New local xact: {:#?}", new_xact_state.rwset.decode_rest());

        // Initialize the xact state
        let xact_status = new_xact_state
            .initialize(self.node_id, self.node_id)
            .await?;

        // Save the xact state
        self.xact_state = Some(XactType::Local(new_xact_state));

        match xact_status {
            XactStatus::Waiting => {
                let request = PrepareRequest {
                    from: self.node_id as u32,
                    xact_id,
                    data: data.into_iter().collect(),
                };

                // TODO: This is sending one-by-one to all other peers, but we want to send
                //  to just relevant peers, in parallel.
                for i in 0..self.peers.size() {
                    let node_id = i as NodeId;
                    if node_id != self.node_id {
                        let mut client = self.peers.get(node_id).await?;
                        client.prepare(tonic::Request::new(request.clone())).await?;
                    }
                }
            }
            XactStatus::Committed => {
                info!("[Dummy] Xact {} committed", xact_id);
            }
            XactStatus::Rollbacked => {
                info!("[Dummy] Xact {} aborted", xact_id);
            }
            XactStatus::Uninitialized => {
                panic!("Unexpected state for xact {}", xact_id);
            }
        }
        Ok(())
    }

    async fn handle_prepare_msg(&mut self, prepare_req: PrepareRequest) -> anyhow::Result<()> {
        if self.xact_state.is_some() {
            bail!("Xact state is not empty");
        }

        // Create and initialize a new xact state
        let xact_id = prepare_req.xact_id;
        let data = Bytes::from(prepare_req.data);
        let mut new_xact_state =
            XactState::<SurrogateXactController>::new(xact_id, data, self.connect_pg)?;

        debug!("New remote xact: {:#?}", new_xact_state.rwset.decode_rest());

        // Initialize the xact state
        let xact_status = new_xact_state
            .initialize(prepare_req.from.try_into()?, self.node_id)
            .await?;

        // Save the xact state
        self.xact_state = Some(XactType::Surrogate(new_xact_state));

        let vote = if xact_status == XactStatus::Rollbacked {
            Vote::Abort
        } else {
            Vote::Commit
        };

        let request = VoteRequest {
            from: self.node_id as u32,
            xact_id,
            vote: vote as i32,
        };

        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        for i in 0..self.peers.size() {
            let node_id = i as NodeId;
            if node_id != self.node_id {
                let mut client = self.peers.get(node_id).await?;
                client.vote(tonic::Request::new(request.clone())).await?;
            }
        }
        Ok(())
    }

    async fn handle_vote_msg(&mut self, vote: VoteRequest) -> anyhow::Result<()> {
        match self.xact_state.as_mut() {
            None => bail!("Xact state does not exist"),
            Some(XactType::Local(xact)) => {
                let status = xact
                    .add_vote(vote.from.try_into()?, vote.vote == Vote::Abort as i32)
                    .await?;
                if status == XactStatus::Committed {
                    info!("Commit local transaction {}", xact.id);
                } else if status == XactStatus::Rollbacked {
                    info!("Abort local transaction {}", xact.id);
                }
                Ok(())
            }
            Some(XactType::Surrogate(xact)) => {
                let status = xact
                    .add_vote(vote.from.try_into()?, vote.vote == Vote::Abort as i32)
                    .await?;
                if status == XactStatus::Committed {
                    info!("Commit surrogate transaction {}", xact.id);
                } else if status == XactStatus::Rollbacked {
                    info!("Abort surrogate transaction {}", xact.id);
                }
                Ok(())
            }
        }
    }
}
