use anyhow::{bail, ensure, Context};
use bytes::{Buf, Bytes};
use std::mem::size_of;

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
    pub fn decode(mut buf: Bytes) -> anyhow::Result<RWSet> {
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

    pub fn participants(&self) -> bit_set::BitSet {
        self.header.participants.clone()
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
pub struct RWSetHeader {
    dbid: Oid,
    participants: bit_set::BitSet,
}

impl RWSetHeader {
    pub fn decode(buf: &mut Bytes) -> anyhow::Result<RWSetHeader> {
        let dbid = get_u32(buf).context("Failed to decode 'dbid'")?;
        let region_set = get_u64(buf).context("Failed to decode 'region_set'")?;

        let mut participants = bit_set::BitSet::new();
        for i in 0..u64::BITS {
            if (region_set >> i) & 1 == 1 {
                participants.insert(i.try_into()?);
            }
        }

        Ok(Self { dbid, participants })
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
