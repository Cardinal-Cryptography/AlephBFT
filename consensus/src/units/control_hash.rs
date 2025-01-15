use crate::{units::UnitCoord, Hasher, NodeCount, NodeIndex, NodeMap, Round};
use codec::{Decode, Encode};

#[derive(Debug, PartialEq)]
pub enum Error {
    NotDescendantOfPreviousUnit(NodeIndex),
    DescendantOfPreviousUnitButWrongRound(Round),
    NotEnoughParentsForRound(Round),
    ParentsHigherThanRound(Round),
}

/// Combined hashes of the parents of a unit together with the set of indices of creators of the
/// parents. By parent here we mean a parent hash and its round.
#[derive(Clone, Eq, PartialEq, Hash, Debug, Decode, Encode)]
pub struct ControlHash<H: Hasher> {
    pub(crate) parents: NodeMap<Round>,
    pub(crate) combined_hash: H::Hash,
}

impl<H: Hasher> ControlHash<H> {
    /// Creates new control hash from parents hashes and rounds
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

    /// Iterator over non-empty parents - returns [`UnitCoord`]s
    pub(crate) fn parents(&self) -> impl Iterator<Item = UnitCoord> + '_ {
        self.parents
            .iter()
            .map(|(node_index, &round)| UnitCoord::new(round, node_index))
    }

    /// Returns number of non-empty parents
    pub(crate) fn n_parents(&self) -> NodeCount {
        NodeCount(self.parents().count())
    }

    /// Returns number of all members in abft consensus
    pub(crate) fn n_members(&self) -> NodeCount {
        self.parents.size()
    }

    /// Validate
    pub fn validate(&self, unit_coord: UnitCoord) -> Result<(), Error> {
        assert!(unit_coord.round > 0, "Round must be greater than 0");

        self.unit_creator_is_descendant_of_previous_unit(unit_coord)?;
        self.previous_round_have_enough_parents(unit_coord)?;
        self.check_if_parents_greater_than_previous_round(unit_coord)?;

        Ok(())
    }

    fn check_if_parents_greater_than_previous_round(
        &self,
        unit_coord: UnitCoord,
    ) -> Result<(), Error> {
        let parents_greater_than_previous_round = self
            .parents()
            .filter(|&parent| parent.round > unit_coord.round - 1)
            .count();
        if parents_greater_than_previous_round > 0 {
            return Err(Error::ParentsHigherThanRound(unit_coord.round - 1));
        }
        Ok(())
    }

    fn previous_round_have_enough_parents(&self, unit_coord: UnitCoord) -> Result<(), Error> {
        let previous_round_parents = self
            .parents()
            .filter(|&parent| parent.round == unit_coord.round - 1)
            .count();
        if previous_round_parents < self.n_members().consensus_threshold().0 {
            return Err(Error::NotEnoughParentsForRound(unit_coord.round - 1));
        }
        Ok(())
    }

    fn unit_creator_is_descendant_of_previous_unit(
        &self,
        unit_coord: UnitCoord,
    ) -> Result<(), Error> {
        match self.parents.get(unit_coord.creator) {
            None => return Err(Error::NotDescendantOfPreviousUnit(unit_coord.creator)),
            Some(&parent_round) => {
                if unit_coord.round - 1 != parent_round {
                    return Err(Error::DescendantOfPreviousUnitButWrongRound(parent_round));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use crate::units::control_hash::Error;
    use crate::units::UnitCoord;
    use crate::{units::ControlHash, NodeCount, NodeIndex};
    use aleph_bft_mock::Hasher64;
    use aleph_bft_types::Round;
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

    #[test]
    fn given_control_hash_when_validate_with_correct_unit_coord_then_validate_is_ok() {
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
        assert!(ch.validate(UnitCoord::new(3, NodeIndex(2))).is_ok());
    }

    #[test]
    #[should_panic(expected = "Round must be greater than 0")]
    fn given_control_hash_when_validate_called_for_initial_unit_then_validate_panics() {
        let parent_map = vec![
            Some(([0; 8], 2)),
            None,
            Some(([2; 8], 2)),
            Some(([3; 8], 2)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        let _ = ch.validate(UnitCoord::new(0, NodeIndex(1)));
    }
    #[test]
    fn given_control_hash_when_creator_parent_does_not_exist_then_err_is_returned_from_validate() {
        let parent_map = vec![
            Some(([0; 8], 2)),
            None,
            Some(([2; 8], 2)),
            Some(([3; 8], 2)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        assert_eq!(
            ch.validate(UnitCoord::new(3, NodeIndex(1)))
                .expect_err("validate() should return error, returned Ok(()) instead"),
            Error::NotDescendantOfPreviousUnit(NodeIndex(1))
        );
    }

    #[test]
    fn given_control_hash_when_creator_parent_exists_but_has_wrong_round_then_err_is_returned_from_validate(
    ) {
        let parent_map = vec![
            Some(([0; 8], 2)),
            Some(([1; 8], 1)),
            Some(([2; 8], 2)),
            Some(([3; 8], 2)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        assert_eq!(
            ch.validate(UnitCoord::new(3, NodeIndex(1)))
                .expect_err("validate() should return error, returned Ok(()) instead"),
            Error::DescendantOfPreviousUnitButWrongRound(1)
        );
    }

    #[test]
    fn given_control_hash_when_there_are_not_enough_previous_round_parents_then_err_is_returned_from_validate(
    ) {
        let parent_map = vec![
            None,
            Some(([1; 8], 2)),
            Some(([2; 8], 2)),
            Some(([3; 8], 1)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        assert_eq!(
            ch.validate(UnitCoord::new(3, NodeIndex(1)))
                .expect_err("validate() should return error, returned Ok(()) instead"),
            Error::NotEnoughParentsForRound(2)
        );
    }

    #[test]
    fn given_control_hash_when_there_are_parents_from_greater_rounds_then_err_is_returned_from_validate(
    ) {
        let parent_map = vec![
            Some(([0; 8], 2)),
            Some(([1; 8], 2)),
            Some(([2; 8], 2)),
            Some(([3; 8], 3)),
        ]
        .into();
        let ch = ControlHash::<Hasher64>::new(&parent_map);
        assert_eq!(
            ch.validate(UnitCoord::new(3, NodeIndex(1)))
                .expect_err("validate() should return error, returned Ok(()) instead"),
            Error::ParentsHigherThanRound(2)
        );
    }

    #[test]
    fn given_correct_control_hash_when_only_round_change_then_control_hash_does_not_match() {
        let all_parents_from_round_three = vec![
            Some(([193, 179, 113, 82, 221, 179, 199, 217], 3)),
            Some(([215, 1, 244, 177, 19, 155, 43, 208], 3)),
            Some(([12, 108, 24, 87, 75, 135, 37, 3], 3)),
            Some(([3, 221, 173, 235, 29, 224, 247, 233], 3)),
        ];

        let parents_from_round_three = &all_parents_from_round_three[0..3].to_vec().into();
        let control_hash_of_fourth_round_unit =
            ControlHash::<Hasher64>::new(&parents_from_round_three);

        let mut parents_from_round_three_but_one_hash_replaced = parents_from_round_three.clone();
        parents_from_round_three_but_one_hash_replaced.insert(
            NodeIndex(2),
            ([234, 170, 183, 55, 61, 24, 31, 143], 3 as Round),
        );
        let borked_hash_of_fourth_round_unit =
            ControlHash::<Hasher64>::new(&parents_from_round_three_but_one_hash_replaced);
        assert_ne!(
            borked_hash_of_fourth_round_unit,
            control_hash_of_fourth_round_unit
        );

        let mut parents_from_round_three_but_one_unit_replaced = parents_from_round_three.clone();
        parents_from_round_three_but_one_unit_replaced
            .insert(NodeIndex(2), all_parents_from_round_three[3].unwrap());
        let control_hash_of_fourth_round_unit_but_one_unit_replaced =
            ControlHash::<Hasher64>::new(&parents_from_round_three_but_one_unit_replaced);
        assert_ne!(
            borked_hash_of_fourth_round_unit,
            control_hash_of_fourth_round_unit_but_one_unit_replaced
        );
        assert_ne!(
            control_hash_of_fourth_round_unit_but_one_unit_replaced,
            control_hash_of_fourth_round_unit
        );
    }
}
