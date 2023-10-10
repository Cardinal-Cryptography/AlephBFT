pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use codec::{Decode, Encode};
use core::fmt::Debug;
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

