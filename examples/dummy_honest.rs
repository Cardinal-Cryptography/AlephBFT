use codec::{Decode, Encode};
use futures::{
    channel::{
        mpsc::{self, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    Future, FutureExt, StreamExt,
};
use log::info;
use parking_lot::Mutex;
use rush::{NodeIndex, OrderedBatch};
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    error::Error,
    hash::Hasher as StdHasher,
    pin::Pin,
    sync::Arc,
};
use tokio::time::Duration;

use libp2p::{
    development_transport,
    floodsub::{self, Floodsub, FloodsubEvent},
    identity,
    mdns::{Mdns, MdnsConfig, MdnsEvent},
    swarm::{NetworkBehaviourEventProcess, SwarmBuilder},
    NetworkBehaviour, PeerId, Swarm,
};

const USAGE_MSG: &str = "Missing arg. Usage
    cargo run --example honest my_id n_members n_finalized

    my_id -- our index
    n_members -- size of the committee
    n_finalized -- number of data to be finalized";

fn parse_arg(n: usize) -> usize {
    if let Some(int) = std::env::args().nth(n) {
        match int.parse::<usize>() {
            Ok(int) => int,
            Err(err) => {
                panic!("Failed to parse arg {:?}", err);
            }
        }
    } else {
        panic!("{}", USAGE_MSG);
    }
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_module("dummy_honest", log::LevelFilter::Info)
        .init();

    let my_id = parse_arg(1);
    let n_members = parse_arg(2);
    let n_finalized = parse_arg(3);

    info!("Getting network up.");
    let (network, mut manager) = Network::new().await.unwrap();
    let (close_network, exit) = oneshot::channel();
    tokio::spawn(async move { manager.run(exit).await });

    let (data_io, mut to_finalize) = DataIO::new();

    let (close_member, exit) = oneshot::channel();
    tokio::spawn(async move {
        let keybox = KeyBox {
            index: my_id.into(),
        };
        let config = rush::default_config(n_members.into(), my_id.into(), 0);
        let member = rush::Member::new(data_io, &keybox, config, Spawner {});

        member.run_session(network, exit).await
    });

    let mut finalized = HashSet::new();
    while let Some(batch) = to_finalize.next().await {
        for data in batch {
            if !finalized.contains(&data) {
                finalized.insert(data);
            }
        }
        info!("Got new batch. Finalized = {:?}", finalized.len());
        if finalized.len() == n_finalized {
            break;
        }
    }
    close_member.send(()).expect("should send");
    close_network.send(()).expect("should send");
}

// This is not cryptographically secure, mocked only for demonstration purposes
#[derive(PartialEq, Eq, Clone, Debug)]
struct Hasher64;

impl rush::Hasher for Hasher64 {
    type Hash = [u8; 8];

    fn hash(x: &[u8]) -> Self::Hash {
        let mut hasher = DefaultHasher::new();
        hasher.write(x);
        hasher.finish().to_ne_bytes()
    }
}

type Data = u64;
struct DataIO {
    next_data: Arc<Mutex<u64>>,
    finalized_tx: UnboundedSender<OrderedBatch<Data>>,
}

impl rush::DataIO<Data> for DataIO {
    type Error = ();
    fn get_data(&self) -> Data {
        let mut data = self.next_data.lock();
        *data += 1;

        *data
    }
    fn check_availability(
        &self,
        _data: &Data,
    ) -> Option<Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send>>> {
        None
    }
    fn send_ordered_batch(&mut self, data: OrderedBatch<Data>) -> Result<(), Self::Error> {
        self.finalized_tx.unbounded_send(data).map_err(|_| ())
    }
}

impl DataIO {
    fn new() -> (Self, UnboundedReceiver<OrderedBatch<Data>>) {
        let (finalized_tx, finalized_rx) = mpsc::unbounded();
        (
            DataIO {
                next_data: Arc::new(Mutex::new(0)),
                finalized_tx,
            },
            finalized_rx,
        )
    }
}

// This is not cryptographically secure, mocked only for demonstration purposes
#[derive(Clone, Debug, Encode, Decode)]
struct Signature;

// This is not cryptographically secure, mocked only for demonstration purposes
#[derive(Clone)]
struct KeyBox {
    index: NodeIndex,
}

impl rush::KeyBox for KeyBox {
    type Signature = Signature;
    fn sign(&self, _msg: &[u8]) -> Self::Signature {
        Signature {}
    }
    fn verify(&self, _msg: &[u8], _sgn: &Self::Signature, _index: NodeIndex) -> bool {
        true
    }
}

impl rush::Index for KeyBox {
    fn index(&self) -> NodeIndex {
        self.index
    }
}

#[derive(Clone)]
struct Spawner;

impl rush::SpawnHandle for Spawner {
    fn spawn(&self, _: &str, task: impl Future<Output = ()> + Send + 'static) {
        tokio::spawn(task);
    }
}

const ALEPH_PROTOCOL_NAME: &str = "aleph";

type NetworkData = rush::NetworkData<Hasher64, Data, Signature>;

#[derive(NetworkBehaviour)]
struct Behaviour {
    mdns: Mdns,
    floodsub: Floodsub,
    #[behaviour(ignore)]
    peers: Vec<PeerId>,
    #[behaviour(ignore)]
    msg_tx: mpsc::UnboundedSender<Vec<u8>>,
}

impl NetworkBehaviourEventProcess<MdnsEvent> for Behaviour {
    fn inject_event(&mut self, event: MdnsEvent) {
        if let MdnsEvent::Discovered(list) = event {
            for (peer, _) in list {
                if self.peers.iter().any(|p| *p == peer) {
                    continue;
                }
                self.peers.push(peer);
                self.floodsub.add_node_to_partial_view(peer);
            }
        }
    }
}

impl NetworkBehaviourEventProcess<FloodsubEvent> for Behaviour {
    fn inject_event(&mut self, message: FloodsubEvent) {
        if let FloodsubEvent::Message(message) = message {
            self.msg_tx
                .unbounded_send(message.data)
                .expect("should succeed");
        }
    }
}

struct Network {
    outgoing_tx: mpsc::UnboundedSender<Vec<u8>>,
    msg_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[async_trait::async_trait]
impl rush::Network<Hasher64, Data, Signature> for Network {
    type Error = ();
    fn send(&self, data: NetworkData, _node: NodeIndex) -> Result<(), Self::Error> {
        self.outgoing_tx
            .unbounded_send(data.encode())
            .map_err(|_| ())
    }
    fn broadcast(&self, data: NetworkData) -> Result<(), Self::Error> {
        self.outgoing_tx
            .unbounded_send(data.encode())
            .map_err(|_| ())
    }
    async fn next_event(&mut self) -> Option<NetworkData> {
        self.msg_rx.next().await.map(|msg| {
            NetworkData::decode(&mut &msg[..]).expect("honest network data should decode")
        })
    }
}

struct NetworkManager {
    swarm: Swarm<Behaviour>,
    outgoing_rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

impl Network {
    async fn new() -> Result<(Self, NetworkManager), Box<dyn Error>> {
        let local_key = identity::Keypair::generate_ed25519();
        let local_peer_id = PeerId::from(local_key.public());
        info!("Local peer id: {:?}", local_peer_id);

        let transport = development_transport(local_key).await?;

        let topic = floodsub::Topic::new(ALEPH_PROTOCOL_NAME);

        let (msg_tx, msg_rx) = mpsc::unbounded();
        let mut swarm = {
            let mdns_config = MdnsConfig {
                ttl: Duration::from_secs(6 * 60),
                query_interval: Duration::from_millis(100),
            };
            let mdns = Mdns::new(mdns_config).await?;
            let mut behaviour = Behaviour {
                floodsub: Floodsub::new(local_peer_id),
                mdns,
                peers: vec![],
                msg_tx: msg_tx.clone(),
            };
            behaviour.floodsub.subscribe(topic.clone());
            SwarmBuilder::new(transport, behaviour, local_peer_id)
                .executor(Box::new(|fut| {
                    tokio::spawn(fut);
                }))
                .build()
        };

        swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

        let (outgoing_tx, outgoing_rx) = mpsc::unbounded();
        let network = Network {
            outgoing_tx,
            msg_rx,
        };
        let network_manager = NetworkManager { swarm, outgoing_rx };

        Ok((network, network_manager))
    }
}

impl NetworkManager {
    async fn run(&mut self, exit: oneshot::Receiver<()>) {
        let mut exit = exit.into_stream();
        loop {
            tokio::select! {
                maybe_pm = self.outgoing_rx.next() => {
                    if let Some(message) = maybe_pm {
                        let floodsub = &mut self.swarm.behaviour_mut().floodsub;
                        let topic = floodsub::Topic::new(ALEPH_PROTOCOL_NAME);
                        floodsub.publish(topic, message);
                    }
                }
                event = self.swarm.next() => {
                    // called only to poll inner future
                    panic!("Unexpected event: {:?}", event);
                }
                _ = exit.next() => break,
            }
        }
    }
}
