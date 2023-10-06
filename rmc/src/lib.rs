pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use codec::{Decode, Encode};
use core::fmt::Debug;
use futures::{
    channel::mpsc::{TrySendError, UnboundedReceiver, UnboundedSender},
    StreamExt,
};
use std::hash::Hash;

mod handler;
mod scheduler;
mod service;

pub use handler::Handler;
pub use scheduler::DoublingDelayScheduler;
pub use service::Service;

/// An RMC hash consisting of either a signed (indexed) hash, or a multisigned hash.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, Hash)]
pub enum RmcHash<H: Signable, S: Signature, M: PartialMultisignature> {
    SignedHash(UncheckedSigned<Indexed<H>, S>),
    MultisignedHash(UncheckedSigned<H, M>),
}

impl<H: Signable, S: Signature, M: PartialMultisignature> RmcHash<H, S, M> {
    pub fn hash(&self) -> &H {
        match self {
            RmcHash::SignedHash(unchecked) => unchecked.as_signable_strip_index(),
            RmcHash::MultisignedHash(unchecked) => unchecked.as_signable(),
        }
    }
    pub fn is_complete(&self) -> bool {
        matches!(self, RmcHash::MultisignedHash(_))
    }
}

/// An incoming RMC message consisting of either a request to start rmc or an outer RMC hash.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, Hash)]
pub enum RmcIncomingMessage<H: Signable, S: Signature, M: PartialMultisignature> {
    StartRmc(H),
    RmcHash(RmcHash<H, S, M>),
}

/// An outgoing RMC message consisting of either a newly created multisigned hash or an RMC hash for broadcast.
#[derive(Debug, Encode, Decode, Clone, PartialEq, Eq, Hash)]
pub enum RmcOutgoingMessage<H: Signable, MK: MultiKeychain> {
    NewMultisigned(Multisigned<H, MK>),
    RmcHash(RmcHash<H, MK::Signature, MK::PartialMultisignature>),
}

pub struct RmcIO<H: Signable, MK: MultiKeychain> {
    messages_to_rmc:
        UnboundedSender<RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>>,
    messages_from_rmc: UnboundedReceiver<RmcOutgoingMessage<H, MK>>,
}

type RmcIOSendError<H, S, M> = TrySendError<RmcIncomingMessage<H, S, M>>;

impl<H: Signable, MK: MultiKeychain> RmcIO<H, MK> {
    pub fn new(
        messages_to_rmc: UnboundedSender<
            RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
        >,
        messages_from_rmc: UnboundedReceiver<RmcOutgoingMessage<H, MK>>,
    ) -> Self {
        RmcIO {
            messages_to_rmc,
            messages_from_rmc,
        }
    }

    pub fn send(
        &self,
        message: RmcIncomingMessage<H, MK::Signature, MK::PartialMultisignature>,
    ) -> Result<(), RmcIOSendError<H, MK::Signature, MK::PartialMultisignature>> {
        self.messages_to_rmc.unbounded_send(message)
    }

    pub async fn receive(&mut self) -> Option<RmcOutgoingMessage<H, MK>> {
        self.messages_from_rmc.next().await
    }
}
