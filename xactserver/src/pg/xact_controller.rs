use anyhow::{anyhow, Context};
use async_trait::async_trait;
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
}

impl SurrogateXactController {
    pub fn new(xact_id: XactId, data: Bytes, pg_conn_pool: PgConnectionPool) -> Self {
        Self {
            xact_id,
            data,
            pg_conn_pool,
        }
    }
}

#[async_trait]
impl XactController for SurrogateXactController {
    async fn execute(&mut self) -> anyhow::Result<()> {
        let conn = self.pg_conn_pool.get().await?;

        conn.batch_execute("BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE")
            .await
            .with_context(|| format!("Failed to begin xact {}", self.xact_id))?;

        let result = conn
            .execute(
                "SELECT validate_and_apply_xact($1::bytea);",
                &[&self.data.as_ref()],
            )
            .await;

        conn.batch_execute(format!("PREPARE TRANSACTION '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to prepare xact {}", self.xact_id))?;

        // Check the result after PREPARE TRANSACTION so that the transaction
        // is properlly aborted if there is an error during validation.
        result?;

        Ok(())
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        let conn = self.pg_conn_pool.get().await?;

        conn.batch_execute(format!("COMMIT PREPARED '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to commit xact {}", self.xact_id))?;

        Ok(())
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        let conn = self.pg_conn_pool.get().await?;

        let prepared_xact = conn
            .query_opt(
                format!(
                    "SELECT 1 FROM pg_prepared_xacts WHERE gid='{}'",
                    self.xact_id
                )
                .as_str(),
                &[],
            )
            .await
            .with_context(|| format!("Failed to check preparedness of xact {}", self.xact_id))?;

        if prepared_xact.is_some() {
            conn.batch_execute(format!("ROLLBACK PREPARED '{}'", self.xact_id).as_str())
                .await
                .with_context(|| format!("Failed to rollback xact {}", self.xact_id))?;
        }

        Ok(())
    }
}
