use anyhow::{anyhow, bail, Context, ensure};
use async_trait::async_trait;
use bytes::Bytes;
use log::{error, info};
use std::net::SocketAddr;
use tokio::sync::oneshot;

use crate::XactId;

#[async_trait]
pub trait XactController {
    async fn execute(&mut self) -> anyhow::Result<()>;
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
    async fn execute(&mut self) -> anyhow::Result<()> {
        Ok(())
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
                tx.send(false)
                    .or(Err(anyhow!("Failed to rollback transaction")))
            })
    }
}

pub struct SurrogateXactController {
    xact_id: XactId,
    connect_pg: SocketAddr,
    data: Bytes,
    client: Option<tokio_postgres::Client>,
    prepared: bool,
}

impl SurrogateXactController {
    pub fn new(xact_id: XactId, connect_pg: SocketAddr, data: Bytes) -> Self {
        Self {
            xact_id,
            connect_pg,
            data,
            client: None,
            prepared: false,
        }
    }
}

#[async_trait]
impl XactController for SurrogateXactController {
    async fn execute(&mut self) -> anyhow::Result<()> {
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
        let xact_id = self.xact_id;

        client
            .simple_query("BEGIN")
            .await
            .with_context(|| format!("Failed to begin xact {}", xact_id))?;

        client
            .execute(
                "SELECT validate_and_apply_xact($1::bytea);",
                &[&self.data.as_ref()],
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to execute validate_and_apply_xact($1::bytea) for xact {}",
                    xact_id
                )
            })?;

        client
            .simple_query(format!("PREPARE TRANSACTION '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to prepare xact {}", xact_id))?;

        self.prepared = true;

        Ok(())
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        // Must be prepared to commit
        ensure!(self.prepared);

        match self.client {
            Some(ref client) => {
                client
                    .simple_query(format!("COMMIT PREPARED '{}'", self.xact_id).as_str())
                    .await
                    .with_context(|| format!("Failed to commit xact {}", self.xact_id))?;
            }
            None => {
                bail!("Connection does not exist");
            }
        }
        Ok(())
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        // Do nothing if it is not prepared
        if !self.prepared {
            return Ok(())
        }

        match self.client {
            Some(ref client) => {
                client
                    .simple_query(format!("ROLLBACK PREPARED '{}'", self.xact_id).as_str())
                    .await
                    .with_context(|| format!("Failed to roll back xact {}", self.xact_id))?;
            }
            None => {
                bail!("Connection does not exist");
            }
        }
        Ok(())
    }
}
