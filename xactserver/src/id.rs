use std::{
    fmt::{self, Debug, Display, Formatter},
    mem,
    str::FromStr,
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct XactId(pub u64);

impl XactId {
    pub const INVALID: XactId = Self(0);

    pub fn new(counter: u32, node_id: NodeId) -> Self {
        Self((counter as u64) << NodeId::BIT_LENGTH | node_id.0 as u64)
    }

    pub fn counter(self) -> u32 {
        (self.0 >> NodeId::BIT_LENGTH) as u32
    }

    pub fn node_id(self) -> NodeId {
        NodeId((self.0 & ((1 << NodeId::BIT_LENGTH) - 1)) as u32)
    }
}

impl Debug for XactId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "XactId({}, {:?})", self.counter(), self.node_id())
    }
}

impl Display for XactId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "({},{})", self.counter(), self.node_id())
    }
}

impl From<XactId> for u64 {
    fn from(txn_id: XactId) -> Self {
        txn_id.0
    }
}

impl From<u64> for XactId {
    fn from(txn_id: u64) -> Self {
        Self(txn_id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const BIT_LENGTH: usize = mem::size_of::<u32>() * 8;
}

impl Display for NodeId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<NodeId> for u32 {
    fn from(node_id: NodeId) -> Self {
        node_id.0
    }
}

impl From<NodeId> for usize {
    fn from(node_id: NodeId) -> Self {
        node_id.0 as usize
    }
}

impl From<u32> for NodeId {
    fn from(node_id: u32) -> Self {
        Self(node_id)
    }
}

impl From<usize> for NodeId {
    fn from(node_id: usize) -> Self {
        Self(node_id as u32)
    }
}

impl FromStr for NodeId {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let node_id = s.parse::<u32>()?;
        Ok(Self(node_id))
    }
}
