use crate::{
    units::{ControlHash, PreUnit, Unit},
    Hasher, NodeCount, NodeIndex, NodeMap, Round,
};
use log::trace;

#[derive(Clone)]
struct UnitsCollector<H: Hasher> {
    n_candidates: NodeCount,
    candidates: NodeMap<H::Hash>,
}

impl<H: Hasher> UnitsCollector<H> {
    pub fn new(n_members: NodeCount) -> Self {
        Self {
            n_candidates: NodeCount(0),
            candidates: NodeMap::with_size(n_members),
        }
    }

    pub fn add_unit(&mut self, unit: &Unit<H>) {
        let pid = unit.creator();
        let hash = unit.hash();

        if self.candidates.get(pid).is_none() {
            self.candidates.insert(pid, hash);
            self.n_candidates += NodeCount(1);
            trace!(target: "AlephBFT-creator", "Added a new unit: {:#?}.", unit);
        }
    }

    pub fn can_create(&self, node_id: NodeIndex) -> Option<&NodeMap<H::Hash>> {
        let threshold = (self.candidates.size() * 2) / 3 + NodeCount(1);

        if self.n_candidates < threshold {
            trace!(target: "AlephBFT-creator", "Unable to create a unit: not enough parents available.");
            return None;
        }
        if self.candidates.get(node_id).is_none() {
            trace!(target: "AlephBFT-creator", "Unable to create a unit: missing our own unit in parents.");
            return None;
        }
        Some(&self.candidates)
    }
}

fn create_unit<H: Hasher>(
    node_id: NodeIndex,
    parents: NodeMap<H::Hash>,
    round: Round,
) -> (PreUnit<H>, Vec<H::Hash>) {
    let control_hash = ControlHash::new(&parents);
    let parent_hashes = parents.into_values().collect();

    let new_preunit = PreUnit::new(node_id, round, control_hash);
    trace!(target: "AlephBFT-creator", "Created a new unit {:?} at round {:?}.", new_preunit, round);
    (new_preunit, parent_hashes)
}

pub struct Creator<H: Hasher> {
    node_id: NodeIndex,
    n_members: NodeCount,
    round_collectors: Vec<UnitsCollector<H>>,
}

impl<H: Hasher> Creator<H> {
    pub fn new(node_id: NodeIndex, n_members: NodeCount) -> Self {
        Creator {
            node_id,
            n_members,
            round_collectors: vec![UnitsCollector::new(n_members)],
        }
    }

    pub fn current_round(&self) -> Round {
        (self.round_collectors.len() - 1) as Round
    }

    // gets or initializes unit collectors for a given round (and all between if not there)
    fn get_collector_for_round(&mut self, round: Round) -> &mut UnitsCollector<H> {
        let round = usize::from(round);
        if round >= self.round_collectors.len() {
            let new_size = (round + 1).into();
            self.round_collectors
                .resize(new_size, UnitsCollector::new(self.n_members));
        };
        &mut self.round_collectors[round]
    }

    /// Returns `None` if a unit cannot be created.
    /// To create a new unit, we need to have at least floor(2*N/3) + 1 parents available in previous round.
    /// Additionally, our unit from previous round must be available.
    pub fn create_unit(&self, round: Round) -> Option<(PreUnit<H>, Vec<H::Hash>)> {
        if round == 0 {
            let parents = NodeMap::with_size(self.n_members);
            return create_unit(self.node_id, parents, round).into();
        }
        let prev_round = usize::from(round - 1);

        let parents = self
            .round_collectors
            .get(prev_round)?
            .can_create(self.node_id)?;

        create_unit(self.node_id, parents.clone(), round).into()
    }

    pub fn add_unit(&mut self, unit: &Unit<H>) {
        self.get_collector_for_round(unit.round()).add_unit(unit);
    }
}

#[cfg(test)]
mod tests {
    use super::Creator as GenericCreator;
    use crate::{
        units::{create_units, creator_set, preunit_to_unit},
        NodeCount, NodeIndex,
    };
    use aleph_bft_mock::Hasher64;
    use std::collections::HashSet;

    type Creator = GenericCreator<Hasher64>;

    #[test]
    fn creates_initial_unit() {
        let n_members = NodeCount(7);
        let round = 0;
        let creator = Creator::new(NodeIndex(0), n_members);
        assert_eq!(creator.current_round(), round);
        let (preunit, parent_hashes) = creator
            .create_unit(round)
            .expect("Creation should succeed.");
        assert_eq!(preunit.round(), round);
        assert_eq!(parent_hashes.len(), 0);
    }

    #[test]
    fn creates_unit_with_all_parents() {
        let n_members = NodeCount(7);
        let mut creators = creator_set(n_members);
        let new_units = create_units(creators.iter(), 0);
        let new_units: Vec<_> = new_units
            .into_iter()
            .map(|(pu, _)| preunit_to_unit(pu, 0))
            .collect();
        let expected_hashes: Vec<_> = new_units.iter().map(|u| u.hash()).collect();
        let creator = &mut creators[0];
        creator.add_units(&new_units);
        let round = 1;
        assert_eq!(creator.current_round(), 0);
        let (preunit, parent_hashes) = creator
            .create_unit(round)
            .expect("Creation should succeed.");
        assert_eq!(preunit.round(), round);
        assert_eq!(parent_hashes, expected_hashes);
    }

    fn create_unit_with_minimal_parents(n_members: NodeCount) {
        let n_parents = (n_members.0 * 2) / 3 + 1;
        let mut creators = creator_set(n_members);
        let new_units = create_units(creators.iter().take(n_parents), 0);
        let new_units: Vec<_> = new_units
            .into_iter()
            .map(|(pu, _)| preunit_to_unit(pu, 0))
            .collect();
        let expected_hashes: Vec<_> = new_units.iter().map(|u| u.hash()).collect();
        let creator = &mut creators[0];
        creator.add_units(&new_units);
        let round = 1;
        assert_eq!(creator.current_round(), 0);
        let (preunit, parent_hashes) = creator
            .create_unit(round)
            .expect("Creation should succeed.");
        assert_eq!(preunit.round(), round);
        assert_eq!(parent_hashes, expected_hashes);
    }

    #[test]
    fn creates_unit_with_minimal_parents_4() {
        create_unit_with_minimal_parents(NodeCount(4));
    }

    #[test]
    fn creates_unit_with_minimal_parents_5() {
        create_unit_with_minimal_parents(NodeCount(5));
    }

    #[test]
    fn creates_unit_with_minimal_parents_6() {
        create_unit_with_minimal_parents(NodeCount(6));
    }

    #[test]
    fn creates_unit_with_minimal_parents_7() {
        create_unit_with_minimal_parents(NodeCount(7));
    }

    fn dont_create_unit_below_parents_threshold(n_members: NodeCount) {
        let n_parents = (n_members.0 * 2) / 3;
        let mut creators = creator_set(n_members);
        let new_units = create_units(creators.iter().take(n_parents), 0);
        let new_units: Vec<_> = new_units
            .into_iter()
            .map(|(pu, _)| preunit_to_unit(pu, 0))
            .collect();
        let creator = &mut creators[0];
        creator.add_units(&new_units);
        let round = 1;
        assert_eq!(creator.current_round(), 0);
        assert!(creator.create_unit(round).is_none())
    }

    #[test]
    fn cannot_create_unit_below_parents_threshold_4() {
        dont_create_unit_below_parents_threshold(NodeCount(4));
    }

    #[test]
    fn cannot_create_unit_below_parents_threshold_5() {
        dont_create_unit_below_parents_threshold(NodeCount(5));
    }

    #[test]
    fn cannot_create_unit_below_parents_threshold_6() {
        dont_create_unit_below_parents_threshold(NodeCount(6));
    }

    #[test]
    fn cannot_create_unit_below_parents_threshold_7() {
        dont_create_unit_below_parents_threshold(NodeCount(7));
    }

    #[test]
    fn creates_two_units_when_possible() {
        let n_members = NodeCount(7);
        let mut creators = creator_set(n_members);
        let mut expected_hashes_per_round = Vec::new();
        for round in 0..2 {
            let new_units = create_units(creators.iter().skip(1), round);
            let new_units: Vec<_> = new_units
                .into_iter()
                .map(|(pu, _)| preunit_to_unit(pu, 0))
                .collect();
            let expected_hashes: HashSet<_> = new_units.iter().map(|u| u.hash()).collect();
            for creator in creators.iter_mut() {
                creator.add_units(&new_units);
            }
            expected_hashes_per_round.push(expected_hashes);
        }
        let creator = &mut creators[0];
        assert_eq!(creator.current_round(), 1);
        for round in 0..3 {
            let (preunit, parent_hashes) = creator
                .create_unit(round)
                .expect("Creation should succeed.");
            assert_eq!(preunit.round(), round);
            let parent_hashes: HashSet<_> = parent_hashes.into_iter().collect();
            if round != 0 {
                assert_eq!(
                    parent_hashes,
                    expected_hashes_per_round[(round - 1) as usize]
                );
            }
            let unit = preunit_to_unit(preunit, 0);
            creator.add_unit(&unit);
            if round < 2 {
                expected_hashes_per_round[round as usize].insert(unit.hash());
            }
        }
    }

    #[test]
    fn cannot_create_unit_without_predecessor() {
        let n_members = NodeCount(7);
        let mut creators = creator_set(n_members);
        let new_units = create_units(creators.iter().skip(1), 0);
        let new_units: Vec<_> = new_units
            .into_iter()
            .map(|(pu, _)| preunit_to_unit(pu, 0))
            .collect();
        let creator = &mut creators[0];
        creator.add_units(&new_units);
        let round = 1;
        assert!(creator.create_unit(round).is_none());
    }
}
