use crate::{Index, KeyBox};
use codec::{Decode, Encode};

pub trait Signable {
    fn bytes_to_sign(&self) -> Vec<u8>;
}

#[derive(Clone, Debug, Decode, Encode)]
pub struct UncheckedSigned<T: Signable + Index, S> {
    pub signable: T,
    pub signature: S,
}

#[derive(Debug)]
pub struct Signed<'a, T: Signable + Index, KB: KeyBox> {
    pub unchecked: UncheckedSigned<T, KB::Signature>,
    pub key_box: &'a KB,
}

impl<'a, T: Signable + Index + Clone, KB: KeyBox> Clone for Signed<'a, T, KB> {
    fn clone(&self) -> Self {
        Signed {
            unchecked: self.unchecked.clone(),
            key_box: self.key_box,
        }
    }
}

impl<'a, T: Signable + Index, KB: KeyBox> Signed<'a, T, KB> {
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

    pub fn from(unchecked: UncheckedSigned<T, KB::Signature>, key_box: &'a KB) -> Option<Self> {
        let signed = Signed { unchecked, key_box };
        if !signed.verify() {
            return None;
        }
        Some(signed)
    }

    pub(crate) fn verify(&self) -> bool {
        let signed = &self.unchecked.signable;
        let index = signed.index().unwrap();
        self.key_box
            .verify(&signed.bytes_to_sign(), &self.unchecked.signature, index)
    }

    pub(crate) fn signed(&self) -> &T {
        &self.unchecked.signable
    }
}

impl<'a, T: Signable + Index, KB: KeyBox + 'a> From<Signed<'a, T, KB>>
    for UncheckedSigned<T, KB::Signature>
{
    fn from(signed: Signed<'a, T, KB>) -> Self {
        signed.unchecked
    }
}
