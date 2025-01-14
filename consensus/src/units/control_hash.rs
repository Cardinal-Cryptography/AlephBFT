use crate::{units::UnitCoord, Hasher, NodeCount, NodeMap, Round};
use codec::{Decode, Encode};

/// Combined hashes of the parents of a unit together with the set of indices of creators of the
/// parents
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decode, Encode)]
pub struct ControlHash<H: Hasher> {
    pub(crate) parents: NodeMap<Round>,
    pub(crate) combined_hash: H::Hash,
}

impl<H: Hasher> ControlHash<H> {
    pub(crate) fn new(parents_with_rounds_and_hashes: &NodeMap<(H::Hash, Round)>) -> Self {
        let mut parents_with_rounds = NodeMap::with_size(parents_with_rounds_and_hashes.size());
        for (parent_index, (_, parent_round)) in parents_with_rounds_and_hashes.iter() {
            parents_with_rounds.insert(parent_index, *parent_round);
        }
        ControlHash {
            parents: parents_with_rounds,
            combined_hash: Self::create_control_hash(parents_with_rounds_and_hashes),
        }
    }

    /// Calculate parent control hash, which includes all parent hashes and their rounds into account.
    pub(crate) fn create_control_hash(parent_map: &NodeMap<(H::Hash, Round)>) -> H::Hash {
        parent_map.using_encoded(H::hash)
    }

    pub(crate) fn parents(&self) -> impl Iterator<Item = UnitCoord> + '_ {
        self.parents
            .iter()
            .map(|(node_index, &round)| UnitCoord::new(round, node_index))
    }

    pub(crate) fn n_parents(&self) -> NodeCount {
        NodeCount(self.parents().count())
    }

    pub(crate) fn n_members(&self) -> NodeCount {
        self.parents.size()
    }
}

#[cfg(test)]
pub mod tests {
    use crate::units::UnitCoord;
    use crate::{units::ControlHash, NodeCount, NodeIndex};
    use aleph_bft_mock::Hasher64;
    use codec::{Decode, Encode};

    #[test]
    fn given_control_hash_is_encoded_when_same_control_hash_is_decoded_then_results_are_the_same() {
        let ch =
            ControlHash::<Hasher64>::new(&vec![Some(([0; 8], 2)), None, Some(([1; 8], 2))].into());
        let encoded = ch.encode();
        let decoded =
            ControlHash::decode(&mut encoded.as_slice()).expect("should decode correctly");
        assert_eq!(decoded, ch);
    }

    #[test]
    fn given_control_hash_then_basic_properties_are_correct() {
        let parent_map = vec![
            Some(([0; 8], 2)),
            None,
            Some(([2; 8], 2)),
            Some(([3; 8], 2)),
            Some(([4; 8], 2)),
            Some(([5; 8], 2)),
            Some(([5; 8], 1)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        assert_eq!(
            ControlHash::<Hasher64>::create_control_hash(&parent_map),
            [119, 216, 234, 178, 169, 143, 117, 127]
        );

        assert_eq!(ch.n_parents(), NodeCount(6));
        assert_eq!(ch.n_members(), NodeCount(7));

        let parents: Vec<_> = ch.parents().collect();
        let expected_parents = vec![
            UnitCoord::new(2, NodeIndex(0)),
            UnitCoord::new(2, NodeIndex(2)),
            UnitCoord::new(2, NodeIndex(3)),
            UnitCoord::new(2, NodeIndex(4)),
            UnitCoord::new(2, NodeIndex(5)),
            UnitCoord::new(1, NodeIndex(6)),
        ];
        assert_eq!(parents, expected_parents);
    }
}
