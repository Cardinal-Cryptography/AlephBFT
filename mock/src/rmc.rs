use crate::{
    crypto::{VerboseKeyBox, VerboseSignature},
    threshold_multi_keychain::DefaultMultiKeychain,
};
use aleph_bft_types::{Index, KeyBox, MultiKeychain, NodeCount, NodeIndex, Signable, SignatureSet};
use async_trait::async_trait;
use std::fmt::Debug;

pub type TestMultiKeychain = DefaultMultiKeychain<VerboseKeyBox>;

pub type VerbosePartialMultisignature = SignatureSet<VerboseSignature>;

pub fn prepare_keychains(node_count: NodeCount) -> Vec<TestMultiKeychain> {
    (0..node_count.0)
        .map(|i| DefaultMultiKeychain::new(VerboseKeyBox::new(node_count, i.into())))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignableByte(u8);

impl Signable for SignableByte {
    type Hash = [u8; 1];
    fn hash(&self) -> Self::Hash {
        [self.0]
    }
}

impl From<u8> for SignableByte {
    fn from(x: u8) -> Self {
        Self(x)
    }
}

#[derive(Clone, Debug)]
pub struct BadKeyBox {
    count: NodeCount,
    index: NodeIndex,
    signature_index: NodeIndex,
}

impl BadKeyBox {
    pub fn new(count: NodeCount, index: NodeIndex, signature_index: NodeIndex) -> Self {
        BadKeyBox {
            count,
            index,
            signature_index,
        }
    }
}

impl Index for BadKeyBox {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl KeyBox for BadKeyBox {
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

impl MultiKeychain for BadKeyBox {
    type PartialMultisignature = SignatureSet<VerboseSignature>;

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
