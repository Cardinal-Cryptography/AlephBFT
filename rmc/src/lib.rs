//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
mod handler;
mod service;

pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use async_trait::async_trait;
use codec::{Decode, Encode};
use core::fmt::Debug;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    future::pending,
    FutureExt, StreamExt,
};
use futures_timer::Delay;
use log::{debug, warn};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    fmt::Formatter,
    hash::Hash,
    ops::{Add, Div, Mul},
    time::{Duration, Instant},
};

/// Abstraction of a task-scheduling logic
///
/// Because the network can be faulty, the task of sending a message must be performed multiple
/// times to ensure that the recipient receives each message.
/// The trait [`TaskScheduler<T>`] describes in what intervals some abstract task of type `T`
/// should be performed.
#[async_trait::async_trait]
pub trait TaskScheduler<T>: Send + Sync {
    fn add_task(&mut self, task: T);
    async fn next_task(&mut self) -> Option<T>;
}

/// An RMC message consisting of either a signed (indexed) hash, or a multisigned hash.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, Hash)]
pub enum Message<H: Signable, S: Signature, M: PartialMultisignature> {
    StartRmc(UncheckedSigned<Indexed<H>, S>),
    SignedHash(UncheckedSigned<Indexed<H>, S>),
    MultisignedHash(UncheckedSigned<H, M>),
}

impl<H: Signable, S: Signature, M: PartialMultisignature> Message<H, S, M> {
    pub fn hash(&self) -> &H {
        match self {
            Message::StartRmc(unchecked) => unchecked.as_signable_strip_index(),
            Message::SignedHash(unchecked) => unchecked.as_signable_strip_index(),
            Message::MultisignedHash(unchecked) => unchecked.as_signable(),
        }
    }
    pub fn is_complete(&self) -> bool {
        matches!(self, Message::MultisignedHash(_))
    }
}

/// A task of brodcasting a message.
#[derive(Clone)]
pub enum Task<H: Signable, MK: MultiKeychain> {
    BroadcastMessage(Message<H, MK::Signature, MK::PartialMultisignature>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScheduledTask<T> {
    task: T,
    delay: Duration,
}

impl<T> ScheduledTask<T> {
    fn new(task: T, delay: Duration) -> Self {
        ScheduledTask { task, delay }
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct IndexedInstant(Instant, usize);

impl IndexedInstant {
    fn at(instant: Instant, i: usize) -> Self {
        IndexedInstant(instant, i)
    }
}

/// A basic task scheduler scheduling tasks with an exponential slowdown
///
/// A scheduler parameterized by a duration `initial_delay`. When a task is added to the scheduler
/// it is first scheduled immediately, then it is scheduled indefinitely, where the first delay is
/// `initial_delay`, and each following delay for that task is two times longer than the previous
/// one.
pub struct DoublingDelayScheduler<T> {
    initial_delay: Duration,
    scheduled_instants: BinaryHeap<Reverse<IndexedInstant>>,
    scheduled_tasks: Vec<ScheduledTask<T>>,
}

impl<T> Debug for DoublingDelayScheduler<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoublingDelayScheduler")
            .field("initial delay", &self.initial_delay)
            .field("scheduled instant count", &self.scheduled_instants.len())
            .field("scheduled task count", &self.scheduled_tasks.len())
            .finish()
    }
}

impl<T> DoublingDelayScheduler<T> {
    pub fn new(initial_delay: Duration) -> Self {
        DoublingDelayScheduler::with_tasks(vec![], initial_delay)
    }

    pub fn with_tasks(initial_tasks: Vec<T>, initial_delay: Duration) -> Self {
        let mut scheduler = DoublingDelayScheduler {
            initial_delay,
            scheduled_instants: BinaryHeap::new(),
            scheduled_tasks: Vec::new(),
        };
        if initial_tasks.is_empty() {
            return scheduler;
        }
        let delta = initial_delay.div((initial_tasks.len()) as u32); // safety: len is non-zero
        for (i, task) in initial_tasks.into_iter().enumerate() {
            scheduler.add_task_after(task, delta.mul(i as u32));
        }
        scheduler
    }

    fn add_task_after(&mut self, task: T, delta: Duration) {
        let i = self.scheduled_tasks.len();
        let instant = Instant::now().add(delta);
        let indexed_instant = IndexedInstant::at(instant, i);
        self.scheduled_instants.push(Reverse(indexed_instant));
        let scheduled_task = ScheduledTask::new(task, self.initial_delay);
        self.scheduled_tasks.push(scheduled_task);
    }
}

#[async_trait]
impl<T: Send + Sync + Clone> TaskScheduler<T> for DoublingDelayScheduler<T> {
    fn add_task(&mut self, task: T) {
        self.add_task_after(task, Duration::ZERO);
    }

    async fn next_task(&mut self) -> Option<T> {
        match self.scheduled_instants.peek() {
            Some(&Reverse(IndexedInstant(instant, _))) => {
                let now = Instant::now();
                if now < instant {
                    Delay::new(instant - now).await;
                }
            }
            None => pending().await,
        }

        let Reverse(IndexedInstant(instant, i)) = self
            .scheduled_instants
            .pop()
            .expect("By the logic of the function, there is an instant available");
        let scheduled_task = &mut self.scheduled_tasks[i];

        let task = scheduled_task.task.clone();
        self.scheduled_instants
            .push(Reverse(IndexedInstant(instant + scheduled_task.delay, i)));

        scheduled_task.delay *= 2;
        Some(task)
    }
}





















#[cfg(test)]
mod tests {
    use crate::{DoublingDelayScheduler, Message, ReliableMulticast, TaskScheduler};
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
        rmcs: Vec<ReliableMulticast<Signable, Keychain>>,
    }

    impl TestData {
        fn new(
            node_count: NodeCount,
            keychains: &[Keychain],
            message_filter: impl FnMut(NodeIndex, TestMessage) -> bool + 'static,
        ) -> Self {
            let (network, channels) = TestNetwork::new(node_count, message_filter);
            let mut rmcs = Vec::new();
            for (i, (rx, tx)) in channels.into_iter().enumerate() {
                let rmc = ReliableMulticast::new(
                    rx,
                    tx,
                    keychains[i],
                    DoublingDelayScheduler::new(Duration::from_millis(1)),
                );
                rmcs.push(rmc);
            }
            TestData { network, rmcs }
        }

        async fn collect_multisigned_hashes(
            mut self,
            count: usize,
        ) -> HashMap<NodeIndex, Vec<Multisigned<Signable, Keychain>>> {
            let mut hashes = HashMap::new();

            for _ in 0..count {
                // covert each RMC into a future returning an optional unchecked multisigned hash.
                let rmc_futures: Vec<BoxFuture<Multisigned<Signable, Keychain>>> = self
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

    #[tokio::test]
    async fn scheduler_yields_proper_order_of_tasks() {
        let mut scheduler = DoublingDelayScheduler::new(Duration::from_millis(25));

        scheduler.add_task(0);
        tokio::time::sleep(Duration::from_millis(2)).await;
        scheduler.add_task(1);

        let task = scheduler.next_task().await;
        assert_eq!(task, Some(0));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(1));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(0));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(1));

        tokio::time::sleep(Duration::from_millis(2)).await;
        scheduler.add_task(2);

        let task = scheduler.next_task().await;
        assert_eq!(task, Some(2));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(2));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(0));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(1));
        let task = scheduler.next_task().await;
        assert_eq!(task, Some(2));
    }

    #[tokio::test]
    async fn scheduler_properly_handles_initial_bunch_of_tasks() {
        let tasks = (0..5).collect();
        let before = Instant::now();
        let mut scheduler = DoublingDelayScheduler::with_tasks(tasks, Duration::from_millis(25));

        for i in 0..5 {
            let task = scheduler.next_task().await;
            assert_eq!(task, Some(i));
            let now = Instant::now();
            // 0, 5, 10, 15, 20
            assert!(now - before >= Duration::from_millis(5).mul(i));
        }

        for i in 0..5 {
            let task = scheduler.next_task().await;
            assert_eq!(task, Some(i));
            let now = Instant::now();
            // 25, 30, 35, 40, 45
            assert!(
                now - before
                    >= Duration::from_millis(5)
                        .mul(i)
                        .add(Duration::from_millis(25))
            );
        }
    }

    #[tokio::test]
    async fn asking_empty_scheduler_for_next_task_blocks() {
        let mut scheduler: DoublingDelayScheduler<u32> =
            DoublingDelayScheduler::new(Duration::from_millis(25));
        let future = tokio::time::timeout(Duration::from_millis(30), scheduler.next_task());
        let result = future.await;
        assert!(result.is_err()); // elapsed
    }
}
