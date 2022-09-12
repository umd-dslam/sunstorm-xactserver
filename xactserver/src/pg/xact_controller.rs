use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use bytes::Bytes;
use log::{error, info};
use std::net::SocketAddr;
use tokio::sync::oneshot;

use crate::XactId;

#[async_trait]
pub trait XactController {
    async fn execute(&mut self) -> anyhow::Result<bool>;
    async fn commit(&mut self) -> anyhow::Result<()>;
    async fn rollback(&mut self) -> anyhow::Result<()>;
}

pub struct LocalXactController {
    commit_tx: Option<oneshot::Sender<bool>>,
}

impl LocalXactController {
    pub fn new(commit_tx: oneshot::Sender<bool>) -> Self {
        Self {
            commit_tx: Some(commit_tx),
        }
    }
}

#[async_trait]
impl XactController for LocalXactController {
    async fn execute(&mut self) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        self.commit_tx
            .take()
            .ok_or(anyhow!("Transaction has already committed or rollbacked"))
            .and_then(|tx| {
                tx.send(true)
                    .or(Err(anyhow!("Failed to commit transaction")))
            })
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        self.commit_tx
            .take()
            .ok_or(anyhow!("Transaction has already committed or rollbacked"))
            .and_then(|tx| {
                tx.send(true)
                    .or(Err(anyhow!("Failed to rollback transaction")))
            })
    }
}

pub struct SurrogateXactController {
    xact_id: XactId,
    connect_pg: SocketAddr,
    data: Bytes,
    client: Option<tokio_postgres::Client>,
}

impl SurrogateXactController {
    pub fn new(xact_id: XactId, connect_pg: SocketAddr, data: Bytes) -> Self {
        Self {
            xact_id,
            connect_pg,
            data,
            client: None,
        }
    }
}

#[async_trait]
impl XactController for SurrogateXactController {
    async fn execute(&mut self) -> anyhow::Result<bool> {
        // TODO: Use a connection pool
        let conn_str = format!(
            "host={} port={} user=cloud_admin dbname=postgres application_name=xactserver",
            self.connect_pg.ip(),
            self.connect_pg.port(),
        );
        info!("Connecting to local pg, conn str: {}", conn_str);
        let (client, conn) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls).await?;

        // The connection object performs the actual communication with the database,
        // so spawn it off to run on its own.
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                error!("Connection error: {}", e);
            }
        });
        let client = self.client.get_or_insert(client);
        // TODO: Not doing anything for now
        client
            .execute("SELECT validate_and_apply_xact($1::bytea);", &[&self.data.as_ref()])
            .await
            .context("Failed to print bytes")?;
        Ok(true)
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        match self.client {
            Some(ref _client) => {
                // TODO: Not doing anything for now
                // client
                //     .batch_execute(format!("COMMIT PREPARED '{}'", self.xact_id).as_str())
                //     .await
                //     .context("Failed to commit prepared transaction")?;
                info!("[Dummy] Commit prepared transaction {}", self.xact_id);
            }
            None => {
                bail!("Connection does not exist");
            }
        }
        Ok(())
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        match self.client {
            Some(ref _client) => {
                // TODO: Not doing anything for now
                // client
                //     .batch_execute(format!("ROLLBACK PREPARED '{}'", self.xact_id).as_str())
                //     .await?;
                info!("[Dummy] Rollback prepared transaction {}", self.xact_id);
            }
            None => {
                bail!("Connection does not exist");
            }
        }
        Ok(())
    }
}
