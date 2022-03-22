//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
pub use aleph_bft_crypto::{
    Indexed, Multisigned, PartialMultisignature, PartiallyMultisigned, Signature, Signed,
    UncheckedSigned,
};
pub use aleph_bft_types::{MultiKeychain, NodeCount, Signable, TaskScheduler};
use async_trait::async_trait;
use codec::{Decode, Encode};
use core::fmt::Debug;
use futures::{
    channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    FutureExt, StreamExt,
};
use futures_timer::Delay;
use log::{debug, warn};
use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap},
    hash::Hash,
    time,
    time::Duration,
};

/// An RMC message consisting of either a signed (indexed) hash, or a multisigned hash.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, Hash)]
pub enum Message<H: Signable, S: Signature, M: PartialMultisignature> {
    SignedHash(UncheckedSigned<Indexed<H>, S>),
    MultisignedHash(UncheckedSigned<H, M>),
}

impl<H: Signable, S: Signature, M: PartialMultisignature> Message<H, S, M> {
    pub fn hash(&self) -> &H {
        match self {
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
    delay: time::Duration,
}

impl<T> ScheduledTask<T> {
    fn new(task: T, delay: time::Duration) -> Self {
        ScheduledTask { task, delay }
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct IndexedInstant(time::Instant, usize);

impl IndexedInstant {
    fn now(i: usize) -> Self {
        let curr_time = time::Instant::now();
        IndexedInstant(curr_time, i)
    }
}

/// A basic task scheduler scheduling tasks with an exponential slowdown
///
/// A scheduler parameterized by a duration `initial_delay`. When a task is added to the scheduler
/// it is first scheduled immediately, then it is scheduled indefinitely, where the first delay is
/// `initial_delay`, and each following delay for that task is two times longer than the previous
/// one.
pub struct DoublingDelayScheduler<T> {
    initial_delay: time::Duration,
    scheduled_instants: BinaryHeap<Reverse<IndexedInstant>>,
    scheduled_tasks: Vec<ScheduledTask<T>>,
    on_new_task_tx: UnboundedSender<T>,
    on_new_task_rx: UnboundedReceiver<T>,
}

impl<T> DoublingDelayScheduler<T> {
    pub fn new(initial_delay: time::Duration) -> Self {
        let (on_new_task_tx, on_new_task_rx) = unbounded();
        DoublingDelayScheduler {
            initial_delay,
            scheduled_instants: BinaryHeap::new(),
            scheduled_tasks: Vec::new(),
            on_new_task_tx,
            on_new_task_rx,
        }
    }
}

#[async_trait]
impl<T: Send + Sync + Clone> TaskScheduler<T> for DoublingDelayScheduler<T> {
    fn add_task(&mut self, task: T) {
        self.on_new_task_tx
            .unbounded_send(task)
            .expect("We own the the rx, so this can't fail");
    }

    async fn next_task(&mut self) -> Option<T> {
        let mut delay: futures::future::Fuse<_> = match self.scheduled_instants.peek() {
            Some(&Reverse(IndexedInstant(instant, _))) => {
                let now = time::Instant::now();
                if now > instant {
                    Delay::new(Duration::new(0, 0)).fuse()
                } else {
                    Delay::new(instant - now).fuse()
                }
            }
            None => futures::future::Fuse::terminated(),
        };
        // wait until either the scheduled time of the peeked task or a next call of add_task
        futures::select! {
            _ = delay => {},
            task = self.on_new_task_rx.next() => {
                if let Some(task) = task {
                    let i = self.scheduled_tasks.len();
                    let indexed_instant = IndexedInstant::now(i);
                    self.scheduled_instants.push(Reverse(indexed_instant));
                    let scheduled_task = ScheduledTask::new(task, self.initial_delay);
                    self.scheduled_tasks.push(scheduled_task);
                } else {
                    return None;
                }
            }
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

/// Reliable Multicast Box
///
/// The instance of [`ReliableMulticast<'a, H, MK>`] reliably broadcasts hashes of type `H`,
/// and when a hash is successfully broadcasted, the multisigned hash `Multisigned<'a, H, MK>`
/// is asynchronously returned.
///
/// A node with an instance of [`ReliableMulticast<'a, H, MK>`] can initiate broadcasting
/// a message `msg: H` by calling the [`ReliableMulticast::start_rmc`] method. As a result,
/// the node signs `msg` and starts broadcasting the signed message via the network.
/// When sufficintly many nodes call [`ReliableMulticast::start_rmc`] with the same message `msg`
/// and a node collects enough signatures to form a complete multisignature under the message,
/// the multisigned message is yielded by the instance of [`ReliableMulticast`].
/// The multisigned messages can be polled by calling [`ReliableMulticast::next_multisigned_hash`].
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/reliable_broadcast.html
/// for a high-level description of this protocol and how it is used for fork alerts.
pub struct ReliableMulticast<'a, H: Signable + Hash, MK: MultiKeychain> {
    hash_states: HashMap<H, PartiallyMultisigned<'a, H, MK>>,
    network_rx: UnboundedReceiver<Message<H, MK::Signature, MK::PartialMultisignature>>,
    network_tx: UnboundedSender<Message<H, MK::Signature, MK::PartialMultisignature>>,
    keychain: &'a MK,
    scheduler: Box<dyn TaskScheduler<Task<H, MK>>>,
    multisigned_hashes_tx: UnboundedSender<Multisigned<'a, H, MK>>,
    multisigned_hashes_rx: UnboundedReceiver<Multisigned<'a, H, MK>>,
}

impl<'a, H: Signable + Hash + Eq + Clone + Debug, MK: MultiKeychain> ReliableMulticast<'a, H, MK> {
    pub fn new(
        network_rx: UnboundedReceiver<Message<H, MK::Signature, MK::PartialMultisignature>>,
        network_tx: UnboundedSender<Message<H, MK::Signature, MK::PartialMultisignature>>,
        keychain: &'a MK,
        //kept for compatibility
        _node_count: NodeCount,
        scheduler: impl TaskScheduler<Task<H, MK>> + 'static,
    ) -> Self {
        let (multisigned_hashes_tx, multisigned_hashes_rx) = unbounded();
        ReliableMulticast {
            hash_states: HashMap::new(),
            network_rx,
            network_tx,
            keychain,
            scheduler: Box::new(scheduler),
            multisigned_hashes_tx,
            multisigned_hashes_rx,
        }
    }

    /// Initiate a new instance of RMC for `hash`.
    pub async fn start_rmc(&mut self, hash: H) {
        debug!(target: "AlephBFT-rmc", "starting rmc for {:?}", hash);
        let signed_hash = Signed::sign_with_index(hash, self.keychain).await;

        let message = Message::SignedHash(signed_hash.into_unchecked());
        self.handle_message(message.clone());
        let task = Task::BroadcastMessage(message);
        self.do_task(task.clone());
        self.scheduler.add_task(task);
    }

    fn on_complete_multisignature(&mut self, multisigned: Multisigned<'a, H, MK>) {
        let hash = multisigned.as_signable().clone();
        self.hash_states.insert(
            hash,
            PartiallyMultisigned::Complete {
                multisigned: multisigned.clone(),
            },
        );
        self.multisigned_hashes_tx
            .unbounded_send(multisigned.clone())
            .expect("We own the the rx, so this can't fail");

        let task = Task::BroadcastMessage(Message::MultisignedHash(multisigned.into_unchecked()));
        self.do_task(task.clone());
        self.scheduler.add_task(task);
    }

    fn handle_message(&mut self, message: Message<H, MK::Signature, MK::PartialMultisignature>) {
        let hash = message.hash().clone();
        if let Some(PartiallyMultisigned::Complete { .. }) = self.hash_states.get(&hash) {
            return;
        }
        match message {
            Message::MultisignedHash(unchecked) => match unchecked.check_multi(self.keychain) {
                Ok(multisigned) => {
                    self.on_complete_multisignature(multisigned);
                }
                Err(_) => {
                    warn!(target: "AlephBFT-rmc", "Received a hash with a bad multisignature");
                }
            },
            Message::SignedHash(unchecked) => {
                let signed_hash = match unchecked.check(self.keychain) {
                    Ok(signed_hash) => signed_hash,
                    Err(_) => {
                        warn!(target: "AlephBFT-rmc", "Received a hash with a bad signature");
                        return;
                    }
                };

                let new_state = match self.hash_states.remove(&hash) {
                    None => signed_hash.into_partially_multisigned(self.keychain),
                    Some(partial) => partial.add_signature(signed_hash, self.keychain),
                };
                match new_state {
                    PartiallyMultisigned::Complete { multisigned } => {
                        self.on_complete_multisignature(multisigned)
                    }
                    incomplete => {
                        self.hash_states.insert(hash.clone(), incomplete);
                    }
                }
            }
        }
    }

    fn do_task(&self, task: Task<H, MK>) {
        let Task::BroadcastMessage(message) = task;
        self.network_tx
            .unbounded_send(message)
            .expect("Sending message should succeed");
    }

    /// Fetches final multisignature.
    pub fn get_multisigned(&self, hash: &H) -> Option<Multisigned<'a, H, MK>> {
        match self.hash_states.get(hash)? {
            PartiallyMultisigned::Complete { multisigned } => Some(multisigned.clone()),
            _ => None,
        }
    }

    /// Perform underlying tasks until the multisignature for the hash of this instance is collected.
    pub async fn next_multisigned_hash(&mut self) -> Multisigned<'a, H, MK> {
        loop {
            futures::select! {
                multisigned_hash = self.multisigned_hashes_rx.next() => {
                    return multisigned_hash.expect("We own the tx, so it is not closed");
                }

                incoming_message = self.network_rx.next() => {
                    if let Some(incoming_message) = incoming_message {
                        self.handle_message(incoming_message);
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
    use crate::{DoublingDelayScheduler, Message, ReliableMulticast};
    use aleph_bft_crypto::{
        KeyBox, MultiKeychain, Multisigned, PartialMultisignature, SignatureSet, Signed,
    };
    use aleph_bft_types::{Index, NodeCount, NodeIndex, Signable};
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
            SignatureSet::add_signature(
                SignatureSet::with_size(self.node_count()),
                signature,
                index,
            )
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

    /// 7 honest nodes and 3 dishonest nodes which emit bad signatures and multisignatures
    #[tokio::test]
    async fn bad_signatures_and_multisignatures_are_ignored() {
        #[derive(Clone, Debug)]
        struct BadKeyBox {
            count: NodeCount,
            index: NodeIndex,
            signature_index: NodeIndex,
        }

        impl BadKeyBox {
            fn new(count: NodeCount, index: NodeIndex, signature_index: NodeIndex) -> Self {
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

        let node_count = NodeCount(10);
        let keychains = prepare_keychains(node_count);
        let mut data = TestData::new(node_count, &keychains, |_, _| true);

        let bad_hash = Hash { byte: 65 };
        let bad_keybox = BadKeyBox::new(node_count, 0.into(), 111.into());
        let bad_msg =
            TestMessage::SignedHash(Signed::sign_with_index(bad_hash, &bad_keybox).await.into());
        data.network.broadcast_message(bad_msg);
        let bad_msg = TestMessage::MultisignedHash(
            Signed::sign_with_index(bad_hash, &bad_keybox)
                .await
                .into_partially_multisigned(&bad_keybox)
                .into_unchecked(),
        );
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
}