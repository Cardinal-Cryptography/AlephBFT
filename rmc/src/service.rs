//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
use crate::{
    handler::Handler, scheduler::TaskScheduler, RmcHash, RmcIncomingMessage, RmcOutgoingMessage,
    Task,
};
pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use core::fmt::Debug;
use futures::{
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
    FutureExt, StreamExt,
};
use log::{debug, error, warn};
use std::hash::Hash;
use aleph_bft_types::Terminator;

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

// #[cfg(test)]
// mod tests {
//     use crate::{DoublingDelayScheduler, RmcHash, ReliableMulticast, TaskScheduler, RmcOutgoingMessage, RmcIncomingMessage};
//     use aleph_bft_crypto::{Multisigned, NodeCount, NodeIndex, Signed};
//     use aleph_bft_mock::{BadSigning, Keychain, PartialMultisignature, Signable, Signature};
//     use futures::{
//         channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
//         future::{self, BoxFuture},
//         stream::{self, Stream},
//         FutureExt, StreamExt,
//     };
//     use rand::Rng;
//     use std::{
//         collections::HashMap,
//         ops::{Add, Mul},
//         pin::Pin,
//         time::{Duration, Instant},
//     };
//     use crate::handler::Handler;
//     use crate::scheduler::DoublingDelayScheduler;
//     use crate::service::Service;
//
//     struct TestNetwork {
//         outgoing_rx: Pin<Box<dyn Stream<Item = RmcIncomingMessage<Signable, Signature, PartialMultisignature>>>>,
//         incoming_txs: Vec<UnboundedSender<RmcOutgoingMessage<Signable, Keychain>>>,
//         message_filter: Box<dyn FnMut(NodeIndex, RmcIncomingMessage<Signable, Signature, PartialMultisignature>) -> bool>,
//     }
//
//     type ReceiversSenders = Vec<(UnboundedReceiver<RmcOutgoingMessage<Signable, Keychain>>, UnboundedSender<RmcIncomingMessage<Signable, Signature, PartialMultisignature>>)>;
//     impl TestNetwork {
//         fn new(
//             node_count: NodeCount,
//             message_filter: impl FnMut(NodeIndex, RmcIncomingMessage<Signable, Signature, PartialMultisignature>) -> bool + 'static,
//         ) -> (Self, ReceiversSenders) {
//             let all_nodes: Vec<_> = (0..node_count.0).map(NodeIndex).collect();
//             let (incomng_txs, incoming_rxs): (Vec<_>, Vec<_>) =
//                 all_nodes.iter().map(|_| unbounded()).unzip();
//             let (outgoing_txs, outgoing_rxs): (Vec<_>, Vec<_>) = {
//                 all_nodes
//                     .iter()
//                     .map(|_| {
//                         let (tx, rx) = unbounded();
//                         (tx, rx)
//                     })
//                     .unzip()
//             };
//             let network = TestNetwork {
//                 outgoing_rx: Box::pin(stream::select_all(outgoing_rxs)),
//                 incoming_txs: incomng_txs,
//                 message_filter: Box::new(message_filter),
//             };
//
//             let channels = incoming_rxs.into_iter().zip(outgoing_txs).collect();
//             (network, channels)
//         }
//
//         fn broadcast_message(&mut self, msg: RmcOutgoingMessage<Signable, Keychain>) {
//             for tx in &mut self.incoming_txs {
//                 tx.unbounded_send(msg.clone())
//                     .expect("Channel should be open");
//             }
//         }
//
//         async fn run(&mut self) {
//             while let Some(message) = self.outgoing_rx.next().await {
//                 for (i, tx) in self.incoming_txs.iter().enumerate() {
//                     if (self.message_filter)(NodeIndex(i), message.clone()) {
//                         tx.unbounded_send(message.clone())
//                             .expect("Channel should be open");
//                     }
//                 }
//             }
//         }
//     }
//
//     struct TestData {
//         network: TestNetwork,
//         services: Vec<Service<Signable, Keychain>>,
//         handlers: Vec<Handler<Signable, Keychain>>,
//     }
//
//     impl TestData {
//         fn new(
//             node_count: NodeCount,
//             keychains: &[Keychain],
//             message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
//         ) -> Self {
//             let (network, channels) = TestNetwork::new(node_count, message_filter);
//             let mut handlers = vec![];
//             let mut services = vec![];
//
//             for (i, (rx, tx)) in channels.into_iter().enumerate() {
//                 let handler = Handler::new(keychains[i]);
//                 handlers.push(handler);
//                 let service = Service::new(keychains[i], rx, tx, DoublingDelayScheduler::new(Duration::from_millis(1)));
//                 services.push(service);
//             }
//             TestData { network, rmcs }
//         }
//
//         async fn collect_multisigned_hashes(
//             mut self,
//             count: usize,
//         ) -> HashMap<NodeIndex, Vec<Multisigned<Signable, Keychain>>> {
//             let mut hashes = HashMap::new();
//
//             for _ in 0..count {
//                 // covert each RMC into a future returning an optional unchecked multisigned hash.
//                 let rmc_futures: Vec<BoxFuture<Multisigned<Signable, Keychain>>> = self
//                     .rmcs
//                     .iter_mut()
//                     .map(|rmc| rmc.next_multisigned_hash().boxed())
//                     .collect();
//                 tokio::select! {
//                     (unchecked, i, _) = future::select_all(rmc_futures) => {
//                         hashes.entry(i.into()).or_insert_with(Vec::new).push(unchecked);
//                     }
//                     _ = self.network.run() => {
//                         panic!("network ended unexpectedly");
//                     }
//                 }
//             }
//             hashes
//         }
//     }
//
//     /// Create 10 honest nodes and let each of them start rmc for the same hash.
//     #[tokio::test]
//     async fn simple_scenario() {
//         let node_count = NodeCount(10);
//         let keychains = Keychain::new_vec(node_count);
//         let mut data = TestData::new(node_count, &keychains, |_, _| true);
//
//         let hash: Signable = "56".into();
//         for i in 0..node_count.0 {
//             data.rmcs[i].start_rmc(hash.clone());
//         }
//
//         let hashes = data.collect_multisigned_hashes(node_count.0).await;
//         assert_eq!(hashes.len(), node_count.0);
//         for i in 0..node_count.0 {
//             let multisignatures = &hashes[&i.into()];
//             assert_eq!(multisignatures.len(), 1);
//             assert_eq!(multisignatures[0].as_signable(), &hash);
//         }
//     }
//
//     /// Each message is delivered with 20% probability
//     #[tokio::test]
//     async fn faulty_network() {
//         let node_count = NodeCount(10);
//         let keychains = Keychain::new_vec(node_count);
//         let mut rng = rand::thread_rng();
//         let mut data = TestData::new(node_count, &keychains, move |_, _| rng.gen_range(0..5) == 0);
//
//         let hash: Signable = "56".into();
//         for i in 0..node_count.0 {
//             data.rmcs[i].start_rmc(hash.clone());
//         }
//
//         let hashes = data.collect_multisigned_hashes(node_count.0).await;
//         assert_eq!(hashes.len(), node_count.0);
//         for i in 0..node_count.0 {
//             let multisignatures = &hashes[&i.into()];
//             assert_eq!(multisignatures.len(), 1);
//             assert_eq!(multisignatures[0].as_signable(), &hash);
//         }
//     }
//
//     /// Only 7 nodes start rmc and one of the nodes which didn't start rmc
//     /// is delivered only messages with complete multisignatures
//     #[tokio::test]
//     async fn node_hearing_only_multisignatures() {
//         let node_count = NodeCount(10);
//         let keychains = Keychain::new_vec(node_count);
//         let mut data = TestData::new(node_count, &keychains, move |node_ix, message| {
//             !matches!((node_ix.0, message), (0, RmcHash::SignedHash(_)))
//         });
//
//         let threshold = (2 * node_count.0 + 1) / 3;
//         let hash: Signable = "56".into();
//         for i in 0..threshold {
//             data.rmcs[i].start_rmc(hash.clone());
//         }
//
//         let hashes = data.collect_multisigned_hashes(node_count.0).await;
//         assert_eq!(hashes.len(), node_count.0);
//         for i in 0..node_count.0 {
//             let multisignatures = &hashes[&i.into()];
//             assert_eq!(multisignatures.len(), 1);
//             assert_eq!(multisignatures[0].as_signable(), &hash);
//         }
//     }
//
//     /// 7 honest nodes and 3 dishonest nodes which emit bad signatures and multisignatures
//     #[tokio::test]
//     async fn bad_signatures_and_multisignatures_are_ignored() {
//         let node_count = NodeCount(10);
//         let keychains = Keychain::new_vec(node_count);
//         let mut data = TestData::new(node_count, &keychains, |_, _| true);
//
//         let bad_hash: Signable = "65".into();
//         let bad_keychain: BadSigning<Keychain> = Keychain::new(node_count, 0.into()).into();
//         let bad_msg = TestMessage::SignedHash(
//             Signed::sign_with_index(bad_hash.clone(), &bad_keychain).into(),
//         );
//         data.network.broadcast_message(bad_msg);
//         let bad_msg = TestMessage::MultisignedHash(
//             Signed::sign_with_index(bad_hash.clone(), &bad_keychain)
//                 .into_partially_multisigned(&bad_keychain)
//                 .into_unchecked(),
//         );
//         data.network.broadcast_message(bad_msg);
//
//         let hash: Signable = "56".into();
//         for i in 0..node_count.0 {
//             data.rmcs[i].start_rmc(hash.clone());
//         }
//
//         let hashes = data.collect_multisigned_hashes(node_count.0).await;
//         assert_eq!(hashes.len(), node_count.0);
//         for i in 0..node_count.0 {
//             let multisignatures = &hashes[&i.into()];
//             assert_eq!(multisignatures.len(), 1);
//             assert_eq!(multisignatures[0].as_signable(), &hash);
//         }
//     }
// }
