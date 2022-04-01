use aleph_bft_types::{Index, NodeIndex, SignatureSet};
use codec::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct VerboseSignature {
    msg: Vec<u8>,
    index: NodeIndex,
}

impl VerboseSignature {
    pub fn new(msg: Vec<u8>, index: NodeIndex) -> Self {
        Self { msg, index }
    }

    pub fn msg(&self) -> &Vec<u8> {
        &self.msg
    }
}

impl Index for VerboseSignature {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

pub type VerbosePartialMultisignature = SignatureSet<VerboseSignature>;
