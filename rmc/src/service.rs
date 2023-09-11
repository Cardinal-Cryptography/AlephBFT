pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use core::fmt::Debug;
use futures::{channel::mpsc::{UnboundedReceiver, UnboundedSender}, FutureExt, StreamExt};
use log::{debug, error, warn};
use std::{
    hash::Hash,
};
use crate::{Message, Task, TaskScheduler};
use crate::handler::Handler;


/// Reliable Multicast Service
///
/// The instance of [`Service<H, MK>`] reliably broadcasts hashes of type `H`,
/// and when a hash is successfully broadcasted, it passes multisigned hash `Multisigned<H, MK>`
/// through `multisigned_hashes_tx`
///
/// A node with an instance of [`Service<H, MK>`] can initiate broadcasting
/// a message `msg: H` by passing it to `hashes_for_rmc` receiver. As a result,
/// the node signs `msg` and starts broadcasting the signed message via the network.
/// When sufficintly many nodes initiate broadcasts with the same message `msg`
/// and a node collects enough signatures to form a complete multisignature under the message,
/// the multisigned message is yielded by the instance of [`Service`].
/// The multisigned messages are passed through `multisigned_hashes_tx`.
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/reliable_broadcast.html
/// for a high-level description of this protocol and how it is used for fork alerts.
pub struct Service<H: Signable + Hash, MK: MultiKeychain> {
    keychain: MK,
    network_tx: UnboundedSender<Message<H, MK::Signature, MK::PartialMultisignature>>,
    network_rx: UnboundedReceiver<Message<H, MK::Signature, MK::PartialMultisignature>>,
    multisigned_hashes_tx: UnboundedSender<Multisigned<H, MK>>,
    hashes_for_rmc: UnboundedReceiver<H>,
    scheduler: Box<dyn TaskScheduler<Task<H, MK>>>,
}

impl<H: Signable + Hash + Eq + Clone + Debug, MK: MultiKeychain> Service<H, MK> {
    pub fn new(
        keychain: MK,
        network_tx: UnboundedSender<Message<H, MK::Signature, MK::PartialMultisignature>>,
        network_rx: UnboundedReceiver<Message<H, MK::Signature, MK::PartialMultisignature>>,
        multisigned_hashes_tx: UnboundedSender<Multisigned<H, MK>>,
        hashes_for_rmc: UnboundedReceiver<H>,
        scheduler: Box<dyn TaskScheduler<Task<H, MK>>>,
    ) -> Service<H, MK> {
        Self {
            keychain,
            network_tx,
            network_rx,
            multisigned_hashes_tx,
            hashes_for_rmc,
            scheduler,
        }
    }

    fn do_task(&self, task: Task<H, MK>) {
        let Task::BroadcastMessage(message) = task;
        self.network_tx
            .unbounded_send(message)
            .expect("Sending message should succeed");
    }

    fn announce_multisigned(&mut self, multisigned: Multisigned<H, MK>, task: Task<H, MK>) {
        if self.multisigned_hashes_tx.unbounded_send(multisigned).is_err() {
            error!(target: "AlephBFT-rmc", "multisigned hashes receiver closed.");
            return;
        }
        self.scheduler.add_task(task);
    }

    fn handle_signed_hash(&mut self, unchecked: UncheckedSigned<Indexed<H>, MK::Signature>, hash: H, handler: &mut Handler<H, MK>) {
        let signed_hash = match unchecked.check(&self.keychain) {
            Ok(signed_hash) => signed_hash,
            Err(_) => {
                warn!(target: "AlephBFT-rmc", "Received a hash with a bad signature");
                return;
            }
        };
        if let Some((multisigned, task)) = handler.on_signature(signed_hash, hash) {
            self.announce_multisigned(multisigned, task);
        }
    }

    fn handle_message(&mut self, message: Message<H, MK::Signature, MK::PartialMultisignature>, mut handler: &mut Handler<H, MK>) {
        if handler.is_rmc_complete(&message) {
            return;
        }
        let hash = message.hash().clone();
        match message {
            Message::StartRmc(unchecked) => {

            }
            Message::SignedHash(unchecked) => {
                self.handle_signed_hash(unchecked, hash, &mut handler);
            }
            Message::MultisignedHash(unchecked) => match unchecked.check_multi(&self.keychain) {
                Ok(multisigned) => {
                    let task = handler.on_complete_multisignature(&multisigned);
                    self.announce_multisigned(multisigned, task);
                }
                Err(_) => {
                    warn!(target: "AlephBFT-rmc", "Received a hash with a bad multisignature");
                }
            },
        }
    }

    /// Initiate a new instance of RMC for `hash`.
    pub fn handle_start_rmc(&mut self, hash: H, handler: &mut Handler<H, MK>) {
        debug!(target: "AlephBFT-rmc", "starting rmc for {:?}", hash);
        let signed_hash = Signed::sign_with_index(hash, &self.keychain);
        let message = Message::SignedHash(signed_hash.into_unchecked());
        self.handle_message(message.clone(), handler);
        if !handler.is_rmc_complete(&message) {
            let task = Task::BroadcastMessage(message);
            self.scheduler.add_task(task);
        }
    }

    pub async fn run(&mut self, mut handler: Handler<H, MK>) -> Multisigned<H, MK> {
        loop {
            futures::select! {
                hash = self.hashes_for_rmc.next() => {
                    if let Some(hash) = hash {
                        self.handle_start_rmc(hash, &mut handler);
                    } else {
                        debug!(target: "AlephBFT-rmc", "rmc hashes stream closed.");
                    }
                }
                incoming_message = self.network_rx.next() => {
                    if let Some(incoming_message) = incoming_message {
                        self.handle_message(incoming_message, &mut handler);
                    } else {
                        debug!(target: "AlephBFT-rmc", "Network connection closed");
                    }
                }
                task = self.scheduler.next_task().fuse() => {
                    if let Some(task) = task {
                        self.do_task(task);
                    } else {
                        debug!(target: "AlephBFT-rmc", "Tasks ended");
                    }
                }
            }
        }
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
    use crate::service::Service;

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
        services: Vec<Service<Signable, Keychain>>,
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
            let mut services = Vec::new();
            for (i, (rx, tx)) in channels.into_iter().enumerate() {
                let (handler, _) = Handler::new(
                    keychains[i],
                    vec![],
                    vec![],
                );
                handlers.push(handler);
                let service = Service::new(
                    keychains[i], tx, rx,
                )
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

