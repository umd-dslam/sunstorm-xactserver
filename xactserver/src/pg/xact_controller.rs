use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use bb8::PooledConnection;
use bb8_postgres::{tokio_postgres::NoTls, PostgresConnectionManager};
use bytes::Bytes;
use tokio::sync::oneshot;

use crate::{pg::PgConnectionPool, XactId};

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
            .ok_or_else(|| anyhow!("Transaction has already committed or rollbacked"))
            .and_then(|tx| {
                tx.send(true)
                    .map_err(|_| anyhow!("Failed to commit transaction"))
            })
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        self.commit_tx
            .take()
            .ok_or_else(|| anyhow!("Transaction has already committed or rollbacked"))
            .and_then(|tx| {
                tx.send(false)
                    .map_err(|_| anyhow!("Failed to rollback transaction"))
            })
    }
}

pub struct SurrogateXactController {
    xact_id: XactId,
    data: Bytes,
    pg_conn_pool: PgConnectionPool,
    pg_conn: Option<PooledConnection<'static, PostgresConnectionManager<NoTls>>>,
}

impl SurrogateXactController {
    pub fn new(xact_id: XactId, data: Bytes, pg_conn_pool: PgConnectionPool) -> Self {
        Self {
            xact_id,
            data,
            pg_conn_pool,
            pg_conn: None,
        }
    }
}

#[async_trait]
impl XactController for SurrogateXactController {
    async fn execute(&mut self) -> anyhow::Result<()> {
        let conn = match self.pg_conn.take() {
            Some(c) => c,
            None => self.pg_conn_pool.get_owned().await?,
        };

        conn.simple_query("BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .await
            .with_context(|| format!("Failed to begin xact {}", self.xact_id))?;

        conn.execute(
            "SELECT validate_and_apply_xact($1::bytea);",
            &[&self.data.as_ref()],
        )
        .await
        .with_context(|| {
            format!(
                "Failed to execute validate_and_apply_xact($1::bytea) for xact {}",
                self.xact_id
            )
        })?;

        conn.simple_query(format!("PREPARE TRANSACTION '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to prepare xact {}", self.xact_id))?;

        // Retain the connection to perform commit or rollback later
        self.pg_conn = Some(conn);

        Ok(())
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        match &self.pg_conn {
            Some(conn) => {
                conn.simple_query(format!("COMMIT PREPARED '{}'", self.xact_id).as_str())
                    .await
                    .with_context(|| format!("Failed to commit xact {}", self.xact_id))?;
            }
            None => {
                bail!("No prepared transaction to commit");
            }
        }
        Ok(())
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        if let Some(conn) = &self.pg_conn {
            conn.simple_query(format!("ROLLBACK PREPARED '{}'", self.xact_id).as_str())
                .await
                .with_context(|| format!("Failed to rollback xact {}", self.xact_id))?;
        }
        Ok(())
    }
}
