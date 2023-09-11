pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use core::fmt::Debug;
use std::{
    collections::{HashMap},
    hash::Hash,
};
use crate::{Message, Task};

pub struct Handler<H: Signable + Hash, MK: MultiKeychain> {
    keychain: MK,
    hash_states: HashMap<H, PartiallyMultisigned<H, MK>>,
}

impl<H: Signable + Hash + Eq + Clone + Debug, MK: MultiKeychain> Handler<H, MK> {

    pub fn new(
        keychain: MK,
        initial_rmc_hashes: Vec<H>,
        mut initial_multisigned: Vec<Multisigned<H, MK>>,
    ) -> (Self, Vec<Task<H, MK>>) {
        let mut handler = Handler {
            keychain,
            hash_states: HashMap::new()
        };
        let mut tasks = vec![];

        for hash in initial_rmc_hashes {
            let signed_hash = Signed::sign_with_index(hash, &handler.keychain);
            let message = Message::SignedHash(signed_hash.clone().into_unchecked());
            if !handler.is_rmc_complete(&message) {
                if let Some((multisigned, _)) = handler.on_signature(signed_hash, message.hash().clone()) {
                    initial_multisigned.push(multisigned);
                }
            }
        }

        for multisigned in initial_multisigned {
            let task = handler.on_complete_multisignature(&multisigned);
            tasks.push(task)
        }

        (handler, tasks)
    }

    /// Fetches final multisignature from storage, if exists.
    pub fn get_multisigned(&self, hash: &H) -> Option<Multisigned<H, MK>> {
        match self.hash_states.get(hash)? {
            PartiallyMultisigned::Complete { multisigned } => Some(multisigned.clone()),
            _ => None,
        }
    }

    pub fn is_rmc_complete(&self, message: &Message<H, MK::Signature, MK::PartialMultisignature>) -> bool {
        let hash = message.hash().clone();
        if let Some(PartiallyMultisigned::Complete { .. }) = self.hash_states.get(&hash) {
            return true;
        }
        false
    }

    pub fn on_signature(&mut self, signed: Signed<Indexed<H>, MK>, hash: H) -> Option<(Multisigned<H, MK>, Task<H, MK>)> {
        let new_state = match self.hash_states.remove(&hash) { // this is just hash
            None => signed.into_partially_multisigned(&self.keychain),
            Some(partial) => partial.add_signature(signed, &self.keychain),
        };
        match new_state {
            PartiallyMultisigned::Complete { multisigned } => {
                let task = self.on_complete_multisignature(&multisigned);
                Some((multisigned, task))
            },
            incomplete => {
                self.hash_states.insert(hash.clone(), incomplete);
                None
            },
        }
    }

    pub fn on_complete_multisignature(&mut self, multisigned: &Multisigned<H, MK>) -> Task<H, MK> {
        let hash = multisigned.as_signable().clone();
        self.hash_states.insert(
            hash,
            PartiallyMultisigned::Complete {
                multisigned: multisigned.clone(),
            },
        );
        Task::BroadcastMessage(Message::MultisignedHash(multisigned.clone().into_unchecked()))
    }
}

#[cfg(test)]
mod tests {
    use crate::{DoublingDelayScheduler, Message, handler::Handler, TaskScheduler};
    use aleph_bft_crypto::{Multisigned, NodeCount, NodeIndex, Signed};
    use aleph_bft_mock::{BadSigning, Keychain, PartialMultisignature, Signable, Signature};
    use futures::{
        channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
        future::{self, BoxFuture},
        stream::{self, Stream},
        FutureExt, StreamExt,
    };
    use rand::Rng;
    use std::{
        collections::HashMap,
        ops::{Add, Mul},
        pin::Pin,
        time::{Duration, Instant},
    };

    type TestMessage = Message<Signable, Signature, PartialMultisignature>;

    struct TestNetwork {
        outgoing_rx: Pin<Box<dyn Stream<Item = TestMessage>>>,
        incoming_txs: Vec<UnboundedSender<TestMessage>>,
        message_filter: Box<dyn FnMut(NodeIndex, TestMessage) -> bool>,
    }

    type ReceiversSenders = Vec<(UnboundedReceiver<TestMessage>, UnboundedSender<TestMessage>)>;
    impl TestNetwork {
        fn new(
            node_count: NodeCount,
            message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
        ) -> (Self, ReceiversSenders) {
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

    struct TestData {
        network: TestNetwork,
        handlers: Vec<Handler<Signable, Keychain>>,
    }

    impl TestData {
        fn new(
            node_count: NodeCount,
            keychains: &[Keychain],
            message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
        ) -> Self {
            let (network, channels) = TestNetwork::new(node_count, message_filter);
            let mut handlers = Vec::new();
            for (i, (rx, tx)) in channels.into_iter().enumerate() {
                let (handler, _) = Handler::new(
                    keychains[i],
                    vec![],
                    vec![],
                );
                handlers.push(handler);
            }
            TestData { network, handlers }
        }

        async fn collect_multisigned_hashes(
            mut self,
            count: usize,
        ) -> HashMap<NodeIndex, Vec<Multisigned<Signable, Keychain>>> {
            let mut hashes = HashMap::new();

            for _ in 0..count {
                // covert each RMC into a future returning an optional unchecked multisigned hash.
                let rmc_futures: Vec<BoxFuture<Multisigned<Signable, Keychain>>> = self
                    .handlers
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
        let keychains = Keychain::new_vec(node_count);
        let mut data = TestData::new(node_count, &keychains, |_, _| true);

        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            data.rmcs[i].start_rmc(hash.clone());
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
        let keychains = Keychain::new_vec(node_count);
        let mut rng = rand::thread_rng();
        let mut data = TestData::new(node_count, &keychains, move |_, _| rng.gen_range(0..5) == 0);

        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            data.rmcs[i].start_rmc(hash.clone());
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
        let keychains = Keychain::new_vec(node_count);
        let mut data = TestData::new(node_count, &keychains, move |node_ix, message| {
            !matches!((node_ix.0, message), (0, Message::SignedHash(_)))
        });

        let threshold = (2 * node_count.0 + 1) / 3;
        let hash: Signable = "56".into();
        for i in 0..threshold {
            data.rmcs[i].start_rmc(hash.clone());
        }

        let hashes = data.collect_multisigned_hashes(node_count.0).await;
        assert_eq!(hashes.len(), node_count.0);
        for i in 0..node_count.0 {
            let multisignatures = &hashes[&i.into()];
            assert_eq!(multisignatures.len(), 1);
            assert_eq!(multisignatures[0].as_signable(), &hash);
        }
    }

    /// 7 honest nodes and 3 dishonest nodes which emit bad signatures and multisignatures
    #[tokio::test]
    async fn bad_signatures_and_multisignatures_are_ignored() {
        let node_count = NodeCount(10);
        let keychains = Keychain::new_vec(node_count);
        let mut data = TestData::new(node_count, &keychains, |_, _| true);

        let bad_hash: Signable = "65".into();
        let bad_keychain: BadSigning<Keychain> = Keychain::new(node_count, 0.into()).into();
        let bad_msg = TestMessage::SignedHash(
            Signed::sign_with_index(bad_hash.clone(), &bad_keychain).into(),
        );
        data.network.broadcast_message(bad_msg);
        let bad_msg = TestMessage::MultisignedHash(
            Signed::sign_with_index(bad_hash.clone(), &bad_keychain)
                .into_partially_multisigned(&bad_keychain)
                .into_unchecked(),
        );
        data.network.broadcast_message(bad_msg);

        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            data.rmcs[i].start_rmc(hash.clone());
        }

        let hashes = data.collect_multisigned_hashes(node_count.0).await;
        assert_eq!(hashes.len(), node_count.0);
        for i in 0..node_count.0 {
            let multisignatures = &hashes[&i.into()];
            assert_eq!(multisignatures.len(), 1);
            assert_eq!(multisignatures[0].as_signable(), &hash);
        }
    }
}
