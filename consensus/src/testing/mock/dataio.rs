use crate::{
    DataProvider as DataProviderT, FinalizationHandler as FinalizationHandlerT, Receiver, Sender,
};
use async_trait::async_trait;
use futures::channel::mpsc::unbounded;
use log::error;

pub type Data = u32;

pub struct DataProvider;

impl DataProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl DataProviderT<Data> for DataProvider {
    async fn get_data(&mut self) -> Data {
        0
    }
}

pub struct FinalizationHandler {
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
