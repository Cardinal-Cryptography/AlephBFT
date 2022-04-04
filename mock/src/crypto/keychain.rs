use crate::crypto::Signature;
use aleph_bft_types::{Index, KeyBox as KeyBoxT, NodeCount, NodeIndex};
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct Keychain {
    count: NodeCount,
    index: NodeIndex,
}

impl Keychain {
    pub fn new(count: NodeCount, index: NodeIndex) -> Self {
        Keychain { count, index }
    }
}

impl Index for Keychain {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl KeyBoxT for Keychain {
    type Signature = Signature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        Signature::new(msg.to_vec(), self.index)
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        index == sgn.index() && msg == sgn.msg()
    }
}
