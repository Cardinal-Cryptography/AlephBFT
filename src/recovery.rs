use crate::{
    member::UnitMessage,
    units::{UncheckedSignedUnit, Unit},
    Data, Hasher, NodeCount, Recipient, Signature,
};
use futures::{Sink, Stream};

// input:

// Given network sink and stream, recovers all units created by us in this instance of consensus.
pub(crate) async fn recover_our_units<
    H: Hasher,
    D: Data,
    S: Signature,
    Si: Sink<(Recipient, UnitMessage<H, D, S>)>,
    St: Stream<Item = Option<UncheckedSignedUnit<H, D, S>>>,
>(
    _node_count: NodeCount,
    _messages_for_peers: St,
    _responses: Si,
) -> Vec<Unit<H>> {
    Vec::new()
}

// select take and timeout
