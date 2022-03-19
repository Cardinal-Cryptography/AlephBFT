use crate::{
    rmc::*, signed::*, Index, KeyBox, MultiKeychain, NodeCount, NodeIndex, PartialMultisignature,
    Signable, SignatureSet,
};
use async_trait::async_trait;
use codec::{Decode, Encode};
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    future::{self, BoxFuture},
    stream::{self, Stream},
    FutureExt, StreamExt,
};
use rand::Rng;
use std::{collections::HashMap, fmt::Debug, pin::Pin, time::Duration};

/// Keybox wrapper which implements MultiKeychain such that a partial multisignature is a list of
/// signatures and a partial multisignature is considered complete if it contains more than 2N/3 signatures.
///
/// Note: this way of multisigning is very inefficient, and should be used only for testing.
#[derive(Debug, Clone)]
struct DefaultMultiKeychain<KB: KeyBox> {
    key_box: KB,
}

impl<KB: KeyBox> DefaultMultiKeychain<KB> {
    // Create a new `DefaultMultiKeychain` using the provided `KeyBox`.
    fn new(key_box: KB) -> Self {
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
struct TestSignature {
    msg: Vec<u8>,
    index: NodeIndex,
}

#[derive(Clone, Debug)]
struct TestKeyBox {
    count: NodeCount,
    index: NodeIndex,
}

impl TestKeyBox {
    fn new(count: NodeCount, index: NodeIndex) -> Self {
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

type TestMultiKeychain = DefaultMultiKeychain<TestKeyBox>;

type TestPartialMultisignature = SignatureSet<TestSignature>;

fn test_multi_keychain(node_count: NodeCount, index: NodeIndex) -> TestMultiKeychain {
    let key_box = TestKeyBox::new(node_count, index);
    DefaultMultiKeychain::new(key_box)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, Ord, PartialOrd)]
struct Hash {
    byte: u8,
}

impl Signable for Hash {
    type Hash = [u8; 1];

    fn hash(&self) -> Self::Hash {
        [self.byte]
    }
}

type TestMessage = Message<Hash, TestSignature, TestPartialMultisignature>;

struct TestNetwork {
    outgoing_rx: Pin<Box<dyn Stream<Item = TestMessage>>>,
    incoming_txs: Vec<UnboundedSender<TestMessage>>,
    message_filter: Box<dyn FnMut(NodeIndex, TestMessage) -> bool>,
}

impl TestNetwork {
    fn new(
        node_count: NodeCount,
        message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
    ) -> (
        Self,
        Vec<(UnboundedReceiver<TestMessage>, UnboundedSender<TestMessage>)>,
    ) {
        let all_nodes: Vec<_> = (0..node_count.0).map(NodeIndex).collect();
        let (incomng_txs, incoming_rxs): (Vec<_>, Vec<_>) =
            all_nodes.iter().map(|_| unbounded::<TestMessage>()).unzip();
        let (outgoing_txs, outgoing_rxs): (Vec<_>, Vec<_>) = {
            all_nodes
                .iter()
                .map(|_| {
                    let (tx, rx) = unbounded::<TestMessage>();
                    (tx, rx)
                })
                .unzip()
        };
        let network = TestNetwork {
            outgoing_rx: Box::pin(stream::select_all(outgoing_rxs)),
            incoming_txs: incomng_txs,
            message_filter: Box::new(message_filter),
        };

        let channels = incoming_rxs.into_iter().zip(outgoing_txs).collect();
        (network, channels)
    }

    fn broadcast_message(&mut self, msg: TestMessage) {
        for tx in &mut self.incoming_txs {
            tx.unbounded_send(msg.clone())
                .expect("Channel should be open");
        }
    }
}

impl TestNetwork {
    async fn run(&mut self) {
        while let Some(message) = self.outgoing_rx.next().await {
            for (i, tx) in self.incoming_txs.iter().enumerate() {
                if (self.message_filter)(NodeIndex(i), message.clone()) {
                    tx.unbounded_send(message.clone())
                        .expect("Channel should be open");
                }
            }
        }
    }
}

fn prepare_keychains(node_count: NodeCount) -> Vec<TestMultiKeychain> {
    (0..node_count.0)
        .map(|i| test_multi_keychain(node_count, i.into()))
        .collect()
}

struct TestData<'a> {
    network: TestNetwork,
    rmcs: Vec<ReliableMulticast<'a, Hash, TestMultiKeychain>>,
}

impl<'a> TestData<'a> {
    fn new(
        node_count: NodeCount,
        keychains: &'a [TestMultiKeychain],
        message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
    ) -> Self {
        let (network, channels) = TestNetwork::new(node_count, message_filter);
        let mut rmcs = Vec::new();
        for (i, (rx, tx)) in channels.into_iter().enumerate() {
            let rmc = ReliableMulticast::new(
                rx,
                tx,
                &keychains[i],
                node_count,
                DoublingDelayScheduler::new(Duration::from_millis(1)),
            );
            rmcs.push(rmc);
        }
        TestData { network, rmcs }
    }

    async fn collect_multisigned_hashes(
        mut self,
        count: usize,
    ) -> HashMap<NodeIndex, Vec<Multisigned<'a, Hash, TestMultiKeychain>>> {
        let mut hashes = HashMap::new();

        for _ in 0..count {
            // covert each RMC into a future returning an optional unchecked multisigned hash.
            let rmc_futures: Vec<BoxFuture<Multisigned<'a, Hash, TestMultiKeychain>>> = self
                .rmcs
                .iter_mut()
                .map(|rmc| rmc.next_multisigned_hash().boxed())
                .collect();
            tokio::select! {
                (unchecked, i, _) = future::select_all(rmc_futures) => {
                    hashes.entry(i.into()).or_insert_with(Vec::new).push(unchecked);
                }
                _ = self.network.run() => {
                    panic!("network ended unexpectedly");
                }
            }
        }
        hashes
    }
}

/// Create 10 honest nodes and let each of them start rmc for the same hash.
#[tokio::test]
async fn simple_scenario() {
    let node_count = NodeCount(10);
    let keychains = prepare_keychains(node_count);
    let mut data = TestData::new(node_count, &keychains, |_, _| true);

    let hash = Hash { byte: 56 };
    for i in 0..node_count.0 {
        data.rmcs[i].start_rmc(hash).await;
    }

    let hashes = data.collect_multisigned_hashes(node_count.0).await;
    assert_eq!(hashes.len(), node_count.0);
    for i in 0..node_count.0 {
        let multisignatures = &hashes[&i.into()];
        assert_eq!(multisignatures.len(), 1);
        assert_eq!(multisignatures[0].as_signable(), &hash);
    }
}

/// Each message is delivered with 20% probability
#[tokio::test]
async fn faulty_network() {
    let node_count = NodeCount(10);
    let keychains = prepare_keychains(node_count);
    let mut rng = rand::thread_rng();
    let mut data = TestData::new(node_count, &keychains, move |_, _| rng.gen_range(0..5) == 0);

    let hash = Hash { byte: 56 };
    for i in 0..node_count.0 {
        data.rmcs[i].start_rmc(hash).await;
    }

    let hashes = data.collect_multisigned_hashes(node_count.0).await;
    assert_eq!(hashes.len(), node_count.0);
    for i in 0..node_count.0 {
        let multisignatures = &hashes[&i.into()];
        assert_eq!(multisignatures.len(), 1);
        assert_eq!(multisignatures[0].as_signable(), &hash);
    }
}

/// Only 7 nodes start rmc and one of the nodes which didn't start rmc
/// is delivered only messages with complete multisignatures
#[tokio::test]
async fn node_hearing_only_multisignatures() {
    let node_count = NodeCount(10);
    let keychains = prepare_keychains(node_count);
    let mut data = TestData::new(node_count, &keychains, move |node_ix, message| {
        !matches!((node_ix.0, message), (0, Message::SignedHash(_)))
    });

    let threshold = (2 * node_count.0 + 1) / 3;
    let hash = Hash { byte: 56 };
    for i in 0..threshold {
        data.rmcs[i].start_rmc(hash).await;
    }

    let hashes = data.collect_multisigned_hashes(node_count.0).await;
    assert_eq!(hashes.len(), node_count.0);
    for i in 0..node_count.0 {
        let multisignatures = &hashes[&i.into()];
        assert_eq!(multisignatures.len(), 1);
        assert_eq!(multisignatures[0].as_signable(), &hash);
    }
}

fn bad_signature() -> TestSignature {
    TestSignature {
        msg: Vec::new(),
        index: 111.into(),
    }
}

fn bad_multisignature(node_count: NodeCount) -> TestPartialMultisignature {
    SignatureSet::with_size(node_count)
}

/// 7 honest nodes and 3 dishonest nodes which emit bad signatures and multisignatures
#[tokio::test]
async fn bad_signatures_and_multisignatures_are_ignored() {
    let node_count = NodeCount(10);
    let keychains = prepare_keychains(node_count);
    let mut data = TestData::new(node_count, &keychains, |_, _| true);

    let bad_hash = Hash { byte: 65 };
    let bad_msg = TestMessage::SignedHash(UncheckedSigned::new_with_index(
        bad_hash,
        0.into(),
        bad_signature(),
    ));
    data.network.broadcast_message(bad_msg);
    let bad_msg = TestMessage::MultisignedHash(UncheckedSigned::new(
        bad_hash,
        bad_multisignature(node_count),
    ));
    data.network.broadcast_message(bad_msg);

    let hash = Hash { byte: 56 };
    for i in 0..node_count.0 {
        data.rmcs[i].start_rmc(hash).await;
    }

    let hashes = data.collect_multisigned_hashes(node_count.0).await;
    assert_eq!(hashes.len(), node_count.0);
    for i in 0..node_count.0 {
        let multisignatures = &hashes[&i.into()];
        assert_eq!(multisignatures.len(), 1);
        assert_eq!(multisignatures[0].as_signable(), &hash);
    }
}
