use aleph_bft_crypto::{
    Index, KeyBox, MultiKeychain, NodeCount, NodeIndex, PartialMultisignature, SignatureSet,
};
use async_trait::async_trait;
use codec::{Decode, Encode};
use std::fmt::Debug;

pub type TestMultiKeychain = DefaultMultiKeychain<TestKeyBox>;

pub fn prepare_keychains(node_count: NodeCount) -> Vec<TestMultiKeychain> {
    (0..node_count.0)
        .map(|i| DefaultMultiKeychain::new(TestKeyBox::new(node_count, i.into())))
        .collect()
}

/// Keybox wrapper which implements MultiKeychain such that a partial multisignature is a list of
/// signatures and a partial multisignature is considered complete if it contains more than 2N/3 signatures.
///
/// Note: this way of multisigning is very inefficient, and should be used only for testing.
#[derive(Debug, Clone)]
pub struct DefaultMultiKeychain<KB: KeyBox> {
    key_box: KB,
}

impl<KB: KeyBox> DefaultMultiKeychain<KB> {
    // Create a new `DefaultMultiKeychain` using the provided `KeyBox`.
    pub fn new(key_box: KB) -> Self {
        DefaultMultiKeychain { key_box }
    }

    fn quorum(&self) -> usize {
        2 * self.node_count().0 / 3 + 1
    }
}

impl<KB: KeyBox> Index for DefaultMultiKeychain<KB> {
    fn index(&self) -> NodeIndex {
        self.key_box.index()
    }
}

#[async_trait::async_trait]
impl<KB: KeyBox> KeyBox for DefaultMultiKeychain<KB> {
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

impl<KB: KeyBox> MultiKeychain for DefaultMultiKeychain<KB> {
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

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct TestSignature {
    msg: Vec<u8>,
    index: NodeIndex,
}

#[derive(Clone, Debug)]
pub struct TestKeyBox {
    count: NodeCount,
    index: NodeIndex,
}

impl TestKeyBox {
    pub fn new(count: NodeCount, index: NodeIndex) -> Self {
        TestKeyBox { count, index }
    }
}

impl Index for TestKeyBox {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait]
impl KeyBox for TestKeyBox {
    type Signature = TestSignature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        TestSignature {
            msg: msg.to_vec(),
            index: self.index,
        }
    }

    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool {
        index == sgn.index && msg == sgn.msg
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
    type Signature = TestSignature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, msg: &[u8]) -> Self::Signature {
        TestSignature {
            msg: msg.to_vec(),
            index: self.signature_index,
        }
    }

    fn verify(&self, _msg: &[u8], _sgn: &Self::Signature, _index: NodeIndex) -> bool {
        true
    }
}

impl MultiKeychain for BadKeyBox {
    type PartialMultisignature = SignatureSet<TestSignature>;

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
