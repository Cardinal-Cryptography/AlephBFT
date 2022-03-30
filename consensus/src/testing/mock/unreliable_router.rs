use crate::{NodeCount, NodeIndex};
use async_trait::async_trait;
use futures::{channel::mpsc::unbounded, Future, StreamExt};
use log::debug;
use std::{
    cell::RefCell,
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use crate::testing::mock::network::{Network, NetworkReceiver, NetworkSender};

pub struct Peer<D> {
    tx: NetworkSender<D>,
    rx: NetworkReceiver<D>,
}

pub struct UnreliableRouter<D> {
    peers: RefCell<HashMap<NodeIndex, Peer<D>>>,
    peer_list: Vec<NodeIndex>,
    hook_list: RefCell<Vec<Box<dyn NetworkHook<D>>>>,
    reliability: f64, //a number in the range [0, 1], 1.0 means perfect reliability, 0.0 means no message gets through
}

impl<D> UnreliableRouter<D> {
    pub(crate) fn new(peer_list: Vec<NodeIndex>, reliability: f64) -> Self {
        UnreliableRouter {
            peers: RefCell::new(HashMap::new()),
            peer_list,
            hook_list: RefCell::new(Vec::new()),
            reliability,
        }
    }

    pub fn add_hook<HK: NetworkHook<D> + 'static>(&mut self, hook: HK) {
        self.hook_list.borrow_mut().push(Box::new(hook));
    }

    pub(crate) fn connect_peer(&mut self, peer: NodeIndex) -> Network<D> {
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
        Network::new(rx_out_hub, tx_in_hub, self.peer_list.clone(), peer)
    }
}

impl<D> Future for UnreliableRouter<D> {
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

pub fn configure_network<D>(
    n_members: NodeCount,
    reliability: f64,
) -> (UnreliableRouter<D>, Vec<Network<D>>) {
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
pub trait NetworkHook<D>: Send {
    /// This must complete during a single poll - the current implementation
    /// of UnreliableRouter will panic if polling this method returns Poll::Pending.
    async fn update_state(&mut self, data: &mut D, sender: NodeIndex, recipient: NodeIndex);
}
