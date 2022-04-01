use aleph_bft_types::Signable;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignableByte(u8);

impl Signable for SignableByte {
    type Hash = [u8; 1];
    fn hash(&self) -> Self::Hash {
        [self.0]
    }
}

impl From<u8> for SignableByte {
    fn from(x: u8) -> Self {
        Self(x)
    }
}
