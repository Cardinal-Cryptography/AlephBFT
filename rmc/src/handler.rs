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

    /// Signs hash and updates the internal state with it. Returns the signed
    /// version of the hash for broadcast. Should be called at most once for a particular hash.
    pub fn on_start_rmc(&mut self, hash: H) -> Signed<Indexed<H>, MK> {
        let signed_hash = Signed::sign_with_index(hash, &self.keychain);
        if !self.already_completed(signed_hash.as_signable().as_signable()) {
            self.handle_signed_hash(signed_hash.clone());
        }
        signed_hash
    }
    /// Update the internal state with the signed hash. If the hash is incorrectly signed then
    /// [`Error::BadSignature`] is returned. If Adding this signature completes a multisignature
    /// then `Ok(multisigned)` is returned. Otherwise `Ok(None)` is returned.
    pub fn on_signed_hash(
        &mut self,
        unchecked: UncheckedSigned<Indexed<H>, MK::Signature>,
    ) -> Result<Option<Multisigned<H, MK>>, Error> {
        let signed_hash = unchecked
            .check(&self.keychain)
            .map_err(|_| Error::BadSignature)?;
        Ok(
            match self.already_completed(signed_hash.as_signable().as_signable()) {
                true => None,
                false => self.handle_signed_hash(signed_hash),
            },
        )
    }

    fn handle_signed_hash(&mut self, signed: Signed<Indexed<H>, MK>) -> Option<Multisigned<H, MK>> {
        let hash = signed.as_signable().as_signable().clone();
        let new_state = match self.hash_states.remove(&hash) {
            None => signed.into_partially_multisigned(&self.keychain),
            Some(partial) => partial.add_signature(signed, &self.keychain),
        };
        match new_state {
            PartiallyMultisigned::Complete { multisigned } => {
                self.hash_states.insert(
                    hash,
                    PartiallyMultisigned::Complete {
                        multisigned: multisigned.clone(),
                    },
                );
                Some(multisigned)
            }
            incomplete => {
                self.hash_states.insert(hash, incomplete);
                None
            }
        }
    }

    /// Update the internal state with the finished multisigned hash. If the hash is incorrectly
    /// signed then [`Error::BadMultisignature`] is returned. Otherwise `multisigned` is returned,
    /// unless the multisignature got completed earlier.
    pub fn on_multisigned_hash(
        &mut self,
        unchecked: UncheckedSigned<H, MK::PartialMultisignature>,
    ) -> Result<Option<Multisigned<H, MK>>, Error> {
        if self.already_completed(unchecked.as_signable()) {
            return Ok(None);
        }

        let multisigned = unchecked
            .check_multi(&self.keychain)
            .map_err(|_| Error::BadMultisignature)?;
        self.hash_states.insert(
            multisigned.as_signable().clone(),
            PartiallyMultisigned::Complete {
                multisigned: multisigned.clone(),
            },
        );
        Ok(Some(multisigned))
    }

    fn already_completed(&self, hash: &H) -> bool {
        matches!(
            self.hash_states.get(hash),
            Some(PartiallyMultisigned::Complete { .. })
        )
    }
}
