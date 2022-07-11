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
