//! Implements the Aleph BFT Consensus protocol as a "finality gadget". The [Member] struct
//! requires access to a network layer, a cryptographic primitive, and a data provider that
//! gives appropriate access to the set of available data that we need to make consensus on.

mod alerts;
mod consensus;
mod creation;
mod extender;
mod member;
mod network;
mod runway;
mod signed;
mod config;
mod terminal;
mod units;

use aleph_bft_types::{
    DataProvider, FinalizationHandler, Hasher, Index,
    Network, NodeCount, NodeIndex, NodeMap, NodeSubset,
    Round, SessionId, SpawnHandle,
};
#[cfg(test)]
use aleph_bft_types::{Recipient, TaskHandle,};

use futures::channel::mpsc;
type Receiver<T> = mpsc::UnboundedReceiver<T>;
type Sender<T> = mpsc::UnboundedSender<T>;


pub use config::{default_config, exponential_slowdown, Config, DelayConfig};
pub use network::{Data, NetworkData};
pub use member::run_session;
//
pub use signed::*;
#[cfg(test)]
pub mod testing;
pub mod rmc;
