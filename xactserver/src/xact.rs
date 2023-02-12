use anyhow::{ensure, Context};
use bit_set::BitSet;
use bytes::Bytes;
use log::{debug, warn};
use tokio::sync::oneshot;

use crate::metrics::EXECUTION_DURATION;
use crate::pg::{LocalXactController, PgConnectionPool, SurrogateXactController, XactController};
use crate::{NodeId, RollbackInfo, RollbackReason, XactId, XactStatus};

pub enum XactStateType {
    Uninitialized,
    Local(XactState<LocalXactController>),
    Surrogate(XactState<SurrogateXactController>),
}

impl XactStateType {
    pub fn new_local_xact(
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        commit_tx: oneshot::Sender<Option<RollbackInfo>>,
    ) -> anyhow::Result<Self> {
        let controller = LocalXactController::new(commit_tx);
        Ok(Self::Local(XactState::<LocalXactController>::new(
            xact_id,
            node_id,
            coordinator,
            participants,
            controller,
        )?))
    }

    pub fn new_surrogate_xact(
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        data: Bytes,
        pg_conn_pool: &PgConnectionPool,
    ) -> anyhow::Result<Self> {
        let controller = SurrogateXactController::new(xact_id, data, pg_conn_pool.clone());
        Ok(Self::Surrogate(XactState::<SurrogateXactController>::new(
            xact_id,
            node_id,
            coordinator,
            participants,
            controller,
        )?))
    }

    pub async fn initialize(&mut self) -> anyhow::Result<&XactStatus> {
        match self {
            Self::Uninitialized => anyhow::bail!("Xact state is uninitialized"),
            Self::Local(xact) => xact
                .initialize()
                .await
                .context("Failed to initialize local xact"),
            Self::Surrogate(xact) => xact
                .initialize()
                .await
                .context("Failed to initialize surrogate xact"),
        }
    }

    pub async fn add_vote(
        &mut self,
        from: NodeId,
        rollback_reason: Option<RollbackReason>,
    ) -> anyhow::Result<&XactStatus> {
        match self {
            Self::Uninitialized => anyhow::bail!("Xact state is uninitialized"),
            Self::Local(xact) => xact
                .add_vote(from, rollback_reason)
                .await
                .context("Failed to add vote for local xact"),
            Self::Surrogate(xact) => xact
                .add_vote(from, rollback_reason)
                .await
                .context("Failed to add vote for surrogate xact"),
        }
    }
}

pub struct XactState<C: XactController> {
    xact_id: XactId,
    node_id: NodeId,
    coordinator: NodeId,
    status: XactStatus,
    participants: BitSet,
    voted: BitSet,
    controller: C,
}

impl<C: XactController> XactState<C> {
    fn new(
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        controller: C,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            xact_id,
            node_id,
            coordinator,
            status: XactStatus::Uninitialized,
            participants,
            voted: BitSet::new(),
            controller,
        })
    }

    pub async fn initialize(&mut self) -> anyhow::Result<&XactStatus> {
        ensure!(self.status == XactStatus::Uninitialized);

        self.status = XactStatus::Waiting;

        // If the current participant is not the coordinator, add a 'yes' vote for
        // the coordinator here. Otherwise, the vote for coordinator is added below.
        if self.node_id != self.coordinator {
            // The coordinator always votes to commit
            self.add_vote(self.coordinator, None).await?;
        }

        // Execute the transaction
        let rollback_reason = {
            let _timer = EXECUTION_DURATION
                .with_label_values(&[&self.node_id.to_string(), &self.coordinator.to_string()])
                .start_timer();

            self.controller.execute().await
        }
        .err()
        .map(|err| {
            // Must use the tokio_postgres module from bb8_postgres, instead of
            // directly from the tokio_postgres crate for this downcast to work
            match err.downcast_ref::<bb8_postgres::tokio_postgres::Error>() {
                Some(err) => err.into(),
                None => RollbackReason::Other(err.to_string()),
            }
        });

        if let Some(reason) = &rollback_reason {
            debug!(
                "Rolled back surrogate xact {}. Reason: {:?}",
                self.xact_id, reason
            );
        }

        self.add_vote(self.node_id, rollback_reason).await?;

        Ok(&self.status)
    }

    pub async fn add_vote(
        &mut self,
        from: NodeId,
        rollback_reason: Option<RollbackReason>,
    ) -> anyhow::Result<&XactStatus> {
        ensure!(self.status != XactStatus::Committed);

        if !self.participants.contains(from) {
            warn!(
                "Node {} is not a participant of xact {}",
                from, self.xact_id
            );
        }

        if self.status != XactStatus::Waiting {
            return Ok(&self.status);
        }

        if let Some(reason) = rollback_reason {
            self.rollback(from, reason).await?;
        } else if !self.voted.contains(from) {
            self.voted.insert(from);
            self.try_commit().await?;
        }
        Ok(&self.status)
    }

    async fn rollback(
        &mut self,
        from: NodeId,
        rollback_reason: RollbackReason,
    ) -> anyhow::Result<()> {
        ensure!(self.status == XactStatus::Waiting);

        let info = RollbackInfo(from, rollback_reason.clone());
        self.controller
            .rollback(&info)
            .await
            .context("Failed to rollback")?;
        self.status = XactStatus::Rollbacked(info);
        Ok(())
    }

    async fn try_commit(&mut self) -> anyhow::Result<()> {
        ensure!(self.status == XactStatus::Waiting);
        if self.voted != self.participants {
            return Ok(());
        }
        self.controller.commit().await.context("Failed to commit")?;
        self.status = XactStatus::Committed;
        Ok(())
    }

    pub fn participants(&self) -> Vec<NodeId> {
        self.participants.iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use crate::pg::XactController;

    use super::*;

    /// A fake controllers that trivially keeps track of transaction states
    struct TestXactController {
        rollback_on_execution: bool,
        executed: bool,
        committed: bool,
        rollbacked: bool,
    }

    impl TestXactController {
        fn assert(&self, executed: bool, committed: bool, rollbacked: bool) {
            assert_eq!(self.executed, executed);
            assert_eq!(self.committed, committed);
            assert_eq!(self.rollbacked, rollbacked);
        }
    }

    #[async_trait]
    impl XactController for TestXactController {
        async fn execute(&mut self) -> anyhow::Result<()> {
            self.executed = true;
            if self.rollback_on_execution {
                anyhow::bail!("rolled back");
            }
            Ok(())
        }

        async fn commit(&mut self) -> anyhow::Result<()> {
            self.committed = true;
            Ok(())
        }

        async fn rollback(&mut self, _info: &RollbackInfo) -> anyhow::Result<()> {
            self.rollbacked = true;
            Ok(())
        }
    }

    fn new_test_xact_state(
        node_id: NodeId,
        coordinator: NodeId,
        participants: Vec<NodeId>,
        rollback_on_execution: bool,
    ) -> XactState<TestXactController> {
        let mut participant_set = BitSet::new();
        for p in participants {
            participant_set.insert(p);
        }
        XactState {
            xact_id: 100,
            node_id,
            coordinator,
            controller: TestXactController {
                rollback_on_execution,
                executed: false,
                committed: false,
                rollbacked: false,
            },
            status: XactStatus::Uninitialized,
            participants: participant_set,
            voted: BitSet::new(),
        }
    }

    fn is_rollbacked(reason: &XactStatus) -> bool {
        matches!(
            reason,
            &XactStatus::Rollbacked(RollbackInfo(_, RollbackReason::Other(_)))
        )
    }

    #[tokio::test]
    async fn test_1_participant() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(0, 0, vec![0], false);
        assert_eq!(state_1.initialize().await?, &XactStatus::Committed);
        state_1.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_1_participant_rollbacked() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(0, 0, vec![0], true);
        let status = state_1.initialize().await?;
        assert!(is_rollbacked(status), "Actual status: {:?}", status);
        state_1.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants() -> anyhow::Result<()> {
        let mut state = new_test_xact_state(1, 3, vec![1, 3, 5], false);
        assert_eq!(state.initialize().await?, &XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // Participant 3 already voted so nothing change
        assert_eq!(state.add_vote(3, None).await?, &XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // The last participant votes no abort so the transaction is committed
        assert_eq!(state.add_vote(5, None).await?, &XactStatus::Committed);
        state.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants_rollbacked() -> anyhow::Result<()> {
        let mut state = new_test_xact_state(2, 2, vec![0, 2, 4], false);
        assert_eq!(state.initialize().await?, &XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // Participant 0 vote to abort
        let status = state
            .add_vote(0, Some(RollbackReason::Other("".to_string())))
            .await?;

        assert!(is_rollbacked(status), "actual status: {:?}", status);
        state.controller.assert(true, false, true);

        // Transaction already rollbacked, further votes have no effect
        let status = state.add_vote(4, None).await?;
        assert!(is_rollbacked(status), "actual status: {:?}", status);
        state.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_participant() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(3, 2, vec![0, 1, 2], false);
        assert_eq!(state_1.initialize().await?, &XactStatus::Waiting);

        let mut state_2 = new_test_xact_state(2, 2, vec![0, 1, 2], false);
        assert_eq!(state_2.initialize().await?, &XactStatus::Waiting);
        assert_eq!(state_2.add_vote(4, None).await?, &XactStatus::Waiting);

        Ok(())
    }
}
