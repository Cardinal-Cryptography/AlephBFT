use aleph_bft_types::{
    Index, KeyBox as KeyBoxT, MultiKeychain as MultiKeychainT, NodeCount, NodeIndex,
    PartialMultisignature as PartialMultisignatureT, SignatureSet,
};
use async_trait::async_trait;
use codec::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct Signature {}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct PartialMultisignature {
    signed_by: Vec<NodeIndex>,
}

impl PartialMultisignatureT for PartialMultisignature {
    type Signature = Signature;
    fn add_signature(self, _: &Self::Signature, index: NodeIndex) -> Self {
        let Self { mut signed_by } = self;
        for id in &signed_by {
            if *id == index {
                return Self { signed_by };
            }
        }
        signed_by.push(index);
        Self { signed_by }
    }
}

#[derive(Clone)]
pub struct KeyBox {
    count: NodeCount,
    ix: NodeIndex,
}

impl KeyBox {
    pub fn new(count: NodeCount, ix: NodeIndex) -> Self {
        KeyBox { count, ix }
    }
}

impl Index for KeyBox {
    fn index(&self) -> NodeIndex {
        self.ix
    }
}

#[async_trait]
impl KeyBoxT for KeyBox {
    type Signature = Signature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, _msg: &[u8]) -> Signature {
        Signature {}
    }

    fn verify(&self, _msg: &[u8], _sgn: &Signature, _index: NodeIndex) -> bool {
        true
    }
}

impl MultiKeychainT for KeyBox {
    type PartialMultisignature = PartialMultisignature;
    fn from_signature(&self, _: &Self::Signature, index: NodeIndex) -> Self::PartialMultisignature {
        let signed_by = vec![index];
        PartialMultisignature { signed_by }
    }
    fn is_complete(&self, _: &[u8], partial: &Self::PartialMultisignature) -> bool {
        (self.count * 2) / 3 < NodeCount(partial.signed_by.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct VerboseSignature {
    msg: Vec<u8>,
    index: NodeIndex,
}

impl VerboseSignature {
    pub fn new(msg: Vec<u8>, index: NodeIndex) -> Self {
        Self { msg, index }
    }
}

pub type VerbosePartialMultisignature = SignatureSet<VerboseSignature>;

#[derive(Clone, Debug)]
pub struct VerboseKeyBox {
    count: NodeCount,
    index: NodeIndex,
}

impl VerboseKeyBox {
    pub fn new(count: NodeCount, index: NodeIndex) -> Self {
        VerboseKeyBox { count, index }
    }
}

impl Index for VerboseKeyBox {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl KeyBoxT for VerboseKeyBox {
    type Signature = VerboseSignature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        VerboseSignature {
            msg: msg.to_vec(),
            index: self.index,
        }
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        index == sgn.index && msg == sgn.msg
    }
}
