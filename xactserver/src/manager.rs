use anyhow::{anyhow, bail, ensure, Context};
use bytes::Bytes;
use log::{debug, info};
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use crate::node::client;
use crate::pg::{
    create_pg_conn_pool, LocalXactController, PgConnectionPool, SurrogateXactController,
};
use crate::proto::{PrepareRequest, Vote, VoteRequest};
use crate::xact::{XactState, XactStatus};
use crate::{NodeId, XactId, XsMessage, NODE_ID_BITS};

pub struct Manager {
    /// Id of current xactserver
    node_id: NodeId,

    /// Receivers for channels to receive messages from
    /// postgres (local) and other nodes (remote)
    local_rx: mpsc::Receiver<XsMessage>,
    remote_rx: mpsc::Receiver<XsMessage>,

    /// Connection for sending messages to postgres
    pg_url: Url,
    pg_conn_pool: Option<PgConnectionPool>,

    /// Connections for sending messages to other xactserver nodes
    peer_addrs: Vec<Url>,
    peers: Option<Arc<client::Nodes>>,

    /// State of all transactions
    xact_state_managers: HashMap<XactId, mpsc::Sender<XsMessage>>,
    xact_id_counter: XactId,
}

impl Manager {
    pub fn new(
        node_id: NodeId,
        pg_url: Url,
        peer_addrs: Vec<Url>,
        local_rx: mpsc::Receiver<XsMessage>,
        remote_rx: mpsc::Receiver<XsMessage>,
    ) -> Self {
        Self {
            node_id,
            local_rx,
            remote_rx,
            peer_addrs,
            pg_url,
            pg_conn_pool: None,
            xact_state_managers: HashMap::new(),
            xact_id_counter: 1,
            peers: None,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.peers = Some(Arc::new(client::Nodes::connect(&self.peer_addrs).await?));
        self.pg_conn_pool = Some(create_pg_conn_pool(&self.pg_url).await?);

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
        let msg_tx = match &msg {
            XsMessage::LocalXact { .. } => {
                let xact_id = self.next_xact_id();
                self.new_xact_state_manager(xact_id)?
            }
            XsMessage::Prepare(prepare_req) => self.new_xact_state_manager(prepare_req.xact_id)?,
            XsMessage::Vote(vote_req) => self
                .xact_state_managers
                .get(&vote_req.xact_id)
                .ok_or_else(|| {
                    anyhow!(
                        "No xact state manager running for xact id {}",
                        vote_req.xact_id
                    )
                })?,
        };
        // Forward the message to the xact state manager
        msg_tx.send(msg).await?;
        Ok(())
    }

    fn new_xact_state_manager(&mut self, id: XactId) -> anyhow::Result<&mpsc::Sender<XsMessage>> {
        ensure!(
            !self.xact_state_managers.contains_key(&id),
            "Xact state manager already exists for exact {}",
            id
        );
        let (msg_tx, msg_rx) = mpsc::channel(2);
        // Start a new xact state manager
        let xact_state_man = XactStateManager::new(
            id,
            self.node_id,
            self.pg_conn_pool.as_ref().unwrap().clone(),
            self.peers.as_ref().unwrap().clone(),
        );
        tokio::spawn(xact_state_man.run(msg_rx));
        // Save and return the sender of the new xact state manager
        Ok(self.xact_state_managers.entry(id).or_insert(msg_tx))
    }
}

enum XactType {
    Local(XactState<LocalXactController>),
    Surrogate(XactState<SurrogateXactController>),
}

struct XactStateManager {
    id: XactId,
    node_id: NodeId,
    pg_conn_pool: PgConnectionPool,
    peers: Arc<client::Nodes>,
    xact_state: Option<XactType>,
}

impl XactStateManager {
    fn new(
        id: XactId,
        node_id: NodeId,
        pg_conn_pool: PgConnectionPool,
        peers: Arc<client::Nodes>,
    ) -> Self {
        Self {
            id,
            node_id,
            pg_conn_pool,
            peers,
            xact_state: None,
        }
    }

    async fn run(mut self, mut msg_rx: mpsc::Receiver<XsMessage>) {
        while let Some(msg) = msg_rx.recv().await {
            match msg {
                XsMessage::LocalXact { data, commit_tx } => self
                    .handle_local_xact_msg(data, commit_tx)
                    .await
                    .context("Failed to handle local xact msg"),
                XsMessage::Prepare(prepare_req) => self
                    .handle_prepare_msg(prepare_req)
                    .await
                    .context("Failed to handle prepare msg"),
                XsMessage::Vote(vote_req) => self
                    .handle_vote_msg(vote_req)
                    .await
                    .context("Failed to handle vote msg"),
            }
            .with_context(|| format!("Failed to process xact {}", self.id))
            .unwrap();
        }
    }

    async fn handle_local_xact_msg(
        &mut self,
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    ) -> anyhow::Result<()> {
        ensure!(
            self.xact_state.is_none(),
            "Xact state {} already exists",
            self.id
        );

        // Create and initialize a new local xact state
        let mut new_xact_state = XactState::<LocalXactController>::new(self.id, &data, commit_tx)?;
        debug!("New local xact: {:#?}", new_xact_state.rwset.decode_rest());

        // Execute the transaction. Because this is a local transaction, the current node is the coordinator
        let xact_status = new_xact_state
            .execute(self.node_id, self.node_id)
            .await
            .context("Failed to execute local xact")?;
        assert_eq!(xact_status, XactStatus::Waiting);

        // Save the xact state
        let participants = new_xact_state.participants();
        self.xact_state = Some(XactType::Local(new_xact_state));

        // Construct the request
        let request = PrepareRequest {
            from: self.node_id as u32,
            xact_id: self.id,
            data: data.into_iter().collect(),
        };

        // Send the transaction to other participants
        // TODO: Send prepare in parallel.
        for i in participants {
            let node_id = i as NodeId;
            if node_id != self.node_id {
                let mut client = self.peers.get(node_id).await?;
                client.prepare(tonic::Request::new(request.clone())).await?;
            }
        }

        Ok(())
    }

    async fn handle_prepare_msg(&mut self, prepare_req: PrepareRequest) -> anyhow::Result<()> {
        ensure!(
            self.xact_state.is_none(),
            "Xact state {} already exists",
            self.id
        );

        // Create and initialize a new surrogate xact state
        let xact_id = prepare_req.xact_id;
        let mut new_xact_state = XactState::<SurrogateXactController>::new(
            xact_id,
            &Bytes::from(prepare_req.data),
            &self.pg_conn_pool,
        )?;

        debug!("New remote xact: {:#?}", new_xact_state.rwset.decode_rest());

        // If this node is not invovled in the remotexact, return immediately.
        let participants = new_xact_state.participants();
        if !participants.contains(&self.node_id) {
            return Ok(());
        }

        // Execute the transaction
        let coordinator = prepare_req.from.try_into()?;
        let xact_status = new_xact_state
            .execute(coordinator, self.node_id)
            .await
            .context("Failed to execute remote xact")?;

        // Save the xact state
        self.xact_state = Some(XactType::Surrogate(new_xact_state));

        // Determine the vote based on the status of the transaction after execution
        let vote = if xact_status == XactStatus::Rollbacked {
            Vote::Abort
        } else {
            Vote::Commit
        };

        // Construct the request
        let request = VoteRequest {
            from: self.node_id as u32,
            xact_id,
            vote: vote as i32,
        };

        // Send the vote to other participants
        // TODO: Send the vote in parallel.
        for i in participants {
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
            None => bail!("Xact state {} does not exist", self.id),
            Some(XactType::Local(xact)) => {
                let status = xact
                    .add_vote(vote.from.try_into()?, vote.vote == Vote::Abort as i32)
                    .await?;
                if status == XactStatus::Committed {
                    debug!("Local xact {} COMMITTED", xact.id);
                } else if status == XactStatus::Rollbacked {
                    debug!("Local xact {} ABORTED", xact.id);
                }
                Ok(())
            }
            Some(XactType::Surrogate(xact)) => {
                let status = xact
                    .add_vote(vote.from.try_into()?, vote.vote == Vote::Abort as i32)
                    .await?;
                if status == XactStatus::Committed {
                    debug!("Surrogate xact {} COMMITTED", xact.id);
                } else if status == XactStatus::Rollbacked {
                    debug!("Surrogate xact {} ABORTED", xact.id);
                }
                Ok(())
            }
        }
    }
}
