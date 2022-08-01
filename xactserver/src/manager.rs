use anyhow::{anyhow, bail, Context};
use bytes::Bytes;
use log::info;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

use crate::pg::PgXactController;
use crate::proto::xact_coordination_client::XactCoordinationClient;
use crate::proto::{PrepareRequest, Vote, VoteRequest};
use crate::xact::{XactState, XactStatus};
use crate::{NodeId, XactId, XsMessage, NODE_ID_BITS};

struct SharedState {
    node_id: NodeId,
    connect_pg: SocketAddr,
    peers: Vec<SocketAddr>,
}

pub struct XactManager {
    shared: Arc<SharedState>,
    local_rx: mpsc::Receiver<XsMessage>,
    remote_rx: mpsc::Receiver<XsMessage>,
    xact_state_msg_tx: HashMap<XactId, mpsc::Sender<XsMessage>>,
    xact_id_counter: XactId,
}

impl XactManager {
    pub fn new(
        node_id: NodeId,
        peers: &Vec<SocketAddr>,
        connect_pg: SocketAddr,
        local_rx: mpsc::Receiver<XsMessage>,
        remote_rx: mpsc::Receiver<XsMessage>,
    ) -> Self {
        Self {
            shared: Arc::new(SharedState {
                node_id,
                connect_pg,
                peers: peers.to_owned(),
            }),
            local_rx,
            remote_rx,
            xact_state_msg_tx: HashMap::new(),
            xact_id_counter: 1,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
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
        let xact_id = (self.xact_id_counter << NODE_ID_BITS) | (self.shared.node_id as u64);
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
        let xact_state_man = XactStateManager::new(self.shared.clone());
        tokio::spawn(xact_state_man.run(id, msg_rx));
        // Save and return the sender of the new xact state manager
        Ok(self.xact_state_msg_tx.entry(id).or_insert(msg_tx))
    }
}

struct XactStateManager {
    shared: Arc<SharedState>,
    xact_state: Option<XactState>,
}

impl XactStateManager {
    fn new(shared: Arc<SharedState>) -> Self {
        Self {
            shared,
            xact_state: None,
        }
    }

    async fn run(mut self, id: XactId, mut msg_rx: mpsc::Receiver<XsMessage>) {
        while let Some(msg) = msg_rx.recv().await {
            let res: anyhow::Result<()> = match msg {
                XsMessage::LocalXact { data, commit_tx } => {
                    self.handle_local_xact_msg(id, data, commit_tx).await
                }
                XsMessage::Prepare(prepare_req) => self.handle_prepare_msg(prepare_req).await,
                XsMessage::Vote(vote_req) => self.handle_vote_msg(vote_req).await,
            };

            res.context(format!("Xact id: {}", id)).unwrap();
        }
    }

    async fn handle_local_xact_msg(
        &mut self,
        id: XactId,
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    ) -> anyhow::Result<()> {
        if self.xact_state.is_some() {
            bail!("Xact state is not empty");
        }
        self.xact_state = Some(XactState::new(
            id,
            data.clone(),
            self.shared.node_id,
            self.shared.peers.len() - 1,
            PgXactController::new_local(commit_tx),
        )?);

        let request = PrepareRequest {
            from: self.shared.node_id,
            xact_id: id,
            data: data.into_iter().collect(),
        };
        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        for (i, peer) in self.shared.peers.iter().enumerate() {
            if i as NodeId != self.shared.node_id {
                let peer_url = format!("http://{}", peer.to_string());
                let mut client = XactCoordinationClient::connect(peer_url).await.unwrap();
                client
                    .prepare(tonic::Request::new(request.clone()))
                    .await
                    .unwrap();
            }
        }
        Ok(())
    }

    async fn handle_prepare_msg(&mut self, prepare_req: PrepareRequest) -> anyhow::Result<()> {
        if self.xact_state.is_some() {
            bail!("Xact state is not empty");
        }
        let xact_id = prepare_req.xact_id;
        let data = Bytes::from(prepare_req.data);
        let new_xact_state = XactState::new(
            xact_id,
            data.clone(),
            prepare_req.from,
            self.shared.peers.len() - 1,
            PgXactController::new_surrogate(xact_id, self.shared.connect_pg, data.clone()),
        )?;
        let vote = if self
            .xact_state
            .get_or_insert(new_xact_state)
            .validate()
            .await?
        {
            Vote::Commit
        } else {
            Vote::Abort
        };
        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        let request = VoteRequest {
            from: self.shared.node_id,
            xact_id,
            vote: vote as i32,
        };
        for (i, peer) in self.shared.peers.iter().enumerate() {
            if i as NodeId != self.shared.node_id {
                let peer_url = format!("http://{}", peer.to_string());
                let mut client = XactCoordinationClient::connect(peer_url).await.unwrap();
                client
                    .vote(tonic::Request::new(request.clone()))
                    .await
                    .unwrap();
            }
        }
        Ok(())
    }

    async fn handle_vote_msg(&mut self, vote: VoteRequest) -> anyhow::Result<()> {
        self.xact_state
            .as_mut()
            .ok_or(anyhow!("Xact state does not exist"))
            .and_then(|xact| {
                let status = xact.add_vote(vote.from, vote.vote == Vote::Abort as i32);
                if status == XactStatus::Commit {
                    info!("Commit transaction {}", xact.id);
                } else if status == XactStatus::Abort {
                    info!("Abort transaction {}", xact.id);
                }
                Ok(())
            })
    }
}
