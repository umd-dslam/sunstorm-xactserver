mod state;

use crate::pg::{LocalXactController, PgConnectionPool, SurrogateXactController};
use crate::{NodeId, RollbackInfo, Vote, XactId, XactStatus};
use anyhow::Context;
use bit_set::BitSet;
use bytes::Bytes;
use state::XactState;
use tokio::sync::oneshot;

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
        xact_id: XactId,
        node_id: NodeId,
        coordinator: NodeId,
        participants: BitSet,
        commit_tx: oneshot::Sender<Option<RollbackInfo>>,
    ) -> anyhow::Result<&XactStatus> {
        let controller = LocalXactController::new(commit_tx);
        let new_state = XactState::<LocalXactController>::new(
            xact_id,
            node_id,
            coordinator,
            participants,
            controller,
        );

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
        let new_state = XactState::<SurrogateXactController>::new(
            xact_id,
            node_id,
            coordinator,
            participants,
            controller,
        );

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
