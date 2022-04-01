use aleph_bft_types::{Index, KeyBox, NodeCount, NodeIndex, Signature};
use async_trait::async_trait;
use std::fmt::Debug;

#[derive(Clone, Debug)]
pub struct ConstantSignatureKeyBox<T: Signature> {
    count: NodeCount,
    index: NodeIndex,
    signature: T,
}

impl<T: Signature> ConstantSignatureKeyBox<T> {
    pub fn new(count: NodeCount, index: NodeIndex, signature: T) -> Self {
        ConstantSignatureKeyBox {
            count,
            index,
            signature,
        }
    }
}

impl<T: Signature> Index for ConstantSignatureKeyBox<T> {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl<T: Signature> KeyBox for ConstantSignatureKeyBox<T> {
    type Signature = T;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, _msg: &[u8]) -> Self::Signature {
        self.signature.clone()
    }

    fn verify(&self, _msg: &[u8], sgn: &Self::Signature, _index: NodeIndex) -> bool {
        self.signature == *sgn
    }
}
