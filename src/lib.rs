//! Implements the Aleph BFT Consensus protocol as a "finality gadget". The [Member] struct
//! requires access to a network layer, a cryptographic primitive, and a data provider that
//! gives appropriate access to the set of available data that we need to make consensus on.
use async_trait::async_trait;
use codec::{Decode, Encode};
use futures::{channel::mpsc, Future};
use std::{fmt::Debug, hash::Hash as StdHash, pin::Pin};

use crate::nodes::NodeMap;

pub use config::{default_config, exponential_slowdown, Config, DelayConfig};
pub use member::run_session;
pub use network::{Network, NetworkData, Recipient};
pub use nodes::{NodeCount, NodeIndex};

mod alerts;
mod consensus;
mod creation;
mod extender;
mod member;
mod network;
mod nodes;
mod runway;
mod signed;
pub use signed::*;
mod config;
pub mod rmc;
mod terminal;
#[cfg(test)]
pub mod testing;
mod units;

/// The number of a session for which the consensus is run.
pub type SessionId = u64;

/// The source of data items that consensus should order.
///
/// AlephBFT internally calls [`DataProvider::get_data`] whenever a new unit is created and data needs to be placed inside.
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html for a discussion
/// and examples of how this trait can be implemented.
#[async_trait]
pub trait DataProvider<Data> {
    /// Outputs a new data item to be ordered
    async fn get_data(&mut self) -> Data;
}

/// The source of finalization of the units that consensus produces.
///
/// The [`FinalizationHandler::data_finalized`] method is called whenever a piece of data input to the algorithm
/// using [`DataProvider::get_data`] has been finalized, in order of finalization.
#[async_trait]
pub trait FinalizationHandler<Data> {
    /// Data, provided by [DataProvider::get_data], has been finalized.
    /// The calls to this function follow the order of finalization.
    async fn data_finalized(&mut self, data: Data);
}

/// Indicates that an implementor has been assigned some index.
pub trait Index {
    fn index(&self) -> NodeIndex;
}

/// A hasher, used for creating identifiers for blocks or units.
pub trait Hasher: Eq + Clone + Send + Sync + Debug + 'static {
    /// A hash, as an identifier for a block or unit.
    type Hash: AsRef<[u8]>
        + Eq
        + Ord
        + Copy
        + Clone
        + Send
        + Sync
        + Debug
        + StdHash
        + Encode
        + Decode;

    fn hash(s: &[u8]) -> Self::Hash;
}

/// Data type that we want to order.
pub trait Data: Eq + Clone + Send + Sync + Debug + StdHash + Encode + Decode + 'static {}

impl<T> Data for T where T: Eq + Clone + Send + Sync + Debug + StdHash + Encode + Decode + 'static {}

/// An asynchronous round of the protocol.
pub type Round = u16;

/// Type for sending a new ordered batch of data items.
pub type OrderedBatch<Data> = Vec<Data>;

/// A handle for waiting the task's completion.
pub type TaskHandle = Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;

/// An abstraction for an execution engine for Rust's asynchronous tasks.
pub trait SpawnHandle: Clone + Send + 'static {
    /// Run a new task.
    fn spawn(&self, name: &'static str, task: impl Future<Output = ()> + Send + 'static);
    /// Run a new task and returns a handle to it. If there is some error or panic during
    /// execution of the task, the handle should return an error.
    fn spawn_essential(
        &self,
        name: &'static str,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle;
}

pub(crate) type Receiver<T> = mpsc::UnboundedReceiver<T>;
pub(crate) type Sender<T> = mpsc::UnboundedSender<T>;
