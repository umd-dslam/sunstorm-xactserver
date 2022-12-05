use anyhow::{bail, ensure, Context};
use bit_set::BitSet;
use bytes::{Buf, Bytes};
use log::error;
use std::convert::TryInto;
use std::mem::size_of;
use tokio::sync::oneshot;
use url::Url;

use crate::pg::{LocalXactController, SurrogateXactController, XactController};
use crate::{NodeId, XactId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XactStatus {
    Uninitialized,
    Waiting,
    Committed,
    Rollbacked,
}

pub struct XactState<C: XactController> {
    pub id: XactId,
    pub rwset: RWSet,
    status: XactStatus,
    participants: BitSet,
    voted: BitSet,
    controller: C,
}

impl XactState<LocalXactController> {
    pub fn new(id: XactId, data: Bytes, commit_tx: oneshot::Sender<bool>) -> anyhow::Result<Self> {
        let controller = LocalXactController::new(commit_tx);
        Self::new_internal(id, data, controller)
    }
}

impl XactState<SurrogateXactController> {
    pub fn new(id: XactId, data: Bytes, connect_pg: &Url) -> anyhow::Result<Self> {
        let controller = SurrogateXactController::new(id, connect_pg.clone(), data.clone());
        Self::new_internal(id, data, controller)
    }
}

impl<C: XactController> XactState<C> {
    fn new_internal(id: XactId, data: Bytes, controller: C) -> anyhow::Result<Self> {
        let rwset = RWSet::decode(data).context("Failed to decode read/write set")?;
        let mut participants = BitSet::new();
        for i in 0..u64::BITS {
            if (rwset.header.region_set >> i) & 1 == 1 {
                participants.insert(i.try_into()?);
            }
        }
        Ok(Self {
            id,
            rwset,
            status: XactStatus::Uninitialized,
            participants,
            voted: BitSet::new(),
            controller,
        })
    }

    pub async fn initialize(
        &mut self,
        coordinator: NodeId,
        me: NodeId,
    ) -> anyhow::Result<XactStatus> {
        ensure!(self.status == XactStatus::Uninitialized);
        self.status = XactStatus::Waiting;

        if me != coordinator {
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

        self.add_vote(me, aborted).await?;

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
        self.controller.rollback().await?;
        self.status = XactStatus::Rollbacked;
        Ok(())
    }

    async fn try_commit(&mut self) -> anyhow::Result<()> {
        ensure!(self.status == XactStatus::Waiting);
        if self.voted != self.participants {
            return Ok(());
        }
        self.controller.commit().await?;
        self.status = XactStatus::Committed;
        Ok(())
    }

    pub fn participants(&self) -> Vec<NodeId> {
        self.participants.iter().collect()
    }
    pub fn voted(&self) -> Vec<NodeId> {
        self.voted.iter().collect()
    }
}

type Oid = u32;

#[derive(Debug, Default)]
pub struct RWSet {
    header: RWSetHeader,
    relations_len: usize,
    n_relations: u32,
    relations: Option<Vec<Relation>>,
    remainder: Bytes,
    decoded_all: bool,
}

impl RWSet {
    fn decode(mut buf: Bytes) -> anyhow::Result<RWSet> {
        let header = RWSetHeader::decode(&mut buf).context("Failed to decode header")?;
        let relations_len = get_u32(&mut buf)
            .context("Failed to decode length")?
            .try_into()?;
        ensure!(
            buf.remaining() >= relations_len,
            "Relations section too short. Expected: {}. Remaining: {}",
            relations_len,
            buf.remaining(),
        );
        let n_relations = get_u32(&mut buf).context("Failed to decode number of relations")?;
        Ok(Self {
            header,
            relations_len,
            n_relations,
            relations: None,
            remainder: buf,
            decoded_all: false,
        })
    }

    fn decode_relations(
        buf: &mut Bytes,
        relations_len: usize,
        n_relations: u32,
    ) -> anyhow::Result<Vec<Relation>> {
        let mut relations_buf = buf.split_to(relations_len);
        let mut relations = Vec::new();
        while relations_buf.has_remaining() {
            let r = Relation::decode(&mut relations_buf).with_context(|| {
                format!(
                    "Failed to decode relation. Relations decoded: {}",
                    relations.len()
                )
            })?;
            relations.push(r);
        }
        ensure!(
            n_relations == relations.len() as u32,
            "Failed to receive relations. Expected: {}, Received:{}",
            n_relations,
            relations.len(),
        );
        Ok(relations)
    }

    /// Decode everything on demand for debugging
    pub fn decode_rest(&mut self) -> anyhow::Result<&Self> {
        if self.decoded_all {
            return Ok(self);
        }
        self.relations = Some(
            Self::decode_relations(&mut self.remainder, self.relations_len, self.n_relations)
                .context("Failed to decode relations")?,
        );
        self.decoded_all = true;
        Ok(self)
    }
}

#[allow(dead_code)]
#[derive(Debug, Default)]
struct RWSetHeader {
    dbid: Oid,
    region_set: u64,
}

impl RWSetHeader {
    pub fn decode(buf: &mut Bytes) -> anyhow::Result<RWSetHeader> {
        let dbid = get_u32(buf).context("Failed to decode 'dbid'")?;
        let region_set = get_u64(buf).context("Failed to decode 'region_set'")?;

        Ok(Self { dbid, region_set })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
enum Relation {
    Table {
        oid: u32,
        region: u8,
        csn: u64,
        is_table_scan: bool,
        n_tuples: u32,
        tuples: Vec<Tuple>,
    },
    Index {
        oid: u32,
        region: u8,
        csn: u64,
        n_pages: u32,
        pages: Vec<Page>,
    },
}

#[allow(dead_code)]
#[derive(Debug)]
struct Tuple {
    blocknum: u32,
    offset: u16,
}

#[allow(dead_code)]
#[derive(Debug)]
struct Page {
    blocknum: u32,
}

impl Relation {
    fn decode(buf: &mut Bytes) -> anyhow::Result<Relation> {
        match get_u8(buf).context("Failed to decode relation type")? {
            b'T' => Relation::decode_relation(buf, false).context("Failed to decode table"),
            b'I' => Relation::decode_relation(buf, true).context("Failed to decode index"),
            other => bail!("Invalid relation type: {}", other),
        }
    }

    fn decode_relation(buf: &mut Bytes, is_index: bool) -> anyhow::Result<Relation> {
        let relid = get_u32(buf).context("Failed to decode 'relid'")?;
        let region = get_u8(buf).context("Failed to decode 'region'")?;
        let csn = get_u64(buf).context("Failed to decode 'csn'")?;
        let is_table_scan = get_u8(buf).context("Failed to decode 'is_table_scan'")?;
        let nitems = get_u32(buf).context("Failed to decode 'nitems'")?;

        if is_index {
            let n_pages = nitems;
            let mut pages = vec![];
            for _ in 0..nitems {
                pages.push(Relation::decode_page(buf).with_context(|| {
                    format!("Failed to decode page. Pages decoded: {}", pages.len())
                })?);
            }

            Ok(Relation::Index {
                oid: relid,
                region,
                csn,
                n_pages,
                pages,
            })
        } else {
            let n_tuples = nitems;
            let mut tuples = vec![];
            for _ in 0..nitems {
                tuples.push(Relation::decode_tuple(buf).with_context(|| {
                    format!("Failed to decode tuple. Tuples decoded: {}", tuples.len())
                })?);
            }

            Ok(Relation::Table {
                oid: relid,
                region,
                csn,
                is_table_scan: is_table_scan != 0,
                n_tuples,
                tuples,
            })
        }
    }

    fn decode_tuple(buf: &mut Bytes) -> anyhow::Result<Tuple> {
        let blocknum = get_u32(buf).context("Failed to decode 'blocknum'")?;
        let offset = get_u16(buf).context("Failed to decode 'offset'")?;

        Ok(Tuple { blocknum, offset })
    }

    fn decode_page(buf: &mut Bytes) -> anyhow::Result<Page> {
        let blocknum = get_u32(buf).context("Failed to decode 'blocknum'")?;

        Ok(Page { blocknum })
    }
}

macro_rules! new_get_num_fn {
    ($name: ident, $type: ident) => {
        fn $name(buf: &mut Bytes) -> anyhow::Result<$type> {
            ensure!(
                buf.remaining() >= size_of::<$type>(),
                "Required bytes: {}. Remaining bytes: {}",
                size_of::<$type>(),
                buf.remaining(),
            );
            Ok(buf.$name())
        }
    };
}

new_get_num_fn!(get_u8, u8);
new_get_num_fn!(get_u16, u16);
new_get_num_fn!(get_u32, u32);
new_get_num_fn!(get_u64, u64);

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
            rwset: RWSet::default(),
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

    #[tokio::test]
    async fn test_1_participant() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(vec![0], false);
        // Initialize with participant 0 (coordinator and myself)
        assert_eq!(state_1.initialize(0, 0).await?, XactStatus::Committed);
        state_1.controller.assert(true, true, false);

        let mut state_2 = new_test_xact_state(vec![4], false);
        // Initialize with participant 4 (myself and coordinator)
        assert_eq!(state_2.initialize(4, 4).await?, XactStatus::Committed);
        state_2.controller.assert(true, true, false);

        Ok(())
    }

    #[tokio::test]
    async fn test_1_participant_rollbacked() -> anyhow::Result<()> {
        let mut state_1 = new_test_xact_state(vec![0], true);
        // Initialize with participant 0 (coordinator and myself)
        assert_eq!(state_1.initialize(0, 0).await?, XactStatus::Rollbacked);
        state_1.controller.assert(true, false, true);

        Ok(())
    }

    #[tokio::test]
    async fn test_3_participants() -> anyhow::Result<()> {
        let mut state = new_test_xact_state(vec![1, 3, 5], false);

        // Initialize with participants 1 (coordinator) and 3 (myself)
        assert_eq!(state.initialize(1, 3).await?, XactStatus::Waiting);
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

        // Initialize with participants 2 (coordinator and myself)
        assert_eq!(state.initialize(2, 2).await?, XactStatus::Waiting);
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
        let res_1 = state_1.initialize(3, 3).await;
        assert!(res_1.is_err());

        let mut state_2 = new_test_xact_state(vec![0, 1, 2], false);
        assert_eq!(state_2.initialize(2, 2).await?, XactStatus::Waiting);
        let res_2 = state_2.add_vote(4, false).await;
        assert!(res_2.is_err());

        Ok(())
    }
}
