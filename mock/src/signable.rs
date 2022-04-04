use aleph_bft_types::Signable as SignableT;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signable(String);

impl SignableT for Signable {
    type Hash = Vec<u8>;
    fn hash(&self) -> Self::Hash {
        self.0.clone().into()
    }
}

impl<T: Into<String>> From<T> for Signable {
    fn from(x: T) -> Self {
        Self(x.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SignableByte(u8);

impl SignableT for SignableByte {
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
