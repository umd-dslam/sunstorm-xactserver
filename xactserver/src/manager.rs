use anyhow::{anyhow, ensure, Context};
use bytes::Bytes;
use futures::{future, Future};
use log::{debug, error, warn};
use prometheus::HistogramTimer;
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use url::Url;

use crate::decoder::RWSet;
use crate::metrics::{TOTAL_DURATION, XACTS};
use crate::node::client;
use crate::pg::{create_pg_conn_pool, PgConnectionPool};
use crate::proto::{PrepareRequest, VoteRequest};
use crate::xact::XactStateType;
use crate::{NodeId, RollbackInfo, XactId, XactStatus, XsMessage, NODE_ID_BITS};

pub struct Manager {
    /// Id of current xactserver
    node_id: NodeId,

    /// Receivers for channels to receive messages from
    /// postgres (local) and other nodes (remote)
    local_rx: mpsc::Receiver<XsMessage>,
    remote_rx: mpsc::Receiver<XsMessage>,

    /// Connection for sending messages to postgres
    pg_url: Url,
    pg_max_conn_pool_size: u32,
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
        pg_max_conn_pool_size: u32,
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
            pg_max_conn_pool_size,
            pg_conn_pool: None,
            xact_state_managers: HashMap::new(),
            xact_id_counter: 1,
            peers: None,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.peers = Some(Arc::new(client::Nodes::connect(&self.peer_addrs).await?));
        self.pg_conn_pool =
            Some(create_pg_conn_pool(&self.pg_url, self.pg_max_conn_pool_size).await?);

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
        // TODO: Clean up state of finished transactions
        Ok(self.xact_state_managers.entry(id).or_insert(msg_tx))
    }
}

struct XactStateManager {
    /// Id of the transaction
    xact_id: XactId,
    /// Id of the current server
    node_id: NodeId,

    /// Connection pools
    pg_conn_pool: PgConnectionPool,
    peers: Arc<client::Nodes>,

    /// State of the transaction
    xact_state: XactStateType,

    /// Timer measuring time taken from starting to finishing the transaction
    total_duration_timer: Option<HistogramTimer>,
}

impl XactStateManager {
    fn new(
        xact_id: XactId,
        node_id: NodeId,
        pg_conn_pool: PgConnectionPool,
        peers: Arc<client::Nodes>,
    ) -> Self {
        Self {
            xact_id,
            node_id,
            pg_conn_pool,
            peers,
            xact_state: XactStateType::Uninitialized,
            total_duration_timer: None,
        }
    }

    async fn run(mut self, mut msg_rx: mpsc::Receiver<XsMessage>) {
        while let Some(msg) = msg_rx.recv().await {
            let result = match msg {
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
            };

            match result {
                Ok(finish) => {
                    if finish {
                        break;
                    }
                }
                Err(e) => {
                    error!("Xact state manager encountered an error: {:?}", e);
                    break;
                }
            };
        }
    }

    async fn handle_local_xact_msg(
        &mut self,
        data: Bytes,
        commit_tx: oneshot::Sender<bool>,
    ) -> anyhow::Result<bool> {
        assert!(matches!(self.xact_state, XactStateType::Uninitialized));

        // This is a local transaction so the current region is the coordinator
        let coordinator = self.node_id;
        self.update_metrics(coordinator);

        // Deserialize the transaction data
        let mut rwset = RWSet::decode(data.clone()).context("Failed to decode read/write set")?;
        debug!("New local xact: {:#?}", rwset.decode_rest());

        // Create and initialize a new local xact
        self.xact_state = XactStateType::new_local_xact(
            self.xact_id,
            self.node_id,
            coordinator,
            rwset.participants(),
            commit_tx,
        )?;

        // Initialize the transaction
        let xact_status = self.xact_state.initialize().await?;
        assert_eq!(xact_status, &XactStatus::Waiting);

        // Send the transaction to other participants
        self.send_to_all_but_me(&rwset.participants(), |p| {
            let peers = self.peers.clone();
            let request = PrepareRequest {
                from: self.node_id as u32,
                xact_id: self.xact_id,
                data: data.clone().into_iter().collect(),
            };
            async move {
                let mut client = peers.get(p as NodeId).await?;
                client.prepare(tonic::Request::new(request)).await?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await?;

        Ok(false)
    }

    async fn handle_prepare_msg(&mut self, prepare_req: PrepareRequest) -> anyhow::Result<bool> {
        assert!(matches!(self.xact_state, XactStateType::Uninitialized));

        // Extract the coordinator of this transaction from the request
        let coordinator: NodeId = prepare_req.from.try_into()?;
        self.update_metrics(coordinator);

        // Deserialize the transaction data
        let data = Bytes::from(prepare_req.data);
        let mut rwset = RWSet::decode(data.clone()).context("Failed to decode read/write set")?;
        debug!("New surrogate xact: {:#?}", rwset.decode_rest());

        // If this node does not involve in the remotexact, stop the transaction immediately.
        if !rwset.participants().contains(self.node_id) {
            warn!(
                "Received a transaction from region {} that I do not participate in",
                self.node_id
            );
            return Ok(true);
        }

        // Create and initialize a new surrogate xact
        let xact_id = prepare_req.xact_id;
        self.xact_state = XactStateType::new_surrogate_xact(
            xact_id,
            self.node_id,
            coordinator,
            rwset.participants(),
            data,
            &self.pg_conn_pool,
        )?;

        // Initialize the transaction
        let status = self.xact_state.initialize().await?;

        // Extract rollback reason, if any
        let rollback_reason = match status {
            XactStatus::Rollbacked(RollbackInfo(_, rollback_reason)) => {
                Some(rollback_reason.into())
            }
            XactStatus::Committed | XactStatus::Waiting => None,
            _ => anyhow::bail!(
                "Surrogate xact {} has unexpected status: {:?}",
                self.xact_id,
                status
            ),
        };

        // Determine whether to terminate the current xact state manager
        let finish = status.is_terminal();

        // Send the vote to other participants
        self.send_to_all_but_me(&rwset.participants(), |p| {
            let peers = self.peers.clone();
            let request = VoteRequest {
                from: self.node_id as u32,
                xact_id,
                rollback_reason: rollback_reason.clone(),
            };
            async move {
                let mut client = peers.get(p as NodeId).await?;
                client.vote(tonic::Request::new(request)).await?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await?;

        Ok(finish)
    }

    fn update_metrics(&mut self, coordinator: usize) {
        let region = self.node_id.to_string();
        let coordinator = coordinator.to_string();

        XACTS.with_label_values(&[&region, &coordinator]).inc();

        self.total_duration_timer = Some(
            TOTAL_DURATION
                .with_label_values(&[&region, &coordinator])
                .start_timer(),
        );
    }

    async fn send_to_all_but_me<I, F, R>(&self, participants: I, f: F) -> anyhow::Result<()>
    where
        I: IntoIterator<Item = NodeId>,
        F: FnMut(NodeId) -> R,
        R: Future<Output = anyhow::Result<()>>,
    {
        future::try_join_all(
            participants
                .into_iter()
                .filter(|p| *p != self.node_id)
                .map(f),
        )
        .await?;
        Ok(())
    }

    async fn handle_vote_msg(&mut self, vote: VoteRequest) -> anyhow::Result<bool> {
        let status = self
            .xact_state
            .add_vote(
                vote.from.try_into()?,
                vote.rollback_reason.map(|e| e.into()),
            )
            .await?;

        Ok(status.is_terminal())
    }
}
