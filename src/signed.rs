use crate::{nodes::NodeIndex, Index, KeyBox};
use codec::{Decode, Encode};

pub trait Signable {
    fn bytes_to_sign(&self) -> Vec<u8>;
}

#[derive(Clone, Debug, Decode, Encode)]
pub struct UncheckedSigned<T: Signable, S> {
    signable: T,
    signature: S,
}

#[derive(Clone, Debug)]
pub(crate) struct SignatureError<T: Signable, S> {
    unchecked: UncheckedSigned<T, S>,
}

impl<T: Signable, S> UncheckedSigned<T, S> {
    pub(crate) fn check_with_index<KB: KeyBox<Signature = S>>(
        self,
        key_box: &KB,
        index: NodeIndex,
    ) -> Result<Signed<T, KB>, SignatureError<T, KB::Signature>> {
        if !key_box.verify(&self.signable.bytes_to_sign(), &self.signature, index) {
            return Err(SignatureError { unchecked: self });
        }
        Ok(Signed {
            unchecked: self,
            key_box,
        })
    }
}

impl<T: Signable + Index, S> UncheckedSigned<T, S> {
    pub(crate) fn check<KB: KeyBox<Signature = S>>(
        self,
        key_box: &KB,
    ) -> Result<Signed<T, KB>, SignatureError<T, S>> {
        let index = self.signable.index();
        self.check_with_index(key_box, index)
    }
}

#[derive(Debug)]
pub struct Signed<'a, T: Signable, KB: KeyBox> {
    pub(crate) unchecked: UncheckedSigned<T, KB::Signature>,
    key_box: &'a KB,
}

impl<'a, T: Signable + Clone, KB: KeyBox> Clone for Signed<'a, T, KB> {
    fn clone(&self) -> Self {
        Signed {
            unchecked: self.unchecked.clone(),
            key_box: self.key_box,
        }
    }
}

impl<'a, T: Signable, KB: KeyBox> Signed<'a, T, KB> {
    pub fn sign(key_box: &'a KB, signable: T) -> Self {
        let signature = key_box.sign(&signable.bytes_to_sign());
        let signed = signable;
        Signed {
            unchecked: UncheckedSigned {
                signable: signed,
                signature,
            },
            key_box,
        }
    }

    pub(crate) fn verify(&self, index: NodeIndex) -> bool {
        let signed = &self.unchecked.signable;
        self.key_box
            .verify(&signed.bytes_to_sign(), &self.unchecked.signature, index)
    }

    pub(crate) fn signed(&self) -> &T {
        &self.unchecked.signable
    }
}

impl<'a, T: Signable, KB: KeyBox + 'a> From<Signed<'a, T, KB>>
    for UncheckedSigned<T, KB::Signature>
{
    fn from(signed: Signed<'a, T, KB>) -> Self {
        signed.unchecked
    }
}
