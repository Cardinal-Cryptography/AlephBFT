//! Implements the Aleph BFT Consensus protocol as a "finality gadget". The [Member] struct
//! requires access to a network layer, a cryptographic primitive, and a data provider that
//! gives appropriate access to the set of available data that we need to make consensus on.
use futures::{channel::mpsc, Future};
use std::pin::Pin;

// types
mod nodes {
    pub use aleph_bft_types::{NodeMap, NodeCount, NodeIndex, NodeSubset};
}
use aleph_bft_types::{NodeMap, NodeCount, NodeIndex,
    SessionId, DataProvider, FinalizationHandler, Index, Hasher, Data };


pub use config::{default_config, exponential_slowdown, Config, DelayConfig};
pub use member::run_session;

pub use network::{Network, NetworkData, Recipient};

mod alerts;
mod consensus;
mod creation;
mod extender;
mod member;
mod network;
mod runway;
mod signed;
pub use signed::*;
mod config;
pub mod rmc;
mod terminal;
#[cfg(test)]
pub mod testing;
mod units;


/// An asynchronous round of the protocol.
pub type Round = u16;

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

type Receiver<T> = mpsc::UnboundedReceiver<T>;
type Sender<T> = mpsc::UnboundedSender<T>;
