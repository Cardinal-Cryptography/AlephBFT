use crate::ThresholdMultiKeychain;
use aleph_bft_types::{Index, KeyBox, NodeCount, NodeIndex, SignatureSet};
use async_trait::async_trait;
use codec::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct DummySignature {}

pub type DummyPartialMultisignature = SignatureSet<DummySignature>;

#[derive(Clone)]
pub struct DummyKeyBox {
    count: NodeCount,
    ix: NodeIndex,
}

impl DummyKeyBox {
    pub fn new(count: NodeCount, ix: NodeIndex) -> Self {
        DummyKeyBox { count, ix }
    }
}

impl Index for DummyKeyBox {
    fn index(&self) -> NodeIndex {
        self.ix
    }
}

#[async_trait]
impl KeyBox for DummyKeyBox {
    type Signature = DummySignature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, _msg: &[u8]) -> Self::Signature {
        Self::Signature {}
    }

    fn verify(&self, _msg: &[u8], _sgn: &Self::Signature, _index: NodeIndex) -> bool {
        true
    }
}

pub type ThresholdDummyMultiKeychain = ThresholdMultiKeychain<DummyKeyBox>;

impl ThresholdDummyMultiKeychain {
    pub fn new(count: NodeCount, ix: NodeIndex) -> Self {
        DummyKeyBox { count, ix }.into()
    }
}
