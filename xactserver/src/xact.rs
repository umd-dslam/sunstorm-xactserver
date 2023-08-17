mod state;

use crate::pg::{LocalXactController, PgConnectionPool, SurrogateXactController};
use crate::{NodeId, RollbackInfo, Vote, XactId};
use anyhow::Context;
use bit_set::BitSet;
use bytes::Bytes;
use state::XactState;
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq)]
pub enum XactStatus {
    Uninitialized,
    Waiting,
    Committing,
    Rollbacking(RollbackInfo),
    Committed,
    Rollbacked(RollbackInfo),
}

impl XactStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, XactStatus::Committed | XactStatus::Rollbacked(_))
    }
}

pub enum XactType {
    Unknown(Vec<Vote>),
    Local(XactState<LocalXactController>),
    Surrogate(XactState<SurrogateXactController>),
}

impl Default for XactType {
    fn default() -> Self {
        Self::Unknown(Vec::new())
    }
}

impl XactType {
    pub async fn init_as_local(
        &mut self,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        commit_tx: oneshot::Sender<Option<RollbackInfo>>,
    ) -> anyhow::Result<&XactStatus> {
        let controller = LocalXactController::new(commit_tx);
        let new_state = XactState::new(node_id, coordinator, participants, controller);

        let old_state = std::mem::replace(self, Self::Local(new_state));

        match (old_state, self) {
            (Self::Unknown(votes), Self::Local(state)) => state.initialize(votes).await,
            _ => unreachable!(),
        }
    }

    pub async fn init_as_surrogate(
        &mut self,
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        data: Bytes,
        pg_conn_pool: &PgConnectionPool,
    ) -> anyhow::Result<&XactStatus> {
        let controller = SurrogateXactController::new(xact_id, data, pg_conn_pool.clone());
        let new_state = XactState::new(node_id, coordinator, participants, controller);

        let old_state = std::mem::replace(self, Self::Surrogate(new_state));

        match (old_state, self) {
            (Self::Unknown(votes), Self::Surrogate(state)) => state.initialize(votes).await,
            _ => unreachable!(),
        }
    }

    pub async fn add_vote(&mut self, vote: Vote) -> anyhow::Result<()> {
        match self {
            Self::Unknown(votes) => {
                votes.push(vote);
            }
            Self::Local(xact) => {
                xact.add_vote(vote)
                    .await
                    .context("Failed to add vote for local xact")?;
            }
            Self::Surrogate(xact) => {
                xact.add_vote(vote)
                    .await
                    .context("Failed to add vote for surrogate xact")?;
            }
        };
        Ok(())
    }

    pub async fn try_finish(&mut self) -> anyhow::Result<&XactStatus> {
        match self {
            Self::Unknown(_) => anyhow::bail!("Xact state is uninitialized"),
            Self::Local(xact) => xact
                .try_finish()
                .await
                .context("Failed to try finishing local xact"),
            Self::Surrogate(xact) => xact
                .try_finish()
                .await
                .context("Failed to try finishing surrogate xact"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::create_pg_conn_pool;
    use crate::RollbackReason;
    use tokio::time::{timeout, Duration};
    use tracing_test::traced_test;

    fn participants(p: &[NodeId]) -> BitSet {
        let mut bs = BitSet::new();
        for i in p {
            bs.insert((*i).into());
        }
        bs
    }

    #[tokio::test]
    async fn test_init_as_local() -> anyhow::Result<()> {
        let (commit_tx, commit_rx) = oneshot::channel();
        let mut xact = XactType::default();
        let status = xact
            .init_as_local(
                NodeId(1),
                NodeId(1),
                participants(&[NodeId(1), NodeId(3), NodeId(4)]),
                commit_tx,
            )
            .await?;
        assert_eq!(status, &XactStatus::Waiting);

        xact.add_vote(Vote::yes(NodeId(3))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Waiting);

        xact.add_vote(Vote::yes(NodeId(4))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Committed);

        assert_eq!(
            timeout(Duration::from_secs(1), commit_rx).await.unwrap()?,
            None
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_init_as_local_aborted() -> anyhow::Result<()> {
        let (commit_tx, commit_rx) = oneshot::channel();
        let mut xact = XactType::default();
        let status = xact
            .init_as_local(
                NodeId(1),
                NodeId(1),
                participants(&[NodeId(1), NodeId(3), NodeId(4)]),
                commit_tx,
            )
            .await?;
        assert_eq!(status, &XactStatus::Waiting);

        xact.add_vote(Vote::yes(NodeId(3))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Waiting);

        xact.add_vote(Vote::no(
            NodeId(4),
            RollbackReason::Other("abort".to_string()),
        ))
        .await?;
        assert!(matches!(
            xact.try_finish().await?,
            XactStatus::Rollbacked(_)
        ));

        assert_eq!(
            timeout(Duration::from_secs(1), commit_rx).await.unwrap()?,
            Some(RollbackInfo(
                NodeId(4),
                RollbackReason::Other("abort".to_string())
            ))
        );

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_init_as_surrogate() -> anyhow::Result<()> {
        let pg_conn_pool =
            create_pg_conn_pool(&url::Url::parse("postgresql://fake:5432")?, 1).await?;
        let xact_id = XactId(100);
        let mut xact = XactType::default();

        // Initialize the transaction as a surrogate
        let status = xact
            .init_as_surrogate(
                xact_id,
                NodeId(2),
                NodeId(1),
                participants(&[NodeId(1), NodeId(2), NodeId(3), NodeId(4)]),
                Bytes::from("data"),
                &pg_conn_pool,
            )
            .await?;
        assert_eq!(status, &XactStatus::Waiting);

        // The transaction should send validation query to postgres
        assert!(logs_contain(
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        ));
        assert!(logs_contain("SELECT validate_and_apply_xact($1::bytea)"));
        assert!(logs_contain(
            format!("PREPARE TRANSACTION '{}'", xact_id).as_str()
        ));

        // No commit or rollback decision yet
        assert!(!logs_contain("COMMIT"));
        assert!(!logs_contain("ROLLBACK"));

        xact.add_vote(Vote::yes(NodeId(3))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Waiting);

        xact.add_vote(Vote::yes(NodeId(4))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Committed);

        // The transaction must commit now
        assert!(logs_contain(
            format!("COMMIT PREPARED '{}'", xact_id).as_str()
        ));

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_init_as_surrogate_aborted() -> anyhow::Result<()> {
        let pg_conn_pool =
            create_pg_conn_pool(&url::Url::parse("postgresql://fake:5432")?, 1).await?;
        let xact_id = XactId(100);
        let mut xact = XactType::default();

        // Initialize the transaction as a surrogate
        let status = xact
            .init_as_surrogate(
                xact_id,
                NodeId(2),
                NodeId(1),
                participants(&[NodeId(1), NodeId(2), NodeId(3), NodeId(4)]),
                Bytes::from("data"),
                &pg_conn_pool,
            )
            .await?;
        assert_eq!(status, &XactStatus::Waiting);

        // The transaction should send validation query to postgres
        assert!(logs_contain(
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        ));
        assert!(logs_contain("SELECT validate_and_apply_xact($1::bytea)"));
        assert!(logs_contain(
            format!("PREPARE TRANSACTION '{}'", xact_id).as_str()
        ));

        // No commit or rollback decision yet
        assert!(!logs_contain("COMMIT"));
        assert!(!logs_contain("ROLLBACK"));

        xact.add_vote(Vote::yes(NodeId(3))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Waiting);

        xact.add_vote(Vote::no(
            NodeId(4),
            RollbackReason::Other("abort".to_string()),
        ))
        .await?;
        assert!(matches!(
            xact.try_finish().await?,
            XactStatus::Rollbacked(_)
        ));

        // The transaction must rollback now
        assert!(logs_contain(
            format!("ROLLBACK PREPARED '{}'", xact_id).as_str()
        ));

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_init_as_surrogate_with_existing_votes() -> anyhow::Result<()> {
        let pg_conn_pool =
            create_pg_conn_pool(&url::Url::parse("postgresql://fake:5432")?, 1).await?;
        let xact_id = XactId(100);
        let mut xact = XactType::default();
        xact.add_vote(Vote::yes(NodeId(3))).await?;

        let status = xact
            .init_as_surrogate(
                xact_id,
                NodeId(2),
                NodeId(1),
                participants(&[NodeId(1), NodeId(2), NodeId(3), NodeId(4)]),
                Bytes::from("data"),
                &pg_conn_pool,
            )
            .await?;
        assert_eq!(status, &XactStatus::Waiting);

        // The transaction should send validation query to postgres
        assert!(logs_contain(
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        ));
        assert!(logs_contain("SELECT validate_and_apply_xact($1::bytea)"));
        assert!(logs_contain(
            format!("PREPARE TRANSACTION '{}'", xact_id).as_str()
        ));

        // No commit or rollback decision yet
        assert!(!logs_contain("COMMIT"));
        assert!(!logs_contain("ROLLBACK"));

        xact.add_vote(Vote::yes(NodeId(4))).await?;
        assert_eq!(xact.try_finish().await?, &XactStatus::Committed);

        // The transaction must commit now
        assert!(logs_contain(
            format!("COMMIT PREPARED '{}'", xact_id).as_str()
        ));

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_init_as_surrogate_with_all_existing_yes_votes() -> anyhow::Result<()> {
        let pg_conn_pool =
            create_pg_conn_pool(&url::Url::parse("postgresql://fake:5432")?, 1).await?;
        let xact_id = XactId(100);
        let mut xact = XactType::default();
        xact.add_vote(Vote::yes(NodeId(3))).await?;
        xact.add_vote(Vote::yes(NodeId(4))).await?;

        let status = xact
            .init_as_surrogate(
                xact_id,
                NodeId(2),
                NodeId(1),
                participants(&[NodeId(1), NodeId(2), NodeId(3), NodeId(4)]),
                Bytes::from("data"),
                &pg_conn_pool,
            )
            .await?;
        assert_eq!(status, &XactStatus::Committing);

        // The transaction should send validation query to postgres
        assert!(logs_contain(
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        ));
        assert!(logs_contain("SELECT validate_and_apply_xact($1::bytea)"));
        assert!(logs_contain(
            format!("PREPARE TRANSACTION '{}'", xact_id).as_str()
        ));

        // No commit or rollback decision yet
        assert!(!logs_contain("COMMIT"));
        assert!(!logs_contain("ROLLBACK"));

        assert_eq!(xact.try_finish().await?, &XactStatus::Committed);

        // The transaction must commit now
        assert!(logs_contain(
            format!("COMMIT PREPARED '{}'", xact_id).as_str()
        ));

        Ok(())
    }

    #[tokio::test]
    #[traced_test]
    async fn test_init_as_surrogate_with_existing_no_votes() -> anyhow::Result<()> {
        let pg_conn_pool =
            create_pg_conn_pool(&url::Url::parse("postgresql://fake:5432")?, 1).await?;
        let xact_id = XactId(100);
        let mut xact = XactType::default();
        xact.add_vote(Vote::no(
            NodeId(4),
            RollbackReason::Other("abort".to_string()),
        ))
        .await?;

        let status = xact
            .init_as_surrogate(
                xact_id,
                NodeId(2),
                NodeId(1),
                participants(&[NodeId(1), NodeId(2), NodeId(3), NodeId(4)]),
                Bytes::from("data"),
                &pg_conn_pool,
            )
            .await?;
        assert!(matches!(status, XactStatus::Rollbacking(_)));

        // The transaction should send validation query to postgres
        assert!(logs_contain(
            "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        ));
        assert!(logs_contain("SELECT validate_and_apply_xact($1::bytea)"));
        assert!(logs_contain(
            format!("PREPARE TRANSACTION '{}'", xact_id).as_str()
        ));

        // No commit or rollback decision yet
        assert!(!logs_contain("COMMIT"));
        assert!(!logs_contain("ROLLBACK"));

        xact.add_vote(Vote::yes(NodeId(3))).await?;
        assert!(matches!(
            xact.try_finish().await?,
            XactStatus::Rollbacked(_)
        ));

        // The transaction must rollback now
        assert!(logs_contain(
            format!("ROLLBACK PREPARED '{}'", xact_id).as_str()
        ));

        Ok(())
    }
}
