//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
use crate::{
    handler::Handler, scheduler::TaskScheduler, RmcHash, RmcIncomingMessage, RmcOutgoingMessage,
    Task,
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
/// The instance of [`Service<H, MK>`] reliably broadcasts hashes of type `H`,
/// and when a hash is successfully broadcast, the multisigned hash `Multisigned<H, MK>`
/// is sent to the network.
///
/// A node with an instance of [`Service<H, MK>`] can initiate broadcasting a message `msg: H`
/// by sending the [`RmcIncomingMessage::StartRmc`] variant through the network to [`Service<H, MK>`].
/// As a result, the node signs `msg` and starts broadcasting the signed message via the network.
/// When sufficintly many nodes initiate rmc with the same message `msg` and a node collects enough
/// signatures to form a complete multisignature under the message, the multisigned message
/// is sent back through the network by the instance of [`Service<H, MK>`].
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/reliable_broadcast.html
/// for a high-level description of this protocol and how it is used for fork alerts.
pub struct Service<H: Signable + Hash, MK: MultiKeychain> {
    network_rx: UnboundedReceiver<RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>>,
    network_tx: UnboundedSender<RmcOutgoingMessage<H, MK>>,
    scheduler: Box<dyn TaskScheduler<Task<H, MK>>>,
}

impl<H: Signable + Hash + Eq + Clone + Debug, MK: MultiKeychain> Service<H, MK> {
    pub fn new(
        network_rx: UnboundedReceiver<
            RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
        >,
        network_tx: UnboundedSender<RmcOutgoingMessage<H, MK>>,
        scheduler: impl TaskScheduler<Task<H, MK>> + 'static,
    ) -> Self {
        Service {
            network_rx,
            network_tx,
            scheduler: Box::new(scheduler),
        }
    }

    fn handle_message(
        &mut self,
        message: RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
        handler: &mut Handler<H, MK>,
    ) {
        match message {
            RmcIncomingMessage::StartRmc(hash) => {
                debug!(target: "AlephBFT-rmc", "starting rmc for {:?}", hash);
                let unchecked = handler.on_start_rmc(hash);
                self.scheduler
                    .add_task(Task::BroadcastMessage(RmcHash::SignedHash(unchecked)));
            }
            RmcIncomingMessage::RmcHash(RmcHash::MultisignedHash(unchecked)) => {
                match handler.on_multisigned_hash(&unchecked) {
                    Ok(Some(multisigned)) => {
                        self.scheduler
                            .add_task(Task::BroadcastMessage(RmcHash::MultisignedHash(unchecked)));
                        if self
                            .network_tx
                            .unbounded_send(RmcOutgoingMessage::NewMultisigned(multisigned))
                            .is_err()
                        {
                            error!(target: "AlephBFT-rmc", "network closed early.");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(target: "AlephBFT-rmc", "{}", error);
                    }
                }
            }
            RmcIncomingMessage::RmcHash(RmcHash::SignedHash(unchecked)) => {
                match handler.on_signed_hash(&unchecked) {
                    Ok(Some(multisigned)) => {
                        self.scheduler
                            .add_task(Task::BroadcastMessage(RmcHash::MultisignedHash(
                                multisigned.clone().into_unchecked(),
                            )));
                        if self
                            .network_tx
                            .unbounded_send(RmcOutgoingMessage::NewMultisigned(multisigned))
                            .is_err()
                        {
                            error!(target: "AlephBFT-rmc", "network closed early.");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(target: "AlephBFT-rmc", "{}", error);
                    }
                }
            }
        }
    }

    fn do_task(&self, task: Task<H, MK>) {
        let Task::BroadcastMessage(hash) = task;
        self.network_tx
            .unbounded_send(RmcOutgoingMessage::RmcHash(hash))
            .expect("Sending message should succeed");
    }

    /// Perform underlying tasks until the multisignature for the hash of this instance is collected.
    pub async fn run(mut self, mut handler: Handler<H, MK>, mut terminator: Terminator) {
        loop {
            futures::select! {
                message = self.network_rx.next() => {
                    match message {
                        Some(message) => self.handle_message(message, &mut handler),
                        None => debug!(target: "AlephBFT-rmc", "Network connection closed"),
                    }
                }
                task = self.scheduler.next_task().fuse() => {
                    match task {
                        Some(task) => self.do_task(task),
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

    struct TestNetwork {
        outgoing_rxs: Vec<UnboundedReceiver<TestOutgoingMessage>>,
        incoming_txs: Vec<UnboundedSender<TestIncomingMessage>>,
        message_filter: Box<dyn FnMut(NodeIndex, TestIncomingMessage) -> bool>,
        multisigned_txs: Vec<UnboundedSender<Multisigned<Signable, Keychain>>>,
    }

    type ReceiversSenders = Vec<(
        UnboundedReceiver<TestIncomingMessage>,
        UnboundedSender<TestOutgoingMessage>,
    )>;
    impl TestNetwork {
        fn new(
            node_count: NodeCount,
            message_filter: impl FnMut(NodeIndex, TestIncomingMessage) -> bool + 'static,
        ) -> (
            Self,
            Vec<UnboundedReceiver<TestIncomingMessage>>,
            Vec<UnboundedSender<TestOutgoingMessage>>,
            Vec<UnboundedReceiver<Multisigned<Signable, Keychain>>>,
        ) {
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
        rmc_handlers: Vec<Handler<Signable, Keychain>>,
        rmc_services: Vec<Service<Signable, Keychain>>,
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
            let mut rmc_handlers = vec![];
            let mut rmc_services = vec![];
            let mut rmc_inputs = rmc_inputs.into_iter();
            let mut rmc_outputs = rmc_outputs.into_iter();
            for i in 0..node_count.0 {
                rmc_handlers.push(Handler::new(Keychain::new(node_count, NodeIndex(i))));
                rmc_services.push(Service::new(
                    rmc_inputs.next().expect("there should be enough rxs"),
                    rmc_outputs.next().expect("there should be enough txs"),
                    DoublingDelayScheduler::new(Duration::from_millis(1)),
                ));
            }
            TestEnvironment {
                network,
                rmc_handlers,
                rmc_services,
                multisigned_rxs,
            }
        }

        fn start_rmc(&mut self, node: NodeIndex, hash: Signable) {
            self.network
                .broadcast_message(RmcIncomingMessage::StartRmc(hash));
        }

        async fn collect_multisigned_hashes(
            mut self,
            count: usize,
        ) -> HashMap<NodeIndex, Vec<Multisigned<Signable, Keychain>>> {
            let node_count = NodeCount(self.rmc_services.len());
            let (exit_tx, exit_rx) = oneshot::channel();
            let mut terminator = Terminator::create_root(exit_rx, "root");

            let mut hashes = HashMap::new();
            let mut handlers = self.rmc_handlers.into_iter();
            let mut services = self.rmc_services.into_iter();
            let mut handles = vec![];

            for i in 0..node_count.0 {
                let rmc_terminator = terminator.add_offspring_connection("rmc");
                let handler = handlers.next().expect("there should be enough handlers");
                let service = services.next().expect("there should be enough services");
                handles.push(tokio::spawn(async move {
                    service.run(handler, rmc_terminator).await;
                }));
            }

            for _ in 0..count {
                if let (Some(unchecked), i, _) =
                    future::select_all(self.multisigned_rxs.iter_mut().map(|rx| rx.next())).await
                {
                    hashes
                        .entry(i.into())
                        .or_insert_with(Vec::new)
                        .push(unchecked);
                }
            }

            exit_tx.send(());
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
        let keychains = Keychain::new_vec(node_count);
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
        let keychains = Keychain::new_vec(node_count);
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
