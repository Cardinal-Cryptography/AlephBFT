use crate::crypto::{PartialMultisignature, Signature};
use aleph_bft_types::{
    Index, KeyBox as KeyBoxT, MultiKeychain as MultiKeychainT, NodeCount, NodeIndex,
};
use async_trait::async_trait;
use std::fmt::Debug;

pub trait MK:
    KeyBoxT<Signature = Signature> + MultiKeychainT<PartialMultisignature = PartialMultisignature>
{
}

/// Keybox wrapper which produces incorrect signatures
#[derive(Debug, Clone)]
pub struct BadSignatureWrapper<KB: MK> {
    key_box: KB,
}

impl<KB: MK> From<KB> for BadSignatureWrapper<KB> {
    fn from(key_box: KB) -> Self {
        Self { key_box }
    }
}

impl<KB: MK> Index for BadSignatureWrapper<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait]
impl<KB: MK> KeyBoxT for BadSignatureWrapper<KB> {
    type Signature = KB::Signature;

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        let signature = self.key_box.sign(msg).await;
        let mut msg = b"BAD".to_vec();
        msg.extend(signature.msg().clone());
        Signature::new(msg, signature.index())
    }

    fn node_count(&self) -> NodeCount {
        self.key_box.node_count()
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        self.key_box.verify(msg, sgn, index)
    }
}

impl<KB: MK> MultiKeychainT for BadSignatureWrapper<KB> {
    type PartialMultisignature = KB::PartialMultisignature;

    fn from_signature(
        &self,
        signature: &Self::Signature,
        index: NodeIndex,
    ) -> Self::PartialMultisignature {
        self.key_box.from_signature(signature, index)
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        self.key_box.is_complete(msg, partial)
    }
}
