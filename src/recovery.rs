use crate::{
    member::UnitMessage,
    units::{SignedUnit, UncheckedSignedUnit},
    Data, Hasher, KeyBox, NodeCount, Recipient,
};
use futures::{Sink, Stream};
use std::time::Duration;

// input:

// Given network sink and stream, recovers all units created by us in this instance of consensus.
pub(crate) async fn recover_our_units<
    H: Hasher,
    D: Data,
    KB: KeyBox,
    Si: Sink<(Recipient, UnitMessage<H, D, KB::Signature>)>,
    St: Stream<Item = Option<UncheckedSignedUnit<H, D, KB::Signature>>>,
>(
    _node_count: NodeCount,
    _messages_for_peers: St,
    _responses: Si,
    _multikeychain: &KB,
) -> Vec<SignedUnit<'_, H, D, KB>> {
    futures_timer::Delay::new(Duration::from_secs(60)).await;
    panic!();
}

// select take and timeout
