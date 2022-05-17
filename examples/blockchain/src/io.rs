use crate::{blockchain::Transaction, consensus::ConsensusData};
use aleph_bft::Recipient;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};

pub struct ConsensusNetworkIO {
    pub rx_data: UnboundedReceiver<ConsensusData>,
    pub tx_data: UnboundedSender<(ConsensusData, Recipient)>,
    pub rx_transactions: UnboundedReceiver<Transaction>,
}

pub struct NetworkConsensusIO {
    pub tx_data: UnboundedSender<ConsensusData>,
    pub rx_data: UnboundedReceiver<(ConsensusData, Recipient)>,
    pub tx_transactions: UnboundedSender<Transaction>,
}

pub fn consensus_network_io() -> (ConsensusNetworkIO, NetworkConsensusIO) {
    let (tx_data_network, rx_data_consensus) = unbounded();
    let (tx_data_consensus, rx_data_network) = unbounded();
    let (tx_transactions, rx_transactions) = unbounded();
    (
        ConsensusNetworkIO {
            rx_data: rx_data_consensus,
            tx_data: tx_data_consensus,
            rx_transactions,
        },
        NetworkConsensusIO {
            tx_data: tx_data_network,
            rx_data: rx_data_network,
            tx_transactions,
        },
    )
}
