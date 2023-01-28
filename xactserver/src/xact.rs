use anyhow::{ensure, Context};
use bit_set::BitSet;
use bytes::Bytes;
use log::error;
use tokio::sync::oneshot;

use crate::metrics::EXECUTION_DURATION;
use crate::pg::{LocalXactController, PgConnectionPool, SurrogateXactController, XactController};
use crate::{NodeId, XactId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XactStatus {
    Unexecuted,
    Waiting,
    Committed,
    Rollbacked,
}

pub enum XactStateType {
    Uninitialized,
    Local(XactState<LocalXactController>),
    Surrogate(XactState<SurrogateXactController>),
}

impl XactStateType {
    pub fn new_local_xact(
        id: XactId,
        participants: BitSet,
        commit_tx: oneshot::Sender<bool>,
    ) -> anyhow::Result<Self> {
        let controller = LocalXactController::new(commit_tx);
        Ok(Self::Local(XactState::<LocalXactController>::new(
            id,
            participants,
            controller,
        )?))
    }

    pub fn new_surrogate_xact(
        id: XactId,
        participants: BitSet,
        data: Bytes,
        pg_conn_pool: &PgConnectionPool,
    ) -> anyhow::Result<Self> {
        let controller = SurrogateXactController::new(id, data, pg_conn_pool.clone());
        Ok(Self::Surrogate(XactState::<SurrogateXactController>::new(
            id,
            participants,
            controller,
        )?))
    }

    pub async fn execute(
        &mut self,
        region: NodeId,
        coordinator: NodeId,
    ) -> anyhow::Result<XactStatus> {
        let _timer = EXECUTION_DURATION
            .with_label_values(&[&region.to_string(), &coordinator.to_string()])
            .start_timer();
        match self {
            Self::Uninitialized => anyhow::bail!("Xact state is uninitialized"),
            Self::Local(xact) => xact
                .execute(region, coordinator)
                .await
                .with_context(|| format!("Failed to execute local xact {}", xact.id)),
            Self::Surrogate(xact) => xact
                .execute(region, coordinator)
                .await
                .with_context(|| format!("Failed to execute surrogate xact {}", xact.id)),
        }
    }

    pub async fn add_vote(&mut self, from: NodeId, abort: bool) -> anyhow::Result<XactStatus> {
        match self {
            Self::Uninitialized => anyhow::bail!("Xact state is uninitialized"),
            Self::Local(xact) => xact
                .add_vote(from, abort)
                .await
                .with_context(|| format!("Failed to add vote for local xact {}", xact.id)),
            Self::Surrogate(xact) => xact
                .add_vote(from, abort)
                .await
                .with_context(|| format!("Failed to add vote for surrogate xact {}", xact.id)),
        }
    }
}

pub struct XactState<C: XactController> {
    id: XactId,
    status: XactStatus,
    participants: BitSet,
    voted: BitSet,
    controller: C,
}

impl<C: XactController> XactState<C> {
    fn new(id: XactId, participants: BitSet, controller: C) -> anyhow::Result<Self> {
        Ok(Self {
            id,
            status: XactStatus::Unexecuted,
            participants,
            voted: BitSet::new(),
            controller,
        })
    }

    pub async fn execute(
        &mut self,
        region: NodeId,
        coordinator: NodeId,
    ) -> anyhow::Result<XactStatus> {
        ensure!(self.status == XactStatus::Unexecuted);
        self.status = XactStatus::Waiting;

        if region != coordinator {
            // The coordinator always votes to commit
            self.add_vote(coordinator, false).await?;
        }

        // Execute the transaction
        let aborted = self.controller.execute().await.map_or_else(
            |err| {
                // TODO: Bubble this error up from here
                error!("{:?}", err);
                true
            },
            |_| false,
        );

        self.add_vote(region, aborted).await?;

        Ok(self.status)
    }

    pub async fn add_vote(&mut self, from: NodeId, abort: bool) -> anyhow::Result<XactStatus> {
        ensure!(self.status != XactStatus::Committed);
        ensure!(
            self.participants.contains(from),
            "Node {} is not a participant of xact {}",
            from,
            self.id
        );
        if self.status != XactStatus::Waiting {
            return Ok(self.status);
        }
        if abort {
            self.rollback().await?;
        } else if !self.voted.contains(from) {
            self.voted.insert(from);
            self.try_commit().await?;
        }
        Ok(self.status)
    }

    async fn rollback(&mut self) -> anyhow::Result<()> {
        ensure!(self.status == XactStatus::Waiting);
        self.controller
            .rollback()
            .await
            .context("Failed to rollback")?;
        self.status = XactStatus::Rollbacked;
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

        async fn rollback(&mut self) -> anyhow::Result<()> {
            self.rollbacked = true;
            Ok(())
        }
    }

    fn new_test_xact_state(
        participants: Vec<NodeId>,
        rollback_on_execution: bool,
    ) -> XactState<TestXactController> {
        let mut participant_set = BitSet::new();
        for p in participants {
            participant_set.insert(p);
        }
        XactState {
            id: 100,
            controller: TestXactController {
                rollback_on_execution,
                executed: false,
                committed: false,
                rollbacked: false,
            },
            status: XactStatus::Unexecuted,
            participants: participant_set,
            voted: BitSet::new(),
        }
    }

    #[tokio::test]
    async fn test_1_participant() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(vec![0], false);
        // Execute with participant 0 (coordinator and myself)
        assert_eq!(state_1.execute(0, 0).await?, XactStatus::Committed);
        state_1.controller.assert(true, true, false);

        let mut state_2 = new_test_xact_state(vec![4], false);
        // Execute with participant 4 (myself and coordinator)
        assert_eq!(state_2.execute(4, 4).await?, XactStatus::Committed);
        state_2.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_1_participant_rollbacked() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(vec![0], true);
        // Execute with participant 0 (coordinator and myself)
        assert_eq!(state_1.execute(0, 0).await?, XactStatus::Rollbacked);
        state_1.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants() -> anyhow::Result<()> {
        let mut state = new_test_xact_state(vec![1, 3, 5], false);

        // Execute with participants 1 (coordinator) and 3 (myself)
        assert_eq!(state.execute(1, 3).await?, XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // Participant 3 already voted so nothing change
        assert_eq!(state.add_vote(3, false).await?, XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // The last participant votes no abort so the transaction is committed
        assert_eq!(state.add_vote(5, false).await?, XactStatus::Committed);
        state.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants_rollbacked() -> anyhow::Result<()> {
        let mut state = new_test_xact_state(vec![0, 2, 4], false);

        // Execute with participants 2 (coordinator and myself)
        assert_eq!(state.execute(2, 2).await?, XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // Participant 0 vote to abort
        assert_eq!(state.add_vote(0, true).await?, XactStatus::Rollbacked);
        state.controller.assert(true, false, true);

        // Transaction already rollbacked, further votes have no effect
        assert_eq!(state.add_vote(4, false).await?, XactStatus::Rollbacked);
        state.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_participant() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(vec![0, 1, 2], false);
        let res_1 = state_1.execute(3, 3).await;
        assert!(res_1.is_err());

        let mut state_2 = new_test_xact_state(vec![0, 1, 2], false);
        assert_eq!(state_2.execute(2, 2).await?, XactStatus::Waiting);
        let res_2 = state_2.add_vote(4, false).await;
        assert!(res_2.is_err());

        Ok(())
    }
}
