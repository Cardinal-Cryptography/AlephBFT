use crate::crypto::{PartialMultisignature, Signature};
use aleph_bft_types::{
    Index, KeyBox as KeychainT, MultiKeychain as MultiKeychainT, NodeCount, NodeIndex,
};
use async_trait::async_trait;
use std::fmt::Debug;

pub trait MK:
    KeychainT<Signature = Signature> + MultiKeychainT<PartialMultisignature = PartialMultisignature>
{
}

/// Keychain wrapper which produces incorrect signatures
#[derive(Debug, Clone)]
pub struct BadSignatureWrapper<T: MK>(T);

impl<T: MK> From<T> for BadSignatureWrapper<T> {
    fn from(mk: T) -> Self {
        Self(mk)
    }
}

impl<T: MK> Index for BadSignatureWrapper<T> {
    fn index(&self) -> NodeIndex {
        self.0.index()
    }
}

#[async_trait]
impl<T: MK> KeychainT for BadSignatureWrapper<T> {
    type Signature = T::Signature;

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        let signature = self.0.sign(msg).await;
        let mut msg = b"BAD".to_vec();
        msg.extend(signature.msg().clone());
        Signature::new(msg, signature.index())
    }

    fn node_count(&self) -> NodeCount {
        self.0.node_count()
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        self.0.verify(msg, sgn, index)
    }
}

impl<T: MK> MultiKeychainT for BadSignatureWrapper<T> {
    type PartialMultisignature = T::PartialMultisignature;

    fn from_signature(
        &self,
        signature: &Self::Signature,
        index: NodeIndex,
    ) -> Self::PartialMultisignature {
        self.0.from_signature(signature, index)
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        self.0.is_complete(msg, partial)
    }
}
