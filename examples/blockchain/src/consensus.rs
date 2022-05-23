use crate::{blockchain::Transaction, io::ConsensusNetworkIO};
use aleph_bft::{
    run_session, DataProvider as DataProviderT, FinalizationHandler as FinalizationHandlerT,
    Network as NetworkT, Recipient,
};
use aleph_bft_mock::{
    Hasher64, Keychain, Loader, PartialMultisignature, Saver, Signature, Spawner,
};
use futures::{
    channel::{
        mpsc::{UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    StreamExt,
};
use parking_lot::Mutex;
use std::sync::Arc;

pub type ConsensusData =
    aleph_bft::NetworkData<Hasher64, Transaction, Signature, PartialMultisignature>;

struct DataProvider {
    rx_transactions: UnboundedReceiver<Transaction>,
}

#[async_trait::async_trait]
impl DataProviderT<Transaction> for DataProvider {
    async fn get_data(&mut self) -> Transaction {
        self.rx_transactions.next().await.unwrap()
    }
}

struct FinalizationHandler {
    tx_transactions: UnboundedSender<Transaction>,
}

#[async_trait::async_trait]
impl FinalizationHandlerT<Transaction> for FinalizationHandler {
    async fn data_finalized(&mut self, data: Transaction) {
        self.tx_transactions.unbounded_send(data).unwrap()
    }
}

struct ConsensusNetwork {
    rx_data: UnboundedReceiver<ConsensusData>,
    tx_data: UnboundedSender<(ConsensusData, Recipient)>,
}

#[async_trait::async_trait]
impl NetworkT<ConsensusData> for ConsensusNetwork {
    fn send(&self, data: ConsensusData, recipient: Recipient) {
        self.tx_data.unbounded_send((data, recipient)).unwrap();
    }

    async fn next_event(&mut self) -> Option<ConsensusData> {
        self.rx_data.next().await
    }
}

pub async fn run(
    id: usize,
    n_members: usize,
    network_io: ConsensusNetworkIO,
    blockchain_io: UnboundedSender<Transaction>,
    loader_vec: Vec<u8>,
    saver_vec: Arc<Mutex<Vec<u8>>>,
    exit: oneshot::Receiver<()>,
) {
    let network = ConsensusNetwork {
        rx_data: network_io.rx_data,
        tx_data: network_io.tx_data,
    };
    let data_provider = DataProvider {
        rx_transactions: network_io.rx_transactions,
    };
    let finalization_handler = FinalizationHandler {
        tx_transactions: blockchain_io,
    };
    let backup_loader = Loader::new(loader_vec);
    let backup_saver = Saver::new(saver_vec);
    let local_io = aleph_bft::LocalIO::new(
        data_provider,
        finalization_handler,
        backup_saver,
        backup_loader,
    );
    let keychain = Keychain::new(n_members.into(), id.into());
    let config = aleph_bft::default_config(n_members.into(), id.into(), 0);
    run_session(config, local_io, network, keychain, Spawner {}, exit).await
}
