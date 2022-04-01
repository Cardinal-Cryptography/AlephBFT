use crate::crypto::{VerbosePartialMultisignature, VerboseSignature};
use aleph_bft_types::{Index, KeyBox, MultiKeychain, NodeCount, NodeIndex, SignatureSet};
use async_trait::async_trait;
use std::fmt::Debug;

#[derive(Clone, Debug)]
pub struct BadVerboseMultiKeychain {
    count: NodeCount,
    index: NodeIndex,
    signature_index: NodeIndex,
}

impl BadVerboseMultiKeychain {
    pub fn new(count: NodeCount, index: NodeIndex, signature_index: NodeIndex) -> Self {
        BadVerboseMultiKeychain {
            count,
            index,
            signature_index,
        }
    }
}

impl Index for BadVerboseMultiKeychain {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl KeyBox for BadVerboseMultiKeychain {
    type Signature = VerboseSignature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        VerboseSignature::new(msg.to_vec(), self.signature_index)
    }

    fn verify(&self, _msg: &[u8], _sgn: &Self::Signature, _index: NodeIndex) -> bool {
        true
    }
}

impl MultiKeychain for BadVerboseMultiKeychain {
    type PartialMultisignature = VerbosePartialMultisignature;

    fn from_signature(
        &self,
        _signature: &Self::Signature,
        _index: NodeIndex,
    ) -> Self::PartialMultisignature {
        SignatureSet::with_size(self.count)
    }

    fn is_complete(&self, _msg: &[u8], _partial: &Self::PartialMultisignature) -> bool {
        true
    }
}
