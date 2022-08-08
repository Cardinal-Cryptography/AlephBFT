use crate::{
    Data, Hasher, Index, Keychain, NodeCount, NodeIndex, NodeMap, NodeSubset, Round, SessionId,
    Signable, Signed, UncheckedSigned,
};
use codec::{Decode, Encode};
use derivative::Derivative;
use parking_lot::RwLock;
use std::collections::HashMap;

mod store;
#[cfg(test)]
mod testing;
mod validator;
pub(crate) use store::*;
#[cfg(test)]
pub use testing::{create_units, creator_set, preunit_to_unchecked_signed_unit, preunit_to_unit};
pub use validator::{ValidationError, Validator};

/// The coordinates of a unit, i.e. creator and round. In the absence of forks this uniquely
/// determines a unit within a session.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Encode, Decode, Hash)]
pub struct UnitCoord {
    round: Round,
    creator: NodeIndex,
}

impl UnitCoord {
    pub fn new(round: Round, creator: NodeIndex) -> Self {
        Self {
            creator,
            round: round as u16,
        }
    }

    pub fn creator(&self) -> NodeIndex {
        self.creator
    }

    pub fn round(&self) -> Round {
        self.round
    }
}

/// Combined hashes of the parents of a unit together with the set of indices of creators of the
/// parents
#[derive(Clone, Debug, PartialEq, Eq, Hash, Encode, Decode)]
pub struct ControlHash<H: Hasher> {
    pub(crate) parents_mask: NodeSubset,
    pub(crate) combined_hash: H::Hash,
}

impl<H: Hasher> ControlHash<H> {
    pub(crate) fn new(parent_map: &NodeMap<H::Hash>) -> Self {
        ControlHash {
            parents_mask: parent_map.to_subset(),
            combined_hash: Self::combine_hashes(parent_map),
        }
    }

    pub(crate) fn combine_hashes(parent_map: &NodeMap<H::Hash>) -> H::Hash {
        parent_map.using_encoded(H::hash)
    }

    pub(crate) fn parents(&self) -> impl Iterator<Item = NodeIndex> + '_ {
        self.parents_mask.elements()
    }

    pub(crate) fn n_parents(&self) -> NodeCount {
        NodeCount(self.parents().count())
    }

    pub(crate) fn n_members(&self) -> NodeCount {
        NodeCount(self.parents_mask.size())
    }
}

/// The simplest type representing a unit, consisting of coordinates and a control hash
#[derive(Clone, Debug, PartialEq, Eq, Hash, Encode, Decode)]
pub struct PreUnit<H: Hasher> {
    coord: UnitCoord,
    control_hash: ControlHash<H>,
}

impl<H: Hasher> PreUnit<H> {
    pub(crate) fn new(creator: NodeIndex, round: Round, control_hash: ControlHash<H>) -> Self {
        PreUnit {
            coord: UnitCoord::new(round, creator),
            control_hash,
        }
    }

    pub(crate) fn n_parents(&self) -> NodeCount {
        self.control_hash.n_parents()
    }

    pub(crate) fn n_members(&self) -> NodeCount {
        self.control_hash.n_members()
    }

    pub(crate) fn creator(&self) -> NodeIndex {
        self.coord.creator()
    }

    pub(crate) fn round(&self) -> Round {
        self.coord.round()
    }

    pub(crate) fn control_hash(&self) -> &ControlHash<H> {
        &self.control_hash
    }
}

///
#[derive(Debug, Encode, Decode, Derivative)]
#[derivative(PartialEq, Eq, Hash)]
pub struct FullUnit<H: Hasher, D: Data> {
    pre_unit: PreUnit<H>,
    data: Option<D>,
    session_id: SessionId,
    #[codec(skip)]
    #[derivative(PartialEq = "ignore")]
    #[derivative(Hash = "ignore")]
    hash: RwLock<Option<H::Hash>>,
}

impl<H: Hasher, D: Data> Clone for FullUnit<H, D> {
    fn clone(&self) -> Self {
        let hash = self.hash.try_read().and_then(|guard| *guard);
        FullUnit {
            pre_unit: self.pre_unit.clone(),
            data: self.data.clone(),
            session_id: self.session_id,
            hash: RwLock::new(hash),
        }
    }
}

impl<H: Hasher, D: Data> FullUnit<H, D> {
    pub(crate) fn new(pre_unit: PreUnit<H>, data: Option<D>, session_id: SessionId) -> Self {
        FullUnit {
            pre_unit,
            data,
            session_id,
            hash: RwLock::new(None),
        }
    }
    pub(crate) fn as_pre_unit(&self) -> &PreUnit<H> {
        &self.pre_unit
    }
    pub(crate) fn creator(&self) -> NodeIndex {
        self.pre_unit.creator()
    }
    pub(crate) fn round(&self) -> Round {
        self.pre_unit.round()
    }
    pub(crate) fn control_hash(&self) -> &ControlHash<H> {
        self.pre_unit.control_hash()
    }
    pub(crate) fn coord(&self) -> UnitCoord {
        self.pre_unit.coord
    }
    pub(crate) fn data(&self) -> &Option<D> {
        &self.data
    }
    pub(crate) fn included_data(&self) -> Vec<D> {
        self.data.iter().cloned().collect()
    }
    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }
    pub(crate) fn hash(&self) -> H::Hash {
        let hash = *self.hash.read();
        match hash {
            Some(hash) => hash,
            None => {
                let hash = self.using_encoded(H::hash);
                *self.hash.write() = Some(hash);
                hash
            }
        }
    }
    pub(crate) fn unit(&self) -> Unit<H> {
        Unit::new(self.pre_unit.clone(), self.hash())
    }
}

impl<H: Hasher, D: Data> Signable for FullUnit<H, D> {
    type Hash = H::Hash;
    fn hash(&self) -> H::Hash {
        self.hash()
    }
}

impl<H: Hasher, D: Data> Index for FullUnit<H, D> {
    fn index(&self) -> NodeIndex {
        self.creator()
    }
}

pub(crate) type UncheckedSignedUnit<H, D, S> = UncheckedSigned<FullUnit<H, D>, S>;

pub(crate) type SignedUnit<H, D, K> = Signed<FullUnit<H, D>, K>;

#[derive(Clone, Debug, Eq, PartialEq, Encode, Decode)]
pub struct Unit<H: Hasher> {
    pre_unit: PreUnit<H>,
    hash: H::Hash,
}

impl<H: Hasher> Unit<H> {
    pub(crate) fn new(pre_unit: PreUnit<H>, hash: H::Hash) -> Self {
        Unit { pre_unit, hash }
    }
    pub(crate) fn creator(&self) -> NodeIndex {
        self.pre_unit.creator()
    }
    pub(crate) fn round(&self) -> Round {
        self.pre_unit.round()
    }
    pub(crate) fn control_hash(&self) -> &ControlHash<H> {
        self.pre_unit.control_hash()
    }
    pub(crate) fn hash(&self) -> H::Hash {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        units::{ControlHash, FullUnit as GenericFullUnit, PreUnit as GenericPreUnit},
        Hasher, NodeIndex,
    };
    use aleph_bft_mock::{Data, Hasher64};
    use codec::{Decode, Encode};

    type PreUnit = GenericPreUnit<Hasher64>;
    type FullUnit = GenericFullUnit<Hasher64, Data>;

    #[test]
    fn test_full_unit_hash_is_correct() {
        let ch = ControlHash::<Hasher64>::new(&vec![].into());
        let pre_unit = PreUnit::new(NodeIndex(5), 6, ch);
        let full_unit = FullUnit::new(pre_unit, Some(7), 8);
        let hash = full_unit.using_encoded(Hasher64::hash);
        assert_eq!(full_unit.hash(), hash);
        let ch = ControlHash::<Hasher64>::new(&vec![].into());
        let pre_unit = PreUnit::new(NodeIndex(5), 6, ch);
        let full_unit = FullUnit::new(pre_unit, None, 8);
        let hash = full_unit.using_encoded(Hasher64::hash);
        assert_eq!(full_unit.hash(), hash);
    }

    #[test]
    fn test_control_hash_codec() {
        let ch = ControlHash::<Hasher64>::new(&vec![Some([0; 8]), None, Some([1; 8])].into());
        let encoded = ch.encode();
        let decoded =
            ControlHash::decode(&mut encoded.as_slice()).expect("should decode correctly");
        assert_eq!(decoded, ch);
    }

    #[test]
    fn test_full_unit_codec() {
        let ch = ControlHash::<Hasher64>::new(&vec![].into());
        let pre_unit = PreUnit::new(NodeIndex(5), 6, ch);
        let full_unit = FullUnit::new(pre_unit, Some(7), 8);
        full_unit.hash();
        let encoded = full_unit.encode();
        let decoded = FullUnit::decode(&mut encoded.as_slice()).expect("should decode correctly");
        assert_eq!(decoded, full_unit);
        let ch = ControlHash::<Hasher64>::new(&vec![].into());
        let pre_unit = PreUnit::new(NodeIndex(5), 6, ch);
        let full_unit = FullUnit::new(pre_unit, None, 8);
        full_unit.hash();
        let encoded = full_unit.encode();
        let decoded = FullUnit::decode(&mut encoded.as_slice()).expect("should decode correctly");
        assert_eq!(decoded, full_unit);
    }
}
