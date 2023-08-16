use super::XactStatus;
use crate::metrics::EXECUTION_DURATION;
use crate::pg::XactController;
use crate::{NodeId, RollbackInfo, RollbackReason, Vote, XactId};
use anyhow::{ensure, Context};
use bit_set::BitSet;
use tracing::{debug, warn};

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
    pub(super) fn new(
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        controller: C,
    ) -> Self {
        Self {
            xact_id,
            node_id,
            coordinator,
            status: XactStatus::Uninitialized,
            participants,
            voted: BitSet::new(),
            controller,
        }
    }

    pub(super) async fn initialize(
        &mut self,
        existing_votes: Vec<Vote>,
    ) -> anyhow::Result<&XactStatus> {
        ensure!(self.status == XactStatus::Uninitialized);

        self.status = XactStatus::Waiting;

        let is_local = self.node_id == self.coordinator;
        let mut rollback_reason = None;
        if !is_local {
            // If the current participant is not the coordinator, add a 'yes' vote for
            // the coordinator here.
            self.add_vote(Vote::yes(self.coordinator)).await?;

            // Execute the transaction
            rollback_reason = {
                let _timer = EXECUTION_DURATION
                    .with_label_values(&[
                        &self.node_id.to_string(),
                        &self.coordinator.to_string(),
                        &is_local.to_string(),
                    ])
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
        }

        // Add a vote for the current node
        self.add_vote(Vote {
            from: self.node_id,
            rollback_reason,
        })
        .await?;

        // Add existing votes
        for vote in existing_votes {
            self.add_vote(vote).await?;
        }

        Ok(&self.status)
    }

    pub(super) async fn add_vote(&mut self, vote: Vote) -> anyhow::Result<&XactStatus> {
        ensure!(self.status != XactStatus::Committing && self.status != XactStatus::Committed);

        if !self.participants.contains(vote.from.into()) {
            warn!(
                "Node {} is not a participant of xact {}",
                vote.from, self.xact_id
            );
        }

        if self.status != XactStatus::Waiting {
            return Ok(&self.status);
        }

        if let Some(reason) = vote.rollback_reason {
            self.status = XactStatus::Rollbacking(RollbackInfo(vote.from, reason));
        } else if !self.voted.contains(vote.from.into()) {
            self.voted.insert(vote.from.into());
            if self.voted == self.participants {
                self.status = XactStatus::Committing;
            }
        }
        Ok(&self.status)
    }

    pub(super) async fn try_finish(&mut self) -> anyhow::Result<&XactStatus> {
        match &self.status {
            XactStatus::Committing => {
                self.controller.commit().await.context("Failed to commit")?;
                self.status = XactStatus::Committed;
            }
            XactStatus::Rollbacking(info) => {
                self.controller
                    .rollback(info)
                    .await
                    .context("Failed to rollback")?;
                self.status = XactStatus::Rollbacked(info.clone());
            }
            XactStatus::Committed
            | XactStatus::Rollbacked(_)
            | XactStatus::Waiting
            | XactStatus::Uninitialized => {
                // Do nothing
            }
        }
        Ok(&self.status)
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
            assert_eq!(self.executed, executed, "wrong execution status");
            assert_eq!(self.committed, committed, "wrong commit status");
            assert_eq!(self.rollbacked, rollbacked, "wrong rollback status");
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

    #[derive(Default)]
    struct XactStateBuilder {
        node_id: NodeId,
        coordinator: NodeId,
        participants: Vec<NodeId>,
        rollback_on_execution: bool,
    }

    impl XactStateBuilder {
        fn new() -> Self {
            Self {
                participants: vec![NodeId(0)],
                ..Default::default()
            }
        }

        fn with_node_id(mut self, node_id: NodeId) -> Self {
            self.node_id = node_id;
            self
        }

        fn with_coordinator(mut self, coordinator: NodeId) -> Self {
            self.coordinator = coordinator;
            self
        }

        fn with_participants(mut self, participants: Vec<NodeId>) -> Self {
            self.participants = participants;
            self
        }

        fn with_rollback_on_execution(mut self, rollback_on_execution: bool) -> Self {
            self.rollback_on_execution = rollback_on_execution;
            self
        }

        fn build(self) -> XactState<TestXactController> {
            let mut participant_set = BitSet::new();
            for p in self.participants {
                participant_set.insert(p.into());
            }
            XactState {
                xact_id: XactId(100),
                node_id: self.node_id,
                coordinator: self.coordinator,
                controller: TestXactController {
                    rollback_on_execution: self.rollback_on_execution,
                    executed: false,
                    committed: false,
                    rollbacked: false,
                },
                status: XactStatus::Uninitialized,
                participants: participant_set,
                voted: BitSet::new(),
            }
        }
    }

    fn is_rollbacking(reason: &XactStatus) -> bool {
        matches!(
            reason,
            &XactStatus::Rollbacking(RollbackInfo(_, RollbackReason::Other(_)))
        )
    }

    fn is_rollbacked(reason: &XactStatus) -> bool {
        matches!(
            reason,
            &XactStatus::Rollbacked(RollbackInfo(_, RollbackReason::Other(_)))
        )
    }

    #[tokio::test]
    async fn test_1_participant() -> anyhow::Result<()> {
        let mut state_1 = XactStateBuilder::new().build();

        assert_eq!(state_1.initialize(vec![]).await?, &XactStatus::Committing);
        assert_eq!(state_1.try_finish().await?, &XactStatus::Committed);
        // There is only one participant and the transaction is local so it is immediately
        // committed without executing.
        state_1.controller.assert(false, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_2_participants_rollbacked() -> anyhow::Result<()> {
        let mut state_1 = XactStateBuilder::new()
            .with_node_id(NodeId(1))
            .with_coordinator(NodeId(0))
            .with_participants(vec![NodeId(0), NodeId(1)])
            .with_rollback_on_execution(true)
            .build();

        let status = state_1.initialize(vec![]).await?;
        assert!(is_rollbacking(status), "Actual status: {:?}", status);
        let status = state_1.try_finish().await?;
        assert!(is_rollbacked(status), "Actual status: {:?}", status);
        state_1.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants() -> anyhow::Result<()> {
        let mut state = XactStateBuilder::new()
            .with_node_id(NodeId(1))
            .with_coordinator(NodeId(3))
            .with_participants(vec![NodeId(1), NodeId(3), NodeId(5)])
            .build();

        assert_eq!(state.initialize(vec![]).await?, &XactStatus::Waiting);
        state.controller.assert(true, false, false);

        // Participant 3 already voted so nothing change
        assert_eq!(
            state.add_vote(Vote::yes(NodeId(3))).await?,
            &XactStatus::Waiting
        );
        state.controller.assert(true, false, false);

        // The last participant votes no abort so the transaction is committed
        assert_eq!(
            state.add_vote(Vote::yes(NodeId(5))).await?,
            &XactStatus::Committing
        );
        state.controller.assert(true, false, false);

        assert_eq!(state.try_finish().await?, &XactStatus::Committed);
        state.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_4_participants_rollbacked() -> anyhow::Result<()> {
        let mut state = XactStateBuilder::new()
            .with_node_id(NodeId(2))
            .with_coordinator(NodeId(2))
            .with_participants(vec![NodeId(0), NodeId(2), NodeId(4), NodeId(6)])
            .build();

        assert_eq!(state.initialize(vec![]).await?, &XactStatus::Waiting);
        state.controller.assert(false, false, false);

        // Participant 0 vote to abort
        let status = state
            .add_vote(Vote::no(NodeId(0), RollbackReason::Other("".to_string())))
            .await?;

        assert!(is_rollbacking(status), "actual status: {:?}", status);
        state.controller.assert(false, false, false);

        // Transaction is rollbacking, further votes have no effect
        let status = state.add_vote(Vote::yes(NodeId(4))).await?;
        assert!(is_rollbacking(status), "actual status: {:?}", status);
        state.controller.assert(false, false, false);

        let status = state.try_finish().await?;
        assert!(is_rollbacked(status), "actual status: {:?}", status);
        state.controller.assert(false, false, true);

        // Transaction is rollbacked, further votes have no effect
        let status = state.add_vote(Vote::yes(NodeId(6))).await?;
        assert!(is_rollbacked(status), "actual status: {:?}", status);
        state.controller.assert(false, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_participant() -> anyhow::Result<()> {
        let mut state_1 = XactStateBuilder::new()
            .with_node_id(NodeId(3))
            .with_coordinator(NodeId(2))
            .with_participants(vec![NodeId(0), NodeId(1), NodeId(2)])
            .build();

        assert_eq!(state_1.initialize(vec![]).await?, &XactStatus::Waiting);

        let mut state_2 = XactStateBuilder::new()
            .with_node_id(NodeId(2))
            .with_coordinator(NodeId(2))
            .with_participants(vec![NodeId(0), NodeId(1), NodeId(2)])
            .build();

        assert_eq!(state_2.initialize(vec![]).await?, &XactStatus::Waiting);
        assert_eq!(
            state_2.add_vote(Vote::yes(NodeId(4))).await?,
            &XactStatus::Waiting
        );

        Ok(())
    }
}
