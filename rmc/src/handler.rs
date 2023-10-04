//! Reliable MultiCast - a primitive for Reliable Broadcast protocol.
pub use aleph_bft_crypto::{
    Indexed, MultiKeychain, Multisigned, NodeCount, PartialMultisignature, PartiallyMultisigned,
    Signable, Signature, Signed, UncheckedSigned,
};
use core::fmt::Debug;
use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    hash::Hash,
};

pub enum Error {
    BadSignature,
    BadMultisignature,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadSignature => write!(f, "received a hash with a bad signature."),
            Error::BadMultisignature => write!(f, "received a hash with a bad multisignature."),
        }
    }
}

pub struct Handler<H: Signable + Hash, MK: MultiKeychain> {
    keychain: MK,
    hash_states: HashMap<H, PartiallyMultisigned<H, MK>>,
}

impl<H: Signable + Hash + Eq + Clone + Debug, MK: MultiKeychain> Handler<H, MK> {
    pub fn new(keychain: MK) -> Self {
        Handler {
            hash_states: HashMap::new(),
            keychain,
        }
    }

    /// Signs hash and updates the internal state with it. Returns the unchecked signed
    /// version of the hash for broadcast. Should be called at most once for a particular hash.
    pub fn on_start_rmc(&mut self, hash: H) -> UncheckedSigned<Indexed<H>, MK::Signature> {
        let unchecked = Signed::sign_with_index(hash.clone(), &self.keychain).into_unchecked();
        let _ = self.on_signed_hash(&unchecked);
        unchecked
    }

    /// Update the internal state with the signed hash. If the hash is incorrectly signed then
    /// [`Error::BadSignature`] is returned. If Adding this signature completes a multisignature
    /// then `Ok(multisigned)` is returned. Otherwise `Ok(None)` is returned.
    pub fn on_signed_hash(
        &mut self,
        unchecked: &UncheckedSigned<Indexed<H>, MK::Signature>,
    ) -> Result<Option<Multisigned<H, MK>>, Error> {
        let hash = unchecked.as_signable_strip_index();
        let signed_hash = match unchecked.clone().check(&self.keychain) {
            Ok(signed_hash) => signed_hash,
            Err(_) => return Err(Error::BadSignature),
        };

        if self.already_completed(hash) {
            return Ok(None);
        }

        let new_state = match self.hash_states.remove(hash) {
            None => signed_hash.into_partially_multisigned(&self.keychain),
            Some(partial) => partial.add_signature(signed_hash, &self.keychain),
        };
        match new_state {
            PartiallyMultisigned::Complete { multisigned } => {
                self.hash_states.insert(
                    hash.clone(),
                    PartiallyMultisigned::Complete {
                        multisigned: multisigned.clone(),
                    },
                );
                Ok(Some(multisigned))
            }
            incomplete => {
                self.hash_states.insert(hash.clone(), incomplete);
                Ok(None)
            }
        }
    }

    /// Update the internal state with the finished multisigned hash. If the hash is incorrectly
    /// signed then [`Error::BadMultisignature`] is returned. Otherwise `multisigned` is returned,
    /// unless the multisignature got completed earlier.
    pub fn on_multisigned_hash(
        &mut self,
        unchecked: &UncheckedSigned<H, MK::PartialMultisignature>,
    ) -> Result<Option<Multisigned<H, MK>>, Error> {
        match unchecked.clone().check_multi(&self.keychain) {
            Ok(multisigned) => {
                let hash = multisigned.as_signable().clone();

                if self.already_completed(&hash) {
                    return Ok(None);
                }

                self.hash_states.insert(
                    hash,
                    PartiallyMultisigned::Complete {
                        multisigned: multisigned.clone(),
                    },
                );
                Ok(Some(multisigned))
            }
            Err(_) => Err(Error::BadMultisignature),
        }
    }

    fn already_completed(&self, hash: &H) -> bool {
        matches!(
            self.hash_states.get(hash),
            Some(PartiallyMultisigned::Complete { .. })
        )
    }
}
