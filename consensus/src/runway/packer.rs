use crate::{
    units::{FullUnit, PreUnit, SignedUnit},
    Data, DataProvider, Hasher, MultiKeychain, NodeIndex, Receiver, Sender, SessionId, Signed,
};
use futures::{channel::oneshot, pin_mut, FutureExt, StreamExt};
use log::{debug, error, info};
use std::marker::PhantomData;

pub struct Packer<'a, H, D, DP, MK>
where
    H: Hasher,
    D: Data,
    DP: DataProvider<D>,
    MK: MultiKeychain,
{
    data_provider: DP,
    preunits_from_runway: Receiver<PreUnit<H>>,
    signed_units_for_runway: Sender<SignedUnit<'a, H, D, MK>>,
    keychain: &'a MK,
    session_id: SessionId,
    _phantom: PhantomData<D>,
}

impl<'a, H, D, DP, MK> Packer<'a, H, D, DP, MK>
where
    H: Hasher,
    D: Data,
    DP: DataProvider<D>,
    MK: MultiKeychain,
{
    pub fn new(
        data_provider: DP,
        preunits_from_runway: Receiver<PreUnit<H>>,
        signed_units_for_runway: Sender<SignedUnit<'a, H, D, MK>>,
        keychain: &'a MK,
        session_id: SessionId,
    ) -> Self {
        Self {
            data_provider,
            preunits_from_runway,
            signed_units_for_runway,
            keychain,
            session_id,
            _phantom: PhantomData,
        }
    }

    fn index(&self) -> NodeIndex {
        self.keychain.index()
    }

    async fn pack(&mut self) {
        loop {
            // the order is important: first wait for a PreUnit, then ask for fresh Data
            let preunit = match self.preunits_from_runway.next().await {
                Some(preunit) => preunit,
                None => {
                    error!(target: "AlephBFT-packer", "{:?} Runway PreUnit stream closed.", self.index());
                    break;
                }
            };
            debug!(target: "AlephBFT-packer", "{:?} Received PreUnit.", self.index());
            let data = self.data_provider.get_data().await;
            debug!(target: "AlephBFT-packer", "{:?} Received data.", self.index());
            let full_unit = FullUnit::new(preunit, data, self.session_id);
            let signed_unit = Signed::sign(full_unit, self.keychain).await;
            if self
                .signed_units_for_runway
                .unbounded_send(signed_unit)
                .is_err()
            {
                error!(target: "AlephBFT-packer", "{:?} Could not send SignedUnit to Runway.", self.index());
                break;
            }
        }
    }

    pub async fn run(&mut self, mut exit: oneshot::Receiver<()>) -> Result<(), ()> {
        info!(target: "AlephBFT-packer", "{:?} Packer started.", self.index());
        let pack = self.pack().fuse();
        pin_mut!(pack);
        futures::select! {
            _ = pack => Err(()),
            _ = exit => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Packer;
    use crate::{
        units::{ControlHash, PreUnit, SignedUnit},
        NodeCount, NodeIndex, Receiver, Sender, SessionId,
    };
    use aleph_bft_mock::{Data, DataProvider, Hasher64, Keychain};
    use aleph_bft_types::NodeMap;
    use futures::{
        channel::{mpsc, oneshot},
        pin_mut, FutureExt, StreamExt,
    };

    const SESSION_ID: SessionId = 43;
    const NODE_ID: NodeIndex = NodeIndex(0);
    const N_MEMBERS: NodeCount = NodeCount(4);

    fn prepare(
        keychain: &Keychain,
    ) -> (
        Sender<PreUnit<Hasher64>>,
        Receiver<SignedUnit<Hasher64, Data, Keychain>>,
        Packer<Hasher64, Data, DataProvider, Keychain>,
        oneshot::Sender<()>,
        oneshot::Receiver<()>,
        PreUnit<Hasher64>,
    ) {
        let data_provider = DataProvider::new();
        let (preunits_channel, preunits_from_runway) = mpsc::unbounded();
        let (signed_units_for_runway, signed_units_channel) = mpsc::unbounded();
        let packer = Packer::new(
            data_provider,
            preunits_from_runway,
            signed_units_for_runway,
            keychain,
            SESSION_ID,
        );
        let (exit_tx, exit_rx) = oneshot::channel();
        let parent_map = NodeMap::with_size(N_MEMBERS);
        let control_hash = ControlHash::new(&parent_map);
        let preunit = PreUnit::new(NODE_ID, 0, control_hash);
        (
            preunits_channel,
            signed_units_channel,
            packer,
            exit_tx,
            exit_rx,
            preunit,
        )
    }

    #[tokio::test]
    async fn unit_packed() {
        let keychain = Keychain::new(N_MEMBERS, NODE_ID);
        let (preunits_channel, signed_units_channel, mut packer, _exit_tx, exit_rx, preunit) =
            prepare(&keychain);
        let packer_handle = packer.run(exit_rx).fuse();
        preunits_channel.unbounded_send(preunit.clone()).unwrap();
        pin_mut!(packer_handle);
        pin_mut!(signed_units_channel);
        let unit = futures::select! {
            unit = signed_units_channel.next() => match unit {
                Some(unit) => unit,
                None => panic!(),
            },
            _ = packer_handle => panic!(),
        }
        .into_unchecked()
        .into_signable();
        assert_eq!(SESSION_ID, unit.session_id());
        assert_eq!(unit.as_pre_unit(), &preunit);
    }

    #[tokio::test]
    async fn closed() {
        let keychain = Keychain::new(N_MEMBERS, NODE_ID);
        let (preunits_channel, _signed_units_channel, mut packer, exit_tx, exit_rx, preunit) =
            prepare(&keychain);
        let packer_handle = packer.run(exit_rx);
        for _ in 0..3 {
            preunits_channel.unbounded_send(preunit.clone()).unwrap();
        }
        exit_tx.send(()).unwrap();
        for _ in 0..3 {
            preunits_channel.unbounded_send(preunit.clone()).unwrap();
        }
        packer_handle.await.unwrap();
    }

    #[tokio::test]
    async fn preunits_channel_closed() {
        let keychain = Keychain::new(N_MEMBERS, NODE_ID);
        let (_, _signed_units_channel, mut packer, _exit_tx, exit_rx, _) = prepare(&keychain);
        assert_eq!(packer.run(exit_rx).await, Err(()));
    }

    #[tokio::test]
    async fn signed_units_channel_closed() {
        let keychain = Keychain::new(N_MEMBERS, NODE_ID);
        let (preunits_channel, _, mut packer, _exit_tx, exit_rx, preunit) = prepare(&keychain);
        preunits_channel.unbounded_send(preunit).unwrap();
        assert_eq!(packer.run(exit_rx).await, Err(()));
    }
}
