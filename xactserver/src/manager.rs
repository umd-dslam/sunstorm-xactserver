use log::{error, info};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use crate::proto::xact_coordination_client::XactCoordinationClient;
use crate::proto::{PrepareRequest, Vote, VoteRequest};
use crate::xact::{XactState, XactStatus};
use crate::{NodeId, XactId, XsMessage, NODE_ID_BITS};

struct SharedState {
    node_id: NodeId,
    peers: Vec<SocketAddr>,
    connect_pg: SocketAddr,
}

pub struct XactManager {
    shared: Arc<SharedState>,
    xact_state: HashMap<XactId, Arc<Mutex<XactState>>>,
    xact_id_counter: XactId,
}

impl XactManager {
    pub fn new(node_id: NodeId, peers: &Vec<SocketAddr>, connect_pg: SocketAddr) -> Self {
        Self {
            shared: Arc::new(SharedState {
                node_id,
                peers: peers.to_owned(),
                connect_pg,
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
                let new_xact_state = Arc::new(Mutex::new(XactState::new(
                    new_xact_id,
                    data.into_iter().collect(),
                    self.shared.node_id,
                    self.shared.peers.len() - 1,
                    Some(commit_tx),
                )));
                // TODO: assert that the returned value is None
                self.xact_state.insert(new_xact_id, new_xact_state.clone());

                tokio::spawn(XactManager::handle_local_xact_msg(
                    self.shared.clone(),
                    new_xact_state,
                ));
            }
            XsMessage::Prepare(prepare_req) => {
                let new_xact_state = Arc::new(Mutex::new(XactState::new(
                    prepare_req.xact_id,
                    prepare_req.data,
                    prepare_req.from,
                    self.shared.peers.len() - 1,
                    None,
                )));

                self.xact_state
                    .entry(prepare_req.xact_id)
                    .or_insert(new_xact_state.clone());

                tokio::spawn(XactManager::handle_prepare_msg(
                    self.shared.clone(),
                    new_xact_state,
                ));
            }
            XsMessage::Vote(vote_req) => {
                let cur_xact_state = self.xact_state.get(&vote_req.xact_id).unwrap();

                tokio::spawn(XactManager::handle_vote_msg(
                    self.shared.clone(),
                    cur_xact_state.clone(),
                    vote_req,
                ));
            }
        }
        Ok(())
    }

    async fn handle_local_xact_msg(shared: Arc<SharedState>, xact: Arc<Mutex<XactState>>) {
        let (xact_id, data) = {
            let xact = xact.lock().unwrap();
            // Return if the xact status has already been decided
            if xact.status() != XactStatus::Waiting {
                return;
            }

            (xact.id, xact.data.clone())
        };

        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        let request = PrepareRequest {
            from: shared.node_id,
            xact_id,
            data,
        };
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

    async fn handle_prepare_msg(shared: Arc<SharedState>, xact: Arc<Mutex<XactState>>) {
        // TODO: Avoid making connection every time
        let connect_pg = &shared.connect_pg;
        let conn_str = format!(
            "host={} port={} user=cloud_admin dbname=postgres application_name=xactserver",
            connect_pg.ip(),
            connect_pg.port(),
        );
        info!("connecting to local pg, conn_str: {}", conn_str);
        let (client, conn) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
            .await
            .unwrap();

        // The connection object performs the actual communication with the database,
        // so spawn it off to run on its own.
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("connection error: {}", e);
            }
        });

        let (xact_id, data) = {
            let xact = xact.lock().unwrap();
            (xact.id, xact.data.clone())
        };

        // TODO(mliu) should retry, just printing the error out for now
        if let Err(e) = client
            .query("SELECT print_bytes($1::bytea);", &[&data])
            .await
        {
            error!("calling postgres UDF failed with error: {}", e);
        }

        // TODO: This is sending one-by-one to all other peers, but we want to send
        //       to just relevant peers, in parallel.
        let request = VoteRequest {
            from: shared.node_id,
            xact_id,
            vote: Vote::Commit as i32,
        };
        for (i, peer) in shared.peers.iter().enumerate() {
            if i as NodeId != shared.node_id {
                let peer_url = format!("http://{}", peer.to_string());
                let mut client = XactCoordinationClient::connect(peer_url).await.unwrap();
                client
                    .vote(tonic::Request::new(request.clone()))
                    .await
                    .unwrap();
            }
        }
    }

    async fn handle_vote_msg(
        _shared: Arc<SharedState>,
        xact: Arc<Mutex<XactState>>,
        vote: VoteRequest,
    ) {
        let mut xact = xact.lock().unwrap();
        let status = xact.add_vote(vote.from, vote.vote == Vote::Abort as i32);
        if status == XactStatus::Commit {
            info!("Commit transaction {}", xact.id);
        } else if status == XactStatus::Abort {
            info!("Abort transaction {}", xact.id);
        }
    }
}
