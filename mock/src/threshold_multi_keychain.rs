use aleph_bft_types::{
    Index, KeyBox, MultiKeychain, NodeCount, NodeIndex, PartialMultisignature, SignatureSet,
};
use async_trait::async_trait;
use std::fmt::Debug;

/// Keybox wrapper which implements MultiKeychain such that a partial multisignature is a list of
/// signatures and a partial multisignature is considered complete if it contains more than 2N/3 signatures.
///
/// Note: this way of multisigning is very inefficient, and should be used only for testing.
#[derive(Debug, Clone)]
pub struct ThresholdMultiKeychain<KB: KeyBox> {
    key_box: KB,
}

impl<KB: KeyBox> ThresholdMultiKeychain<KB> {
    // Create a new `ThresholdMultiKeychain` using the provided `KeyBox`.
    pub fn new(key_box: KB) -> Self {
        ThresholdMultiKeychain { key_box }
    }

    fn quorum(&self) -> usize {
        2 * self.node_count().0 / 3 + 1
    }
}

impl<KB: KeyBox> Index for ThresholdMultiKeychain<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait]
impl<KB: KeyBox> KeyBox for ThresholdMultiKeychain<KB> {
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

impl<KB: KeyBox> MultiKeychain for ThresholdMultiKeychain<KB> {
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
