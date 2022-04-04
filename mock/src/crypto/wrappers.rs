use crate::crypto::Signature;
use aleph_bft_types::{
    Index, KeyBox as KeyBoxT, MultiKeychain, NodeCount, NodeIndex, PartialMultisignature,
    SignatureSet,
};
use async_trait::async_trait;
use std::fmt::Debug;

/// Keybox wrapper which produces incorrect signatures
#[derive(Debug, Clone)]
pub struct BadSignatureWrapper<KB: KeyBoxT<Signature = Signature>> {
    key_box: KB,
}

impl<KB: KeyBoxT<Signature = Signature>> From<KB> for BadSignatureWrapper<KB> {
    fn from(key_box: KB) -> Self {
        Self { key_box }
    }
}

impl<KB: KeyBoxT<Signature = Signature>> Index for BadSignatureWrapper<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait]
impl<KB: KeyBoxT<Signature = Signature>> KeyBoxT for BadSignatureWrapper<KB> {
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

/// Keybox wrapper which claims that every signature is correct
#[derive(Debug, Clone)]
pub struct YesManWrapper<KB: KeyBoxT<Signature = Signature>> {
    key_box: KB,
}

impl<KB: KeyBoxT<Signature = Signature>> From<KB> for YesManWrapper<KB> {
    fn from(key_box: KB) -> Self {
        Self { key_box }
    }
}

impl<KB: KeyBoxT<Signature = Signature>> Index for YesManWrapper<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait]
impl<KB: KeyBoxT<Signature = Signature>> KeyBoxT for YesManWrapper<KB> {
    type Signature = KB::Signature;

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        self.key_box.sign(msg).await
    }

    fn node_count(&self) -> NodeCount {
        self.key_box.node_count()
    }

    fn verify(&self, _msg: &[u8], _sgn: &Self::Signature, _index: NodeIndex) -> bool {
        true
    }
}

/// Keybox wrapper which implements MultiKeychain such that a partial multisignature is a list of
/// signatures and a partial multisignature is considered complete if it contains more than 2N/3 signatures.
///
/// Note: this way of multisigning is very inefficient, and should be used only for testing.
#[derive(Debug, Clone)]
pub struct ThresholdMultiWrapper<KB: KeyBoxT> {
    key_box: KB,
}

impl<KB: KeyBoxT> ThresholdMultiWrapper<KB> {
    fn quorum(&self) -> usize {
        2 * self.node_count().0 / 3 + 1
    }
}

impl<KB: KeyBoxT> From<KB> for ThresholdMultiWrapper<KB> {
    fn from(key_box: KB) -> Self {
        Self { key_box }
    }
}

impl<KB: KeyBoxT> Index for ThresholdMultiWrapper<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait]
impl<KB: KeyBoxT> KeyBoxT for ThresholdMultiWrapper<KB> {
    type Signature = KB::Signature;

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        self.key_box.sign(msg).await
    }

    fn node_count(&self) -> NodeCount {
        self.key_box.node_count()
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        self.key_box.verify(msg, sgn, index)
    }
}

impl<KB: KeyBoxT> MultiKeychain for ThresholdMultiWrapper<KB> {
    type PartialMultisignature = SignatureSet<KB::Signature>;

    fn from_signature(
        &self,
        signature: &Self::Signature,
        index: NodeIndex,
    ) -> Self::PartialMultisignature {
        SignatureSet::add_signature(SignatureSet::with_size(self.node_count()), signature, index)
    }

    fn is_complete(&self, msg: &[u8], partial: &Self::PartialMultisignature) -> bool {
        let signature_count = partial.iter().count();
        if signature_count < self.quorum() {
            return false;
        }
        partial
            .iter()
            .all(|(i, sgn)| self.key_box.verify(msg, sgn, i))
    }
}
