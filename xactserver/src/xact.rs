use anyhow::{bail, ensure, Context};
use bit_set::BitSet;
use bytes::{Buf, Bytes};
use std::convert::TryInto;
use std::mem::size_of;
use std::net::SocketAddr;
use tokio::sync::oneshot;

use crate::pg::{LocalXactController, SurrogateXactController, XactController};
use crate::{NodeId, XactId};

#[derive(Clone, Copy, PartialEq)]
pub enum XactStatus {
    Uninitialized,
    Waiting,
    Commit,
    Abort,
}

pub struct XactState<C: XactController> {
    pub id: XactId,
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
    pub fn new(id: XactId, data: Bytes, connect_pg: SocketAddr) -> anyhow::Result<Self> {
        let controller = SurrogateXactController::new(id, connect_pg, data.clone());
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
        // Execute the transaction and set status to waiting
        let commit_vote = self.controller.execute().await?;
        self.status = XactStatus::Waiting;

        // Add votes if pass validation
        self.add_vote(me, !commit_vote).await?;
        if commit_vote && me != coordinator {
            self.add_vote(coordinator, false).await?;
        }

        Ok(self.status)
    }

    pub async fn add_vote(&mut self, from: NodeId, abort: bool) -> anyhow::Result<XactStatus> {
        ensure!(self.status == XactStatus::Waiting);
        ensure!(self.participants.contains(from));
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
        self.status = XactStatus::Abort;
        Ok(())
    }

    async fn try_commit(&mut self) -> anyhow::Result<()> {
        ensure!(self.status == XactStatus::Waiting);
        if self.voted == self.participants {
            return Ok(());
        }
        self.controller.commit().await?;
        self.status = XactStatus::Commit;
        Ok(())
    }

    pub fn participants(&self) -> Vec<NodeId> {
        self.participants.iter().collect()
    }
}

type Oid = u32;

#[derive(Debug)]
pub struct RWSet {
    header: RWSetHeader,
    relations: Option<Vec<Relation>>,
    remainder: Bytes,
}

impl RWSet {
    fn decode(mut buf: Bytes) -> anyhow::Result<RWSet> {
        let header = RWSetHeader::decode(&mut buf).context("Failed to decode header")?;
        Ok(Self {
            header,
            relations: None,
            remainder: buf,
        })
    }

    fn decode_relations(buf: &mut Bytes) -> anyhow::Result<Vec<Relation>> {
        let relations_len = get_u32(buf)
            .context("Failed to decode length")?
            .try_into()?;
        ensure!(
            buf.remaining() >= relations_len,
            "Relations section too short. Expected: {}. Remaining: {}",
            relations_len,
            buf.remaining(),
        );
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
        Ok(relations)
    }

    /// Decode everything on demand for debugging
    pub fn decode_all(&mut self) -> anyhow::Result<&Self> {
        self.relations = Some(
            Self::decode_relations(&mut self.remainder).context("Failed to decode relations")?,
        );
        Ok(self)
    }
}

#[allow(dead_code)]
#[derive(Debug)]
struct RWSetHeader {
    dbid: Oid,
    xid: u32,
    region_set: u64,
}

impl RWSetHeader {
    pub fn decode(buf: &mut Bytes) -> anyhow::Result<RWSetHeader> {
        let dbid = get_u32(buf).context("Failed to decode 'dbid'")?;
        let xid = get_u32(buf).context("Failed to decode 'xid'")?;
        let region_set = get_u64(buf).context("Failed to decode 'region_set'")?;

        Ok(Self {
            dbid,
            xid,
            region_set,
        })
    }
}

#[allow(dead_code)]
#[derive(Debug)]
enum Relation {
    Table {
        oid: u32,
        csn: u32,
        tuples: Vec<Tuple>,
    },
    Index {
        oid: u32,
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
    csn: u32,
}

impl Relation {
    fn decode(buf: &mut Bytes) -> anyhow::Result<Relation> {
        match get_u8(buf).context("Failed to decode relation type")? {
            b'T' => Relation::decode_table(buf).context("Failed to decode table"),
            b'I' => Relation::decode_index(buf).context("Failed to decode index"),
            other => bail!("Invalid relation type: {}", other),
        }
    }

    fn decode_table(buf: &mut Bytes) -> anyhow::Result<Relation> {
        let relid = get_u32(buf).context("Failed to decode 'relid'")?;
        let ntuples = get_u32(buf).context("Failed to decode 'ntuples'")?;
        let csn = get_u32(buf).context("Failed to decode 'csn'")?;
        let mut tuples = vec![];
        for _ in 0..ntuples {
            tuples.push(Relation::decode_tuple(buf).with_context(|| {
                format!("Failed to decode tuple. Tuples decoded: {}", tuples.len())
            })?);
        }

        Ok(Relation::Table {
            oid: relid,
            csn,
            tuples,
        })
    }

    fn decode_tuple(buf: &mut Bytes) -> anyhow::Result<Tuple> {
        let blocknum = get_u32(buf).context("Failed to decode 'blocknum'")?;
        let offset = get_u16(buf).context("Failed to decode 'offset'")?;

        Ok(Tuple { blocknum, offset })
    }

    fn decode_index(buf: &mut Bytes) -> anyhow::Result<Relation> {
        let relid = get_u32(buf).context("Failed to decode 'relid'")?;
        let npages = get_u32(buf).context("Failed to decode 'npages'")?;
        let mut pages = vec![];
        for _ in 0..npages {
            pages.push(Relation::decode_page(buf).with_context(|| {
                format!("Failed to decode page. Pages decoded: {}", pages.len())
            })?);
        }

        Ok(Relation::Index { oid: relid, pages })
    }

    fn decode_page(buf: &mut Bytes) -> anyhow::Result<Page> {
        let blocknum = get_u32(buf).context("Failed to decode 'blocknum'")?;
        let csn = get_u32(buf).context("Failed to decode 'csn'")?;

        Ok(Page { blocknum, csn })
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
