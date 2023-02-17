use std::net::SocketAddr;
use tokio::sync::mpsc;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use crate::proto::xact_coordination_server::{XactCoordination, XactCoordinationServer};
use crate::proto::{DummyResponse, PrepareMessage, VoteMessage};
use crate::XsMessage;

pub struct Node {
    addr: SocketAddr,
    xact_manager_tx: mpsc::Sender<XsMessage>,
}

impl Node {
    pub fn new(addr: SocketAddr, xact_manager_tx: mpsc::Sender<XsMessage>) -> Node {
        Self {
            addr,
            xact_manager_tx,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let addr = self.addr;
        let svc = XactCoordinationServer::new(self);
        Server::builder().add_service(svc).serve(addr).await?;

        Ok(())
    }
}

#[tonic::async_trait]
impl XactCoordination for Node {
    async fn prepare(
        &self,
        request: Request<PrepareMessage>,
    ) -> Result<Response<DummyResponse>, Status> {
        self.xact_manager_tx
            .send(XsMessage::Prepare(request.into_inner()))
            .await
            .unwrap();
        Ok(Response::new(DummyResponse {}))
    }

    async fn vote(&self, request: Request<VoteMessage>) -> Result<Response<DummyResponse>, Status> {
        self.xact_manager_tx
            .send(XsMessage::Vote(request.into_inner()))
            .await
            .unwrap();
        Ok(Response::new(DummyResponse {}))
    }
}

pub mod client {
    use anyhow::anyhow;
    use async_trait::async_trait;
    use futures::stream::{self, StreamExt, TryStreamExt};
    use url::Url;

    use crate::proto::xact_coordination_client::XactCoordinationClient;
    use crate::NodeId;

    pub struct Nodes {
        conn_pools: Vec<bb8::Pool<ConnectionManager>>,
    }

    impl Nodes {
        pub async fn connect(urls: &[Url]) -> anyhow::Result<Self> {
            let nbufferred = urls.len();
            let conn_pools = stream::iter(urls)
                .map(|url| bb8::Pool::builder().build(ConnectionManager(url.to_string())))
                .buffered(nbufferred)
                .try_collect()
                .await?;

            Ok(Self { conn_pools })
        }

        pub async fn get(
            &self,
            id: NodeId,
        ) -> anyhow::Result<bb8::PooledConnection<'_, ConnectionManager>> {
            let pool = self.conn_pools.get(id).ok_or_else(|| {
                anyhow!(
                    "Node id {} is out of range (1 - {})",
                    id,
                    self.conn_pools.len()
                )
            })?;
            Ok(pool.get().await?)
        }

        pub fn size(&self) -> usize {
            self.conn_pools.len()
        }
    }

    pub struct ConnectionManager(String);

    #[async_trait]
    impl bb8::ManageConnection for ConnectionManager {
        type Connection = XactCoordinationClient<tonic::transport::Channel>;
        type Error = tonic::transport::Error;

        async fn connect(&self) -> anyhow::Result<Self::Connection, Self::Error> {
            Ok(XactCoordinationClient::connect(self.0.clone()).await?)
        }

        async fn is_valid(&self, _: &mut Self::Connection) -> Result<(), Self::Error> {
            Ok(())
        }

        fn has_broken(&self, _: &mut Self::Connection) -> bool {
            false
        }
    }
}
