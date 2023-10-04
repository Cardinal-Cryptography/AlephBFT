//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
use crate::{
    handler::Handler, scheduler::TaskScheduler, RmcHash, RmcIncomingMessage, RmcOutgoingMessage,
};
pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use aleph_bft_types::Terminator;
use core::fmt::Debug;
use futures::{
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
    FutureExt, StreamExt,
};
use log::{debug, error, warn};
use std::hash::Hash;

/// Reliable Multicast Box
///
/// The instance of [`Service<H, MK, SCH>`] reliably broadcasts hashes of type `H`,
/// and when a hash is successfully broadcast, the multisigned hash `Multisigned<H, MK>`
/// is sent to the network.
///
/// A node with an instance of [`Service<H, MK, SCH>`] can initiate broadcasting a message `msg: H`
/// by sending the [`RmcIncomingMessage::StartRmc`] variant through the network to [`Service<H, MK, SCH>`].
/// As a result, the node signs `msg` and starts broadcasting the signed message via the network.
/// When sufficintly many nodes initiate rmc with the same message `msg` and a node collects enough
/// signatures to form a complete multisignature under the message, the multisigned message
/// is sent back through the network by the instance of [`Service<H, MK, SCH>`].
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/reliable_broadcast.html
/// for a high-level description of this protocol and how it is used for fork alerts.
pub struct Service<H, MK, SCH>
where
    H: Signable + Hash,
    MK: MultiKeychain,
    SCH: TaskScheduler<RmcHash<H, MK::Signature, MK::PartialMultisignature>>,
{
    incoming_messages:
        UnboundedReceiver<RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>>,
    outgoing_messages: UnboundedSender<RmcOutgoingMessage<H, MK>>,
    scheduler: SCH,
    handler: Handler<H, MK>,
}

impl<H, MK, SCH> Service<H, MK, SCH>
where
    H: Signable + Hash + Eq + Clone + Debug,
    MK: MultiKeychain,
    SCH: TaskScheduler<RmcHash<H, MK::Signature, MK::PartialMultisignature>>,
{
    pub fn new(
        incoming_messages: UnboundedReceiver<
            RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
        >,
        outgoing_messages: UnboundedSender<RmcOutgoingMessage<H, MK>>,
        scheduler: SCH,
        handler: Handler<H, MK>,
    ) -> Self {
        Service {
            incoming_messages,
            outgoing_messages,
            scheduler,
            handler,
        }
    }

    fn handle_message(
        &mut self,
        message: RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
    ) {
        match message {
            RmcIncomingMessage::StartRmc(hash) => {
                debug!(target: "AlephBFT-rmc", "starting rmc for {:?}", hash);
                let unchecked = self.handler.on_start_rmc(hash);
                self.scheduler.add_task(RmcHash::SignedHash(unchecked));
            }
            RmcIncomingMessage::RmcHash(RmcHash::MultisignedHash(unchecked)) => {
                match self.handler.on_multisigned_hash(&unchecked) {
                    Ok(Some(multisigned)) => {
                        self.scheduler.add_task(RmcHash::MultisignedHash(unchecked));
                        self.send_message(RmcOutgoingMessage::NewMultisigned(multisigned));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(target: "AlephBFT-rmc", "{}", error);
                    }
                }
            }
            RmcIncomingMessage::RmcHash(RmcHash::SignedHash(unchecked)) => {
                match self.handler.on_signed_hash(&unchecked) {
                    Ok(Some(multisigned)) => {
                        self.scheduler.add_task(RmcHash::MultisignedHash(
                            multisigned.clone().into_unchecked(),
                        ));
                        self.send_message(RmcOutgoingMessage::NewMultisigned(multisigned));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(target: "AlephBFT-rmc", "{}", error);
                    }
                }
            }
        }
    }

    fn send_message(&self, message: RmcOutgoingMessage<H, MK>) {
        if self.outgoing_messages.unbounded_send(message).is_err() {
            error!(target: "AlephBFT-rmc", "outgoing messages channel closed early.");
        }
    }

    /// Run the rmc service.
    pub async fn run(mut self, mut terminator: Terminator) {
        loop {
            futures::select! {
                message = self.incoming_messages.next() => {
                    match message {
                        Some(message) => self.handle_message(message),
                        None => debug!(target: "AlephBFT-rmc", "Network connection closed"),
                    }
                }
                task = self.scheduler.next_task().fuse() => {
                    match task {
                        Some(task) => self.send_message(RmcOutgoingMessage::RmcHash(task)),
                        None => debug!(target: "AlephBFT-rmc", "Tasks ended"),
                    }
                }
                _ = terminator.get_exit().fuse() => {
                    debug!(target: "AlephBFT-rmc", "received exit signal.");
                    break;
                },
            }
        }
        terminator.terminate_sync().await;
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        DoublingDelayScheduler, Handler, RmcHash, RmcIncomingMessage, RmcOutgoingMessage, Service,
    };
    use aleph_bft_crypto::{Multisigned, NodeCount, NodeIndex, Signed};
    use aleph_bft_mock::{BadSigning, Keychain, PartialMultisignature, Signable, Signature};
    use aleph_bft_types::Terminator;
    use futures::{
        channel::{
            mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
            oneshot,
        },
        future::{self},
        StreamExt,
    };
    use rand::Rng;
    use std::{collections::HashMap, time::Duration};

    type TestIncomingMessage = RmcIncomingMessage<Signable, Signature, PartialMultisignature>;
    type TestOutgoingMessage = RmcOutgoingMessage<Signable, Keychain>;
    type TestScheduler =
        DoublingDelayScheduler<RmcHash<Signable, Signature, PartialMultisignature>>;

    struct TestNetwork {
        outgoing_rxs: Vec<UnboundedReceiver<TestOutgoingMessage>>,
        incoming_txs: Vec<UnboundedSender<TestIncomingMessage>>,
        message_filter: Box<dyn FnMut(NodeIndex, TestIncomingMessage) -> bool>,
        multisigned_txs: Vec<UnboundedSender<Multisigned<Signable, Keychain>>>,
    }

    type NewTestNetworkResult = (
        TestNetwork,
        Vec<UnboundedReceiver<TestIncomingMessage>>,
        Vec<UnboundedSender<TestOutgoingMessage>>,
        Vec<UnboundedReceiver<Multisigned<Signable, Keychain>>>,
    );

    impl TestNetwork {
        fn new(
            node_count: NodeCount,
            message_filter: impl FnMut(NodeIndex, TestIncomingMessage) -> bool + 'static,
        ) -> NewTestNetworkResult {
            let (incoming_txs, incoming_rxs): (Vec<_>, Vec<_>) = (0..node_count.0)
                .map(|_| unbounded::<TestIncomingMessage>())
                .unzip();
            let (outgoing_txs, outgoing_rxs): (Vec<_>, Vec<_>) = (0..node_count.0)
                .map(|_| unbounded::<TestOutgoingMessage>())
                .unzip();
            let (multisigned_txs, multisigned_rxs) = (0..node_count.0).map(|_| unbounded()).unzip();
            (
                TestNetwork {
                    outgoing_rxs,
                    incoming_txs,
                    message_filter: Box::new(message_filter),
                    multisigned_txs,
                },
                incoming_rxs,
                outgoing_txs,
                multisigned_rxs,
            )
        }

        fn send_message(&mut self, node: NodeIndex, msg: TestIncomingMessage) {
            self.incoming_txs[node.0]
                .unbounded_send(msg)
                .expect("channel should be open");
        }

        fn broadcast_message(&mut self, msg: TestIncomingMessage) {
            for i in 0..self.incoming_txs.len() {
                self.send_message(NodeIndex(i), msg.clone());
            }
        }

        async fn run(&mut self) {
            while let (Some(message), i, _) =
                future::select_all(self.outgoing_rxs.iter_mut().map(|rx| rx.next())).await
            {
                match message {
                    RmcOutgoingMessage::NewMultisigned(multisigned) => {
                        self.multisigned_txs[i]
                            .unbounded_send(multisigned)
                            .expect("channel should be open");
                    }
                    RmcOutgoingMessage::RmcHash(hash) => {
                        for (i, tx) in self.incoming_txs.iter().enumerate() {
                            if (self.message_filter)(
                                NodeIndex(i),
                                TestIncomingMessage::RmcHash(hash.clone()),
                            ) {
                                tx.unbounded_send(TestIncomingMessage::RmcHash(hash.clone()))
                                    .expect("channel should be open");
                            }
                        }
                    }
                }
            }
        }
    }

    struct TestEnvironment {
        network: TestNetwork,
        rmc_services: Vec<Service<Signable, Keychain, TestScheduler>>,
        multisigned_rxs: Vec<UnboundedReceiver<Multisigned<Signable, Keychain>>>,
    }

    impl TestEnvironment {
        fn new(
            node_count: NodeCount,
            message_filter: impl FnMut(NodeIndex, TestIncomingMessage) -> bool + 'static,
        ) -> Self {
            let (network, rmc_inputs, rmc_outputs, multisigned_rxs) =
                TestNetwork::new(node_count, message_filter);
            let node_count = NodeCount(rmc_inputs.len());
            let mut rmc_services = vec![];
            let mut rmc_inputs = rmc_inputs.into_iter();
            let mut rmc_outputs = rmc_outputs.into_iter();
            for i in 0..node_count.0 {
                rmc_services.push(Service::new(
                    rmc_inputs.next().expect("there should be enough rxs"),
                    rmc_outputs.next().expect("there should be enough txs"),
                    DoublingDelayScheduler::new(Duration::from_millis(1)),
                    Handler::new(Keychain::new(node_count, NodeIndex(i))),
                ));
            }
            TestEnvironment {
                network,
                rmc_services,
                multisigned_rxs,
            }
        }

        fn start_rmc(&mut self, node: NodeIndex, hash: Signable) {
            self.network
                .send_message(node, RmcIncomingMessage::StartRmc(hash));
        }

        async fn collect_multisigned_hashes(
            mut self,
            count: usize,
        ) -> HashMap<NodeIndex, Vec<Multisigned<Signable, Keychain>>> {
            let node_count = NodeCount(self.rmc_services.len());
            let (exit_tx, exit_rx) = oneshot::channel();
            let mut terminator = Terminator::create_root(exit_rx, "root");

            let mut hashes = HashMap::new();
            let mut services = self.rmc_services.into_iter();
            let mut handles = vec![];

            for _ in 0..node_count.0 {
                let rmc_terminator = terminator.add_offspring_connection("rmc");
                let service = services.next().expect("there should be enough services");
                handles.push(tokio::spawn(async move {
                    service.run(rmc_terminator).await;
                }));
            }

            for _ in 0..count {
                tokio::select! {
                    (unchecked, i, _) = future::select_all(self.multisigned_rxs.iter_mut().map(|rx| rx.next())) => {
                        if let Some(unchecked) = unchecked {
                            hashes.entry(i.into()).or_insert_with(Vec::new).push(unchecked);
                        }
                    }
                    _ = self.network.run() => {
                        panic!("network ended unexpectedly");
                    }
                }
            }

            let _ = exit_tx.send(());
            terminator.terminate_sync().await;
            hashes
        }
    }

    /// Create 10 honest nodes and let each of them start rmc for the same hash.
    #[tokio::test]
    async fn simple_scenario() {
        let node_count = NodeCount(10);
        let mut environment = TestEnvironment::new(node_count, |_, _| true);
        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            environment.start_rmc(NodeIndex(i), hash.clone());
        }

        let hashes = environment.collect_multisigned_hashes(node_count.0).await;
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
        let _keychains = Keychain::new_vec(node_count);
        let mut rng = rand::thread_rng();
        let mut environment =
            TestEnvironment::new(node_count, move |_, _| rng.gen_range(0..5) == 0);

        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            environment.start_rmc(NodeIndex(i), hash.clone());
        }

        let hashes = environment.collect_multisigned_hashes(node_count.0).await;
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
        let mut environment = TestEnvironment::new(node_count, move |node_ix, message| {
            !matches!(
                (node_ix.0, message),
                (0, TestIncomingMessage::RmcHash(RmcHash::SignedHash(_)))
            )
        });

        let threshold = (2 * node_count.0 + 1) / 3;
        let hash: Signable = "56".into();
        for i in 0..threshold {
            environment.start_rmc(NodeIndex(i), hash.clone());
        }

        let hashes = environment.collect_multisigned_hashes(node_count.0).await;
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
        let _keychains = Keychain::new_vec(node_count);
        let mut environment = TestEnvironment::new(node_count, |_, _| true);

        let bad_hash: Signable = "65".into();
        let bad_keychain: BadSigning<Keychain> = Keychain::new(node_count, 0.into()).into();
        let bad_msg = TestIncomingMessage::RmcHash(RmcHash::SignedHash(
            Signed::sign_with_index(bad_hash.clone(), &bad_keychain).into(),
        ));
        environment.network.broadcast_message(bad_msg);
        let bad_msg = TestIncomingMessage::RmcHash(RmcHash::MultisignedHash(
            Signed::sign_with_index(bad_hash.clone(), &bad_keychain)
                .into_partially_multisigned(&bad_keychain)
                .into_unchecked(),
        ));
        environment.network.broadcast_message(bad_msg);

        let hash: Signable = "56".into();
        for i in 0..node_count.0 {
            environment.start_rmc(NodeIndex(i), hash.clone());
        }

        let hashes = environment.collect_multisigned_hashes(node_count.0).await;
        assert_eq!(hashes.len(), node_count.0);
        for i in 0..node_count.0 {
            let multisignatures = &hashes[&i.into()];
            assert_eq!(multisignatures.len(), 1);
            assert_eq!(multisignatures[0].as_signable(), &hash);
        }
    }
}
