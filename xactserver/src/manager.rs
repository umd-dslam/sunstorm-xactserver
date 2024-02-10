use crate::decoder::RWSet;
use crate::metrics::{
    get_rollback_reason_label, FINISHED_XACTS, PEER_NETWORK_DURATION, STARTED_XACTS, TOTAL_DURATION,
};
use crate::node::client;
use crate::pg::{create_pg_conn_pool, PgConnectionPool};
use crate::proto::{PrepareMessage, VoteMessage};
use crate::xact::XactType;
use crate::{NodeId, RollbackInfo, XactId, XactStatus, XsMessage};
use anyhow::Context;
use bytes::Bytes;
use futures::{future, Future};
use prometheus::HistogramTimer;
use std::cell::OnceCell;
use std::collections::HashMap;
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
    max_conn_pool_size_pg: u32,
    pg_conn_pool: OnceCell<PgConnectionPool>,

    /// Connections for sending messages to other xactserver nodes
    node_addrs: Vec<Url>,
    max_conn_pool_size_peer: u32,
    peers: OnceCell<Arc<client::Nodes>>,

    /// State of all transactions
    xact_state_managers: HashMap<XactId, mpsc::Sender<XsMessage>>,
    xact_id_counter: u32,
}

impl Manager {
    pub fn new(
        node_id: NodeId,
        pg_url: Url,
        max_conn_pool_size_pg: u32,
        node_addrs: Vec<Url>,
        max_conn_pool_size_peer: u32,
        local_rx: mpsc::Receiver<XsMessage>,
        remote_rx: mpsc::Receiver<XsMessage>,
    ) -> Self {
        Self {
            node_id,
            local_rx,
            remote_rx,
            pg_url,
            max_conn_pool_size_pg,
            pg_conn_pool: OnceCell::new(),
            node_addrs,
            max_conn_pool_size_peer,
            peers: OnceCell::new(),
            xact_state_managers: HashMap::new(),
            xact_id_counter: 0,
        }
    }

    pub async fn run(mut self, cancel: CancellationToken) -> anyhow::Result<()> {
        let _drop_guard = cancel.clone().drop_guard();

        let peers = Arc::new(
            client::Nodes::create_conn_pools(&self.node_addrs, self.max_conn_pool_size_peer)
                .await?,
        );
        self.peers.set(peers).unwrap();

        let pg_conn_pool = create_pg_conn_pool(&self.pg_url, self.max_conn_pool_size_pg).await?;
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
        // Extract the transaction id
        let xact_id = match &msg {
            XsMessage::LocalXact { .. } => {
                self.xact_id_counter += 1;
                XactId::new(self.xact_id_counter, self.node_id)
            }
            XsMessage::Prepare(prepare_req) => prepare_req.xact_id.into(),
            XsMessage::Vote(vote_req) => vote_req.xact_id.into(),
        };

        // Get the sender to the state manager of the transaction. Create one if it does not exist.
        let msg_tx = self.xact_state_managers.entry(xact_id).or_insert_with(|| {
            let (msg_tx, msg_rx) = mpsc::channel(100);

            let xact_state_man = XactStateManager::new(
                xact_id,
                self.node_id,
                self.pg_conn_pool.get().unwrap().clone(),
                self.peers.get().unwrap().clone(),
            );

            tokio::spawn(xact_state_man.run(msg_rx, cancel));

            msg_tx
        });

        // Send the message to the state manager
        if msg_tx.send(msg).await.is_err() {
            debug!("Cannot send message to stopped xact {}", xact_id);
        }
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
                            error!("{:?}", e);
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
                self.node_id,
                coordinator_id,
                rwset.participants(),
                commit_tx,
            )
            .await?;

        assert_eq!(status, &XactStatus::Waiting);

        // Send the transaction to other participants
        self.send_to_all_but_me(&rwset.participants(), |p| {
            let from = self.node_id;
            let peers = self.peers.clone();
            let message = PrepareMessage {
                from: from.into(),
                xact_id: self.xact_id.into(),
                data: data.clone().into_iter().collect(),
            };
            async move {
                let mut client = {
                    let _timer = PEER_NETWORK_DURATION
                        .with_label_values(&[
                            &from.to_string(),
                            &p.to_string(),
                            "get_client_to_prepare",
                        ])
                        .start_timer();

                    peers.get(p).await?
                };

                {
                    let _timer = PEER_NETWORK_DURATION
                        .with_label_values(&[&from.to_string(), &p.to_string(), "prepare"])
                        .start_timer();

                    client.prepare(tonic::Request::new(message)).await?;
                }

                Ok::<(), anyhow::Error>(())
            }
        })
        .await?;

        Ok(false)
    }

    async fn handle_prepare_msg(&mut self, prepare: PrepareMessage) -> anyhow::Result<bool> {
        // Extract the coordinator of this transaction from the request
        let coordinator_id = *self.coordinator_id.insert(prepare.from.into());

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
            let from = self.node_id;
            let peers = self.peers.clone();
            let message = VoteMessage {
                from: from.into(),
                xact_id: self.xact_id.into(),
                rollback_reason: rollback_reason.clone(),
            };
            async move {
                let mut client = {
                    let _timer = PEER_NETWORK_DURATION
                        .with_label_values(&[
                            &from.to_string(),
                            &p.to_string(),
                            "get_client_to_vote",
                        ])
                        .start_timer();

                    peers.get(p).await?
                };

                {
                    let _timer = PEER_NETWORK_DURATION
                        .with_label_values(&[&from.to_string(), &p.to_string(), "vote"])
                        .start_timer();

                    client.vote(tonic::Request::new(message)).await?;
                }

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

    async fn handle_vote_msg(&mut self, vote: VoteMessage) -> anyhow::Result<bool> {
        self.xact.add_vote(vote.into()).await?;

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
