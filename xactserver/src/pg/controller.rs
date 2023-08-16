use anyhow::{anyhow, Context};
use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::oneshot;

use crate::{pg::PgConnectionPool, RollbackInfo, XactId};

#[async_trait]
pub trait XactController {
    async fn execute(&mut self) -> anyhow::Result<()>;
    async fn commit(&mut self) -> anyhow::Result<()>;
    async fn rollback(&mut self, info: &RollbackInfo) -> anyhow::Result<()>;
}

pub struct LocalXactController {
    commit_tx: Option<oneshot::Sender<Option<RollbackInfo>>>,
}

impl LocalXactController {
    pub fn new(commit_tx: oneshot::Sender<Option<RollbackInfo>>) -> Self {
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
            .ok_or_else(|| anyhow!("Transaction has already been committed or rollbacked"))
            .and_then(|tx| {
                tx.send(None)
                    .map_err(|_| anyhow!("The pg watcher has dropped the receiver"))
            })
    }

    async fn rollback(&mut self, info: &RollbackInfo) -> anyhow::Result<()> {
        self.commit_tx
            .take()
            .ok_or_else(|| anyhow!("Transaction has already been committed or rollbacked"))
            .and_then(|tx| {
                tx.send(Some(info.clone()))
                    .map_err(|_| anyhow!("The pg watcher has dropped the receiver"))
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
            .await
            .with_context(|| format!("Failed to validate xact {}", self.xact_id));

        conn.batch_execute(format!("PREPARE TRANSACTION '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to prepare xact {}", self.xact_id))?;

        // Check the result here to give PREPARE TRANSACTION a chance to run and
        // properly rollback the transaction if there is an error during validation.
        result?;

        Ok(())
    }

    async fn commit(&mut self) -> anyhow::Result<()> {
        let conn = self.pg_conn_pool.get().await?;

        conn.batch_execute(format!("COMMIT PREPARED '{}'", self.xact_id).as_str())
            .await
            .with_context(|| format!("Failed to commit prepared xact {}", self.xact_id))?;

        Ok(())
    }

    async fn rollback(&mut self, _info: &RollbackInfo) -> anyhow::Result<()> {
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
