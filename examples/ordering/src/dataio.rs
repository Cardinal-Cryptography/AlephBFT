use aleph_bft_types::{
    DataProvider as DataProviderT, FinalizationHandler as FinalizationHandlerT, NodeIndex,
    PerformancePenalties,
};
use async_trait::async_trait;
use codec::{Decode, Encode};
use futures::{channel::mpsc::unbounded, future::pending};
use log::{error, info};

type Receiver<T> = futures::channel::mpsc::UnboundedReceiver<T>;
type Sender<T> = futures::channel::mpsc::UnboundedSender<T>;

pub type Data = (NodeIndex, u32);

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default, Decode, Encode)]
pub struct DataProvider {
    id: NodeIndex,
    counter: u32,
    n_data: u32,
    stalled: bool,
}

impl DataProvider {
    pub fn new(id: NodeIndex, counter: u32, n_data: u32, stalled: bool) -> Self {
        Self {
            id,
            counter,
            n_data,
            stalled,
        }
    }
}

#[async_trait]
impl DataProviderT for DataProvider {
    type Output = Data;

    async fn get_data(&mut self) -> Option<Data> {
        if self.n_data == 0 {
            if self.stalled {
                info!("Awaiting DataProvider::get_data forever");
                pending::<()>().await;
            }
            info!("Providing None");
            None
        } else {
            let data = (self.id, self.counter);
            info!("Providing data: {}", self.counter);
            self.counter += 1;
            self.n_data -= 1;
            Some(data)
        }
    }
}

#[derive(Clone)]
pub struct FinalizationHandler {
    tx: Sender<Data>,
}

impl FinalizationHandlerT<Data> for FinalizationHandler {
    fn data_finalized(&mut self, data: Data) {
        if let Err(e) = self.tx.unbounded_send(data) {
            error!(target: "finalization-handler", "Error when sending data from FinalizationHandler {:?}.", e);
        }
    }

    fn performance_reported(&mut self, _penalties: PerformancePenalties) {}
}

impl FinalizationHandler {
    pub fn new() -> (Self, Receiver<Data>) {
        let (tx, rx) = unbounded();
        (Self { tx }, rx)
    }
}
