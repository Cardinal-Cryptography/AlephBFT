use crate::{
    exponential_slowdown, run_session, Config, DataProvider as DataProviderT, DelayConfig,
    FinalizationHandler as FinalizationHandlerT, Hasher, Index, KeyBox as KeyBoxT,
    MultiKeychain as MultiKeychainT, Network as NetworkT, NodeCount, NodeIndex,
    PartialMultisignature as PartialMultisignatureT, Receiver, Recipient, Sender, SpawnHandle,
    TaskHandle,
};
use async_trait::async_trait;
use codec::{Decode, Encode};
use futures::{
    channel::{
        mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    Future, StreamExt,
};
use log::{debug, error};
use parking_lot::Mutex;
use std::{
    cell::RefCell,
    collections::{hash_map::DefaultHasher, HashMap},
    hash::Hasher as StdHasher,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

pub fn gen_config(node_ix: NodeIndex, n_members: NodeCount) -> Config {
    let delay_config = DelayConfig {
        tick_interval: Duration::from_millis(5),
        requests_interval: Duration::from_millis(50),
        unit_broadcast_delay: Arc::new(|t| exponential_slowdown(t, 100.0, 1, 3.0)),
        //100, 100, 300, 900, 2700, ...
        unit_creation_delay: Arc::new(|t| exponential_slowdown(t, 50.0, usize::MAX, 1.000)),
        //50, 50, 50, 50, ...
    };
    Config {
        node_ix,
        session_id: 0,
        n_members,
        delay_config,
        max_round: 5000,
    }
}

pub fn spawn_honest_member(
    spawner: Spawner,
    node_index: NodeIndex,
    n_members: NodeCount,
    network: impl 'static + NetworkT<NetworkData>,
) -> (UnboundedReceiver<Data>, oneshot::Sender<()>, TaskHandle) {
    let data_provider = DataProvider::new();

    let (finalization_provider, finalization_rx) = FinalizationHandler::new();
    let config = gen_config(node_index, n_members);
    let (exit_tx, exit_rx) = oneshot::channel();
    let spawner_inner = spawner.clone();
    let member_task = async move {
        let keybox = KeyBox::new(n_members, node_index);
        run_session(
            config,
            network,
            data_provider,
            finalization_provider,
            keybox,
            spawner_inner.clone(),
            exit_rx,
        )
        .await
    };
    let handle = spawner.spawn_essential("member", member_task);
    (finalization_rx, exit_tx, handle)
}

// A hasher from the standard library that hashes to u64, should be enough to
// avoid collisions in testing.
#[derive(PartialEq, Eq, Hash, Clone, Debug)]
pub struct Hasher64;

impl Hasher for Hasher64 {
    type Hash = [u8; 8];

    fn hash(x: &[u8]) -> Self::Hash {
        let mut hasher = DefaultHasher::new();
        hasher.write(x);
        hasher.finish().to_ne_bytes()
    }
}

pub(crate) type Hash64 = <Hasher64 as Hasher>::Hash;

#[derive(Clone)]
pub struct Spawner {}

impl SpawnHandle for Spawner {
    fn spawn(&self, _name: &str, task: impl Future<Output = ()> + Send + 'static) {
        tokio::spawn(task);
    }

    fn spawn_essential(
        &self,
        _: &str,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle {
        let (res_tx, res_rx) = oneshot::channel();
        tokio::spawn(async move {
            task.await;
            res_tx.send(()).expect("We own the rx.");
        });
        Box::pin(async move { res_rx.await.map_err(|_| ()) })
    }
}

impl Spawner {
    pub fn new() -> Self {
        Spawner {}
    }
}

impl Default for Spawner {
    fn default() -> Self {
        Spawner::new()
    }
}

pub type NetworkData = crate::NetworkData<Hasher64, Data, Signature, PartialMultisignature>;
type NetworkReceiver = UnboundedReceiver<(NetworkData, NodeIndex)>;
type NetworkSender = UnboundedSender<(NetworkData, NodeIndex)>;

pub struct Network {
    rx: NetworkReceiver,
    tx: NetworkSender,
    peers: Vec<NodeIndex>,
    index: NodeIndex,
}

impl Network {
    pub fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait::async_trait]
impl NetworkT<NetworkData> for Network {
    fn send(&self, data: NetworkData, recipient: Recipient) {
        use Recipient::*;
        match recipient {
            Node(node) => self
                .tx
                .unbounded_send((data, node))
                .expect("send on channel should work"),
            Everyone => {
                for peer in self.peers.iter() {
                    if *peer != self.index {
                        self.send(data.clone(), Node(*peer));
                    }
                }
            }
        }
    }

    async fn next_event(&mut self) -> Option<NetworkData> {
        Some(self.rx.next().await?.0)
    }
}

struct Peer {
    tx: NetworkSender,
    rx: NetworkReceiver,
}

pub struct UnreliableRouter {
    peers: RefCell<HashMap<NodeIndex, Peer>>,
    peer_list: Vec<NodeIndex>,
    hook_list: RefCell<Vec<Box<dyn NetworkHook>>>,
    reliability: f64, //a number in the range [0, 1], 1.0 means perfect reliability, 0.0 means no message gets through
}

impl UnreliableRouter {
    pub(crate) fn new(peer_list: Vec<NodeIndex>, reliability: f64) -> Self {
        UnreliableRouter {
            peers: RefCell::new(HashMap::new()),
            peer_list,
            hook_list: RefCell::new(Vec::new()),
            reliability,
        }
    }

    pub fn add_hook<HK: NetworkHook + 'static>(&mut self, hook: HK) {
        self.hook_list.borrow_mut().push(Box::new(hook));
    }

    pub(crate) fn connect_peer(&mut self, peer: NodeIndex) -> Network {
        assert!(
            self.peer_list.iter().any(|p| *p == peer),
            "Must connect a peer in the list."
        );
        assert!(
            !self.peers.borrow().contains_key(&peer),
            "Cannot connect a peer twice."
        );
        let (tx_in_hub, rx_in_hub) = unbounded();
        let (tx_out_hub, rx_out_hub) = unbounded();
        let peer_entry = Peer {
            tx: tx_out_hub,
            rx: rx_in_hub,
        };
        self.peers.borrow_mut().insert(peer, peer_entry);
        Network {
            rx: rx_out_hub,
            tx: tx_in_hub,
            peers: self.peer_list.clone(),
            index: peer,
        }
    }
}

impl Future for UnreliableRouter {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut self;
        let mut disconnected_peers: Vec<NodeIndex> = Vec::new();
        let mut buffer = Vec::new();
        for (peer_id, peer) in this.peers.borrow_mut().iter_mut() {
            loop {
                // this call is responsible for waking this Future
                match peer.rx.poll_next_unpin(cx) {
                    Poll::Ready(Some(msg)) => {
                        buffer.push((*peer_id, msg));
                    }
                    Poll::Ready(None) => {
                        disconnected_peers.push(*peer_id);
                        break;
                    }
                    Poll::Pending => {
                        break;
                    }
                }
            }
        }
        for peer_id in disconnected_peers {
            this.peers.borrow_mut().remove(&peer_id);
        }
        for (sender, (mut data, recipient)) in buffer {
            let rand_sample = rand::random::<f64>();
            if rand_sample > this.reliability {
                debug!("Simulated network fail.");
                continue;
            }

            if let Some(peer) = this.peers.borrow().get(&recipient) {
                for hook in this.hook_list.borrow_mut().iter_mut() {
                    match hook
                        .update_state(&mut data, sender, recipient)
                        .as_mut()
                        .poll(cx)
                    {
                        Poll::Ready(()) => (),
                        Poll::Pending => panic!(),
                    }
                }
                peer.tx
                    .unbounded_send((data, sender))
                    .expect("channel should be open");
            }
        }
        if this.peers.borrow().is_empty() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

pub fn configure_network(
    n_members: NodeCount,
    reliability: f64,
) -> (UnreliableRouter, Vec<Network>) {
    let peer_list = n_members.into_iterator().collect();
    let mut router = UnreliableRouter::new(peer_list, reliability);
    let mut networks = Vec::new();
    for ix in n_members.into_iterator() {
        let network = router.connect_peer(ix);
        networks.push(network);
    }
    (router, networks)
}

#[async_trait]
pub trait NetworkHook: Send {
    /// This must complete during a single poll - the current implementation
    /// of UnreliableRouter will panic if polling this method returns Poll::Pending.
    async fn update_state(
        &mut self,
        data: &mut NetworkData,
        sender: NodeIndex,
        recipient: NodeIndex,
    );
}

#[derive(Clone)]
pub(crate) struct AlertHook {
    alerts_sent_by_connection: Arc<Mutex<HashMap<(NodeIndex, NodeIndex), usize>>>,
}

impl AlertHook {
    pub(crate) fn new() -> Self {
        AlertHook {
            alerts_sent_by_connection: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn count(&self, sender: NodeIndex, recipient: NodeIndex) -> usize {
        match self
            .alerts_sent_by_connection
            .lock()
            .get(&(sender, recipient))
        {
            Some(count) => *count,
            None => 0,
        }
    }
}

#[async_trait]
impl NetworkHook for AlertHook {
    async fn update_state(
        &mut self,
        data: &mut NetworkData,
        sender: NodeIndex,
        recipient: NodeIndex,
    ) {
        use crate::{alerts::AlertMessage::*, network::NetworkDataInner::*};
        if let crate::NetworkData(Alert(ForkAlert(_))) = data {
            *self
                .alerts_sent_by_connection
                .lock()
                .entry((sender, recipient))
                .or_insert(0) += 1;
        }
    }
}

pub type Data = u32;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct Signature {}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct PartialMultisignature {
    signed_by: Vec<NodeIndex>,
}

impl PartialMultisignatureT for PartialMultisignature {
    type Signature = Signature;
    fn add_signature(self, _: &Self::Signature, index: NodeIndex) -> Self {
        let Self { mut signed_by } = self;
        for id in &signed_by {
            if *id == index {
                return Self { signed_by };
            }
        }
        signed_by.push(index);
        Self { signed_by }
    }
}

pub(crate) struct DataProvider;

impl DataProvider {
    pub(crate) fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DataProviderT<Data> for DataProvider {
    async fn get_data(&mut self) -> Data {
        0
    }
}

pub(crate) struct FinalizationHandler {
    tx: Sender<Data>,
}

#[async_trait]
impl FinalizationHandlerT<Data> for FinalizationHandler {
    async fn data_finalized(&mut self, d: Data) {
        if let Err(e) = self.tx.unbounded_send(d) {
            error!(target: "finalization-provider", "Error when sending data from FinalizationProvider {:?}.", e);
        }
    }
}

impl FinalizationHandler {
    pub(crate) fn new() -> (Self, Receiver<Data>) {
        let (tx, rx) = unbounded();

        (Self { tx }, rx)
    }
}

#[derive(Clone)]
pub(crate) struct KeyBox {
    count: NodeCount,
    ix: NodeIndex,
}

impl KeyBox {
    pub(crate) fn new(count: NodeCount, ix: NodeIndex) -> Self {
        KeyBox { count, ix }
    }
}

impl Index for KeyBox {
    fn index(&self) -> NodeIndex {
        self.ix
    }
}

#[async_trait]
impl KeyBoxT for KeyBox {
    type Signature = Signature;

    fn node_count(&self) -> NodeCount {
        self.count
    }

    async fn sign(&self, _msg: &[u8]) -> Signature {
        Signature {}
    }

    fn verify(&self, _msg: &[u8], _sgn: &Signature, _index: NodeIndex) -> bool {
        true
    }
}

impl MultiKeychainT for KeyBox {
    type PartialMultisignature = PartialMultisignature;
    fn from_signature(&self, _: &Self::Signature, index: NodeIndex) -> Self::PartialMultisignature {
        let signed_by = vec![index];
        PartialMultisignature { signed_by }
    }
    fn is_complete(&self, _: &[u8], partial: &Self::PartialMultisignature) -> bool {
        (self.count * 2) / 3 < NodeCount(partial.signed_by.len())
    }
}
