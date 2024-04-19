use crate::{dag::DagUnit, units::Unit, Data, Hasher, MultiKeychain};

mod election;
mod extender;
mod units;

use aleph_bft_types::{FinalizationHandler, OrderedUnit};
use extender::Extender;

/// A struct responsible for executing the Consensus protocol on a local copy of the Dag.
/// It receives units which are guaranteed to eventually appear in the Dags
/// of all honest nodes. The static Aleph Consensus algorithm is then run on this Dag in order
/// to finalize subsequent rounds of the Dag. More specifically whenever a new unit is received
/// this process checks whether a new round can be finalized and if so, it computes the batch of
/// units that should be finalized, and uses the finalization handler to report that to the user.
///
/// We refer to the documentation https://cardinal-cryptography.github.io/AlephBFT/internals.html
/// Section 5.4 for a discussion of this component.
pub struct Ordering<
    H: Hasher,
    D: Data,
    MK: MultiKeychain,
    FH: FinalizationHandler<Vec<OrderedUnit<D, H>>>,
> {
    extender: Extender<DagUnit<H, D, MK>>,
    finalization_handler: FH,
}

impl<H: Hasher, D: Data, MK: MultiKeychain, FH: FinalizationHandler<Vec<OrderedUnit<D, H>>>>
    Ordering<H, D, MK, FH>
{
    pub fn new(finalization_handler: FH) -> Self {
        let extender = Extender::new();
        Ordering {
            extender,
            finalization_handler,
        }
    }

    fn handle_batch(&mut self, batch: Vec<DagUnit<H, D, MK>>) {
        let batch = batch
            .into_iter()
            .map(|unit| {
                let (unit, parents) = unit.into();
                let hash = unit.hash();
                let (pre_unit, data) = unit.into_signable().into();
                OrderedUnit {
                    data,
                    parents,
                    hash,
                    creator: pre_unit.creator(),
                    round: pre_unit.round(),
                }
            })
            .collect();
        self.finalization_handler.data_finalized(batch);
    }

    pub fn add_unit(&mut self, unit: DagUnit<H, D, MK>) {
        for batch in self.extender.add_unit(unit) {
            self.handle_batch(batch);
        }
    }
}
