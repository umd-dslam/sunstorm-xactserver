use crate::decoder::RWSet;
use crate::metrics::{get_rollback_reason_label, FINISHED_XACTS, STARTED_XACTS, TOTAL_DURATION};
use crate::node::client;
use crate::pg::{create_pg_conn_pool, PgConnectionPool};
use crate::proto::{PrepareMessage, VoteMessage};
use crate::xact::XactType;
use crate::{NodeId, RollbackInfo, XactId, XactStatus, XsMessage};
use anyhow::{anyhow, ensure, Context};
use bytes::Bytes;
use futures::{future, Future};
use prometheus::HistogramTimer;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, warn};
use url::Url;

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
    pg_conn_pool: OnceCell<PgConnectionPool>,

    /// Connections for sending messages to other xactserver nodes
    peer_addrs: Vec<Url>,
    peers: OnceCell<Arc<client::Nodes>>,

    /// State of all transactions
    xact_state_managers: HashMap<XactId, mpsc::Sender<XsMessage>>,
    xact_id_counter: u32,
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
            pg_conn_pool: OnceCell::new(),
            xact_state_managers: HashMap::new(),
            xact_id_counter: 0,
            peers: OnceCell::new(),
        }
    }

    pub async fn run(mut self, cancel: CancellationToken) -> anyhow::Result<()> {
        let _drop_guard = cancel.clone().drop_guard();

        let peers = Arc::new(client::Nodes::connect(&self.peer_addrs).await?);
        self.peers.set(peers).unwrap();

        let pg_conn_pool = create_pg_conn_pool(&self.pg_url, self.pg_max_conn_pool_size).await?;
        self.pg_conn_pool.set(pg_conn_pool).unwrap();

        loop {
            tokio::select! {
                Some(msg) = self.local_rx.recv() => {
                    self.process_message(msg, cancel.clone()).await;
                }
                Some(msg) = self.remote_rx.recv() => {
                    self.process_message(msg, cancel.clone()).await;
                }
                _ = cancel.cancelled() => {
                    break;
                }
            };
        }

        info!("Manager stopped");

        Ok(())
    }

    async fn process_message(&mut self, msg: XsMessage, cancel: CancellationToken) {
        // Get the sender for the xact state manager
        let (msg_tx, xact_id) = match &msg {
            XsMessage::LocalXact { .. } => {
                self.xact_id_counter += 1;
                let xact_id = XactId::new(self.xact_id_counter, self.node_id);
                (self.new_xact_state_manager(xact_id, cancel), xact_id)
            }
            XsMessage::Prepare(prepare_req) => (
                self.new_xact_state_manager(prepare_req.xact_id.into(), cancel),
                prepare_req.xact_id.into(),
            ),
            XsMessage::Vote(vote_req) => {
                let msg_tx = self
                    .xact_state_managers
                    .get(&vote_req.xact_id.into())
                    .ok_or_else(|| anyhow!("no xact state manager running to vote"));
                (msg_tx, vote_req.xact_id.into())
            }
        };

        match msg_tx {
            Ok(msg_tx) => {
                // Forward the message to the xact state manager
                if msg_tx.send(msg).await.is_err() {
                    debug!(
                        "cannot send message to stopped xact state manager {}",
                        xact_id
                    );
                    self.xact_state_managers.remove(&xact_id);
                }
            }
            Err(err) => warn!("failed to obtain manager for xact {}: {:?}", xact_id, err),
        }
    }

    fn new_xact_state_manager(
        &mut self,
        id: XactId,
        cancel: CancellationToken,
    ) -> anyhow::Result<&mpsc::Sender<XsMessage>> {
        ensure!(
            !self.xact_state_managers.contains_key(&id),
            "xact state manager already exists for exact {}",
            id
        );
        let (msg_tx, msg_rx) = mpsc::channel(2);
        // Start a new xact state manager
        let xact_state_man = XactStateManager::new(
            id,
            self.node_id,
            self.pg_conn_pool.get().unwrap().clone(),
            self.peers.get().unwrap().clone(),
        );
        tokio::spawn(xact_state_man.run(msg_rx, cancel));
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
    /// Id of the coordinator
    coordinator_id: Option<NodeId>,

    /// Connection pools
    pg_conn_pool: PgConnectionPool,
    peers: Arc<client::Nodes>,

    /// State of the transaction
    xact: XactType,

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
            coordinator_id: None,
            pg_conn_pool,
            peers,
            xact: Default::default(),
            total_duration_timer: None,
        }
    }

    #[instrument(skip_all, fields(xact_id = %self.xact_id, node_id = %self.node_id))]
    async fn run(mut self, mut msg_rx: mpsc::Receiver<XsMessage>, cancel: CancellationToken) {
        loop {
            tokio::select! {
                Some(msg) = msg_rx.recv() => {
                    let result = match msg {
                        XsMessage::LocalXact { data, commit_tx } => self
                            .handle_local_xact_msg(data, commit_tx)
                            .await
                            .context("failed to handle local xact msg"),
                        XsMessage::Prepare(prepare_req) => self
                            .handle_prepare_msg(prepare_req)
                            .await
                            .context("failed to handle prepare msg"),
                        XsMessage::Vote(vote_req) => self
                            .handle_vote_msg(vote_req)
                            .await
                            .context("failed to handle vote msg"),
                    };

                    match result {
                        Ok(finish) => {
                            if finish {
                                break;
                            }
                        }
                        Err(e) => {
                            error!("{}", e);
                            break;
                        }
                    };
                }
                _ = cancel.cancelled() => {
                    break;
                }
                else => {
                    break;
                }
            }
        }

        debug!("Stopped");
    }

    async fn handle_local_xact_msg(
        &mut self,
        data: Bytes,
        commit_tx: oneshot::Sender<Option<RollbackInfo>>,
    ) -> anyhow::Result<bool> {
        // This is a local transaction so the current region is the coordinator
        let coordinator_id = *self.coordinator_id.insert(self.node_id);

        // This must happen after coordinator id is set
        self.begin_xact_measurement();

        // Deserialize the transaction data
        let mut rwset = RWSet::decode(data.clone()).context("Failed to decode read/write set")?;
        debug!("{:#?}", rwset.decode_rest());

        // Create and initialize a new local xact
        let status = self
            .xact
            .init_as_local(
                self.xact_id,
                self.node_id,
                coordinator_id,
                rwset.participants(),
                commit_tx,
            )
            .await?;

        assert_eq!(status, &XactStatus::Waiting);

        // Send the transaction to other participants
        self.send_to_all_but_me(&rwset.participants(), |p| {
            let peers = self.peers.clone();
            let message = PrepareMessage {
                from: self.node_id.into(),
                xact_id: self.xact_id.into(),
                data: data.clone().into_iter().collect(),
            };
            async move {
                let mut client = peers.get(p).await?;
                client.prepare(tonic::Request::new(message)).await?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await?;

        Ok(false)
    }

    async fn handle_prepare_msg(&mut self, prepare: PrepareMessage) -> anyhow::Result<bool> {
        // Extract the coordinator of this transaction from the request
        let coordinator_id = *self.coordinator_id.insert(prepare.from.try_into()?);

        // This must happen after coordinator id is set
        self.begin_xact_measurement();

        // Deserialize the transaction data
        let data = Bytes::from(prepare.data);
        let mut rwset = RWSet::decode(data.clone()).context("Failed to decode read/write set")?;
        debug!("{:#?}", rwset.decode_rest());

        // If this node does not involve in the remotexact, stop the transaction immediately.
        if !rwset.participants().contains(self.node_id.into()) {
            warn!(
                "Received a transaction from {:?} that I do not participate in",
                self.node_id
            );
            return Ok(true);
        }

        // Create and initialize a new surrogate xact
        let status = self
            .xact
            .init_as_surrogate(
                self.xact_id,
                self.node_id,
                coordinator_id,
                rwset.participants(),
                data,
                &self.pg_conn_pool,
            )
            .await?;

        // Extract rollback reason, if any
        let rollback_reason = match status {
            XactStatus::Rollbacking(RollbackInfo(_, rollback_reason))
            | XactStatus::Rollbacked(RollbackInfo(_, rollback_reason)) => {
                Some(rollback_reason.into())
            }
            XactStatus::Committing | XactStatus::Committed | XactStatus::Waiting => None,
            _ => anyhow::bail!(
                "Surrogate xact {} has unexpected status: {:?}",
                self.xact_id,
                status
            ),
        };

        // Send the vote to other participants
        self.send_to_all_but_me(&rwset.participants(), |p| {
            let peers = self.peers.clone();
            let message = VoteMessage {
                from: self.node_id.into(),
                xact_id: self.xact_id.into(),
                rollback_reason: rollback_reason.clone(),
            };
            async move {
                let mut client = peers.get(p).await?;
                client.vote(tonic::Request::new(message)).await?;
                Ok::<(), anyhow::Error>(())
            }
        })
        .await?;

        let status = self.xact.try_finish().await?;
        let finished = status.is_terminal();
        if let Some(label) = get_rollback_reason_label(status) {
            self.end_xact_measurement(label);
        }

        Ok(finished)
    }

    async fn send_to_all_but_me<F, R>(
        &self,
        participants: &bit_set::BitSet,
        f: F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(NodeId) -> R,
        R: Future<Output = anyhow::Result<()>>,
    {
        future::try_join_all(
            participants
                .into_iter()
                .map(NodeId::from)
                .filter(|p| *p != self.node_id)
                .map(f),
        )
        .await?;
        Ok(())
    }

    async fn handle_vote_msg(&mut self, vote: VoteMessage) -> anyhow::Result<bool> {
        self.xact.add_vote(vote.into()).await?;

        let status = self.xact.try_finish().await?;

        let finished = status.is_terminal();

        if let Some(label) = get_rollback_reason_label(status) {
            self.end_xact_measurement(label);
        }

        Ok(finished)
    }

    fn begin_xact_measurement(&mut self) {
        let region = self.node_id.to_string();
        let coordinator = self.coordinator_id.unwrap().to_string();
        let is_local = (self.node_id == self.coordinator_id.unwrap()).to_string();

        STARTED_XACTS
            .with_label_values(&[&region, &coordinator, &is_local])
            .inc();

        self.total_duration_timer = Some(
            TOTAL_DURATION
                .with_label_values(&[&region, &coordinator, &is_local])
                .start_timer(),
        );
    }

    fn end_xact_measurement(&mut self, rollback_reason: &str) {
        let region = self.node_id.to_string();
        let coordinator = self.coordinator_id.unwrap().to_string();
        let is_local = (self.node_id == self.coordinator_id.unwrap()).to_string();

        self.total_duration_timer.take().unwrap().stop_and_record();

        FINISHED_XACTS
            .with_label_values(&[&region, &coordinator, &is_local, rollback_reason])
            .inc();
    }
}
