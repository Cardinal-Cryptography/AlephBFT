use aleph_bft_crypto::NodeMap;
use async_trait::async_trait;

use crate::{Data, Hasher, NodeIndex, Round};

/// The source of data items that consensus should order.
///
/// AlephBFT internally calls [`DataProvider::get_data`] whenever a new unit is created and data
/// needs to be placed inside.
///
/// We refer to the documentation
/// https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html for a discussion and
/// examples of how this trait can be implemented.
#[async_trait]
pub trait DataProvider: Sync + Send + 'static {
    type Output: Data;
    /// Outputs a new data item to be ordered
    async fn get_data(&mut self) -> Option<Self::Output>;
}

/// The source of finalization of the units that consensus produces.
///
/// The [`FinalizationHandler::data_finalized`] method is called whenever a piece of data input
/// to the algorithm using [`DataProvider::get_data`] has been finalized, in order of finalization.
pub trait FinalizationHandler<D: Data>: Sync + Send + 'static {
    /// Data, provided by [DataProvider::get_data], has been finalized.
    /// The calls to this function follow the order of finalization.
    fn data_finalized(&mut self, data: D);
}

/// The source of finalization of the units that consensus produces.
///
/// The [`UnitFinalizationHandler::batch_finalized`] method is called whenever a batch of units
/// has been finalized, in order of finalization.
pub trait UnitFinalizationHandler<Data, H: Hasher>: Sync + Send + 'static {
    /// A unit, that contains data provided by [DataProvider::get_data], has been finalized.
    /// The calls to this function follow the order of finalization.
    fn batch_finalized(&mut self, batch: Vec<OrderedUnit<Data, H::Hash>>);
}

impl<D: Data, H: Hasher, FH: FinalizationHandler<D>> UnitFinalizationHandler<D, H> for FH {
    fn batch_finalized(&mut self, batch: Vec<OrderedUnit<D, H::Hash>>) {
        for unit in batch {
            if let Some(data) = unit.data {
                self.data_finalized(data)
            }
        }
    }
}

/// Represents state of the main internal data structure of AlephBFT (direct acyclic graph) used for
/// achieving distributed consensus.
///
/// Instances of this type are returned indirectly by [`member::run_session_for_units`] method using the
/// [`UnitFinalizationHandler`]. This way it allows to reconstruct the DAG's structure used by AlephBFT,
/// which can be then used for example for the purpose of node's performance evaluation.
pub struct OrderedUnit<Data, Hash> {
    pub data: Option<Data>,
    pub parents: NodeMap<Hash>,
    pub hash: Hash,
    pub creator: NodeIndex,
    pub round: Round,
}
