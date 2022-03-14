use async_trait::async_trait;
use codec::{Decode, Encode};
use std::{fmt::Debug, hash::Hash as StdHash};

mod nodes;
pub use nodes::{
    NodeCount,
    NodeIndex,
    NodeMap,
    NodeSubset,
};

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

