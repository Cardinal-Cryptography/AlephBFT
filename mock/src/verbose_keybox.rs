use crate::VerboseSignature;
use aleph_bft_types::{Index, KeyBox as KeyBoxT, NodeCount, NodeIndex};
use async_trait::async_trait;

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
        VerboseSignature::new(msg.to_vec(), self.index)
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        index == sgn.index() && msg == sgn.msg()
    }
}
