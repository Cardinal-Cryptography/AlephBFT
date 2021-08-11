use crate::{
    member::UnitMessage, units::UncheckedSignedUnit, Data, Hasher, NodeIndex, Recipient, Signature,
};
use futures::{
    channel::mpsc::{UnboundedReceiver, UnboundedSender},
    future::ready,
    SinkExt, StreamExt,
};

const CATCH_UP_SECS: u64 = 5;

/// Given network sink and stream, recovers the newest unit created by us in this instance of consensus.
pub(crate) async fn recover_newest_unit<H: Hasher, D: Data, S: Signature>(
    responses_newest: UnboundedReceiver<Option<UncheckedSignedUnit<H, D, S>>>,
    mut messages_for_peers: UnboundedSender<(UnitMessage<H, D, S>, Recipient)>,
    our_index: NodeIndex,
) -> Option<UncheckedSignedUnit<H, D, S>> {
    messages_for_peers
        .send((UnitMessage::RequestNewest(our_index), Recipient::Everyone))
        .await
        .expect("send should succeed");
    // receive responses for CATCH_UP_SECS seconds, ignore the invalid ones, and
    // choose the one with highest round
    responses_newest
        .take_until(futures_timer::Delay::new(std::time::Duration::from_secs(
            CATCH_UP_SECS,
        )))
        .filter_map(|maybe_unchecked: Option<UncheckedSignedUnit<H, D, S>>| {
            ready(
                maybe_unchecked.filter(|unchecked| unchecked.as_signable().creator() == our_index),
            )
        })
        .fold(None, |best: Option<UncheckedSignedUnit<H, D, S>>, curr| {
            let new_best = best
                .filter(|unchecked| unchecked.as_signable().round() >= curr.as_signable().round())
                .or(Some(curr));
            ready(new_best)
        })
        .await
}
