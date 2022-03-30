use crate::{Network as NetworkT, NodeIndex, Recipient};
use futures::{
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
    StreamExt,
};

pub type NetworkReceiver<D> = UnboundedReceiver<(D, NodeIndex)>;
pub type NetworkSender<D> = UnboundedSender<(D, NodeIndex)>;

pub struct Network<D> {
    rx: NetworkReceiver<D>,
    tx: NetworkSender<D>,
    peers: Vec<NodeIndex>,
    index: NodeIndex,
}

impl<D> Network<D> {
    pub fn new(
        rx: NetworkReceiver<D>,
        tx: NetworkSender<D>,
        peers: Vec<NodeIndex>,
        index: NodeIndex,
    ) -> Self {
        Network {
            rx,
            tx,
            peers,
            index,
        }
    }
    pub fn index(&self) -> NodeIndex {
        self.index
    }
}

#[async_trait::async_trait]
impl<D: Clone + Send> NetworkT<D> for Network<D> {
    fn send(&self, data: D, recipient: Recipient) {
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

    async fn next_event(&mut self) -> Option<D> {
        Some(self.rx.next().await?.0)
    }
}
