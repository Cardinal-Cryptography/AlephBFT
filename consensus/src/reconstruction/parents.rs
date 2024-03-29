use crate::{
    reconstruction::ReconstructedUnit,
    runway::NotificationOut,
    units::{ControlHash, Unit, UnitCoord},
    Hasher, NodeIndex, NodeMap,
};
use std::collections::{hash_map::Entry, HashMap};

/// A unit in the process of reconstructing its parents.
#[derive(Debug, PartialEq, Eq, Clone)]
enum ReconstructingUnit<H: Hasher> {
    /// We are trying to optimistically reconstruct the unit from potential parents we get.
    Reconstructing(Unit<H>, NodeMap<H::Hash>),
    /// We are waiting for receiving an explicit list of unit parents.
    WaitingForParents(Unit<H>),
}

enum SingleParentReconstructionResult<H: Hasher> {
    Reconstructed(ReconstructedUnit<H>),
    InProgress(ReconstructingUnit<H>),
    RequestParents(ReconstructingUnit<H>),
}

impl<H: Hasher> ReconstructingUnit<H> {
    /// Produces a new reconstructing unit and a list of coordinates of parents we need for the reconstruction. Will panic if called for units of round 0.
    fn new(unit: Unit<H>) -> (Self, Vec<UnitCoord>) {
        let n_members = unit.control_hash().n_members();
        let round = unit.round();
        assert!(
            round != 0,
            "We should never try to reconstruct parents of a unit of round 0."
        );
        let coords = unit
            .control_hash()
            .parents()
            .map(|parent_id| UnitCoord::new(round - 1, parent_id))
            .collect();
        (
            ReconstructingUnit::Reconstructing(unit, NodeMap::with_size(n_members)),
            coords,
        )
    }

    fn reconstruct_parent(
        self,
        parent_id: NodeIndex,
        parent_hash: H::Hash,
    ) -> SingleParentReconstructionResult<H> {
        use ReconstructingUnit::*;
        use SingleParentReconstructionResult::*;
        match self {
            Reconstructing(unit, mut parents) => {
                parents.insert(parent_id, parent_hash);
                match parents.item_count() == unit.control_hash().parents().count() {
                    // We have enought parents, just need to check the control hash matches.
                    true => match ReconstructedUnit::with_parents(unit, parents) {
                        Ok(unit) => Reconstructed(unit),
                        // If the control hash doesn't match we want to get an explicit list of parents.
                        Err(unit) => RequestParents(WaitingForParents(unit)),
                    },
                    false => InProgress(Reconstructing(unit, parents)),
                }
            }
            // If we are already waiting for explicit parents, ignore any resolved ones; this shouldn't really happen.
            WaitingForParents(unit) => InProgress(WaitingForParents(unit)),
        }
    }

    fn control_hash(&self) -> &ControlHash<H> {
        self.as_unit().control_hash()
    }

    fn as_unit(&self) -> &Unit<H> {
        use ReconstructingUnit::*;
        match self {
            Reconstructing(unit, _) | WaitingForParents(unit) => unit,
        }
    }

    fn with_parents(self, parent_hashes: Vec<H::Hash>) -> Result<ReconstructedUnit<H>, Self> {
        let control_hash = self.control_hash();
        let mut parents = NodeMap::with_size(control_hash.n_members());
        for (parent_id, parent_hash) in control_hash.parents().zip(parent_hashes.into_iter()) {
            parents.insert(parent_id, parent_hash);
        }
        ReconstructedUnit::with_parents(self.as_unit().clone(), parents).map_err(|_| self)
    }
}

/// What we need to request to reconstruct units.
#[derive(Debug, PartialEq, Eq)]
pub enum Request<H: Hasher> {
    /// We need a unit at this coordinate.
    Coord(UnitCoord),
    /// We need the explicit list of parents for the unit identified by the hash.
    /// This should only happen in the presence of forks, when optimistic reconstruction failed.
    ParentsOf(H::Hash),
}

impl<H: Hasher> From<Request<H>> for NotificationOut<H> {
    fn from(request: Request<H>) -> Self {
        use NotificationOut::*;
        use Request::*;
        match request {
            // This is a tad weird, but should get better after the runway refactor.
            Coord(coord) => MissingUnits(vec![coord]),
            ParentsOf(unit) => WrongControlHash(unit),
        }
    }
}

/// The result of a reconstruction attempt. Might contain multiple reconstructed units,
/// as well as requests for some data that is needed for further reconstruction.
#[derive(Debug, PartialEq, Eq)]
pub struct ReconstructionResult<H: Hasher> {
    reconstructed_units: Vec<ReconstructedUnit<H>>,
    requests: Vec<Request<H>>,
}

impl<H: Hasher> ReconstructionResult<H> {
    fn new() -> Self {
        ReconstructionResult {
            reconstructed_units: Vec::new(),
            requests: Vec::new(),
        }
    }

    fn reconstructed(unit: ReconstructedUnit<H>) -> Self {
        ReconstructionResult {
            reconstructed_units: vec![unit],
            requests: Vec::new(),
        }
    }

    fn request(request: Request<H>) -> Self {
        ReconstructionResult {
            reconstructed_units: Vec::new(),
            requests: vec![request],
        }
    }

    fn add_unit(&mut self, unit: ReconstructedUnit<H>) {
        self.reconstructed_units.push(unit);
    }

    fn add_request(&mut self, request: Request<H>) {
        self.requests.push(request);
    }

    fn accumulate(&mut self, other: ReconstructionResult<H>) {
        let ReconstructionResult {
            mut reconstructed_units,
            mut requests,
        } = other;
        self.reconstructed_units.append(&mut reconstructed_units);
        self.requests.append(&mut requests);
    }
}

impl<H: Hasher> From<ReconstructionResult<H>> for (Vec<ReconstructedUnit<H>>, Vec<Request<H>>) {
    fn from(result: ReconstructionResult<H>) -> Self {
        let ReconstructionResult {
            reconstructed_units,
            requests,
        } = result;
        (reconstructed_units, requests)
    }
}

/// Receives units with control hashes and reconstructs their parents.
pub struct Reconstruction<H: Hasher> {
    reconstructing_units: HashMap<H::Hash, ReconstructingUnit<H>>,
    units_by_coord: HashMap<UnitCoord, H::Hash>,
    waiting_for_coord: HashMap<UnitCoord, Vec<H::Hash>>,
}

impl<H: Hasher> Reconstruction<H> {
    /// A new parent reconstruction widget.
    pub fn new() -> Self {
        Reconstruction {
            reconstructing_units: HashMap::new(),
            units_by_coord: HashMap::new(),
            waiting_for_coord: HashMap::new(),
        }
    }

    fn reconstruct_parent(
        &mut self,
        child_hash: H::Hash,
        parent_id: NodeIndex,
        parent_hash: H::Hash,
    ) -> ReconstructionResult<H> {
        use SingleParentReconstructionResult::*;
        match self.reconstructing_units.remove(&child_hash) {
            Some(child) => match child.reconstruct_parent(parent_id, parent_hash) {
                Reconstructed(unit) => ReconstructionResult::reconstructed(unit),
                InProgress(unit) => {
                    self.reconstructing_units.insert(child_hash, unit);
                    ReconstructionResult::new()
                }
                RequestParents(unit) => {
                    let hash = unit.as_unit().hash();
                    self.reconstructing_units.insert(child_hash, unit);
                    ReconstructionResult::request(Request::ParentsOf(hash))
                }
            },
            // We might have reconstructed the unit through explicit parents if someone sent them to us for no reason,
            // in which case we don't have it any more.
            None => ReconstructionResult::new(),
        }
    }

    /// Add a unit and start reconstructing its parents.
    pub fn add_unit(&mut self, unit: Unit<H>) -> ReconstructionResult<H> {
        let mut result = ReconstructionResult::new();
        let unit_hash = unit.hash();
        if self.reconstructing_units.contains_key(&unit_hash) {
            // We already received this unit once, no need to do anything.
            return result;
        }
        let unit_coord = UnitCoord::new(unit.round(), unit.creator());
        // We place the unit in the coord map only if this is the first variant ever received.
        // This is not crucial for correctness, but helps in clarity.
        if let Entry::Vacant(entry) = self.units_by_coord.entry(unit_coord) {
            entry.insert(unit_hash);
        }

        if let Some(children) = self.waiting_for_coord.remove(&unit_coord) {
            // We reconstruct the parent for each unit that waits for this coord.
            for child_hash in children {
                result.accumulate(self.reconstruct_parent(
                    child_hash,
                    unit_coord.creator(),
                    unit_hash,
                ));
            }
        }
        match unit_coord.round() {
            0 => {
                let unit = ReconstructedUnit::initial(unit);
                result.add_unit(unit);
            }
            _ => {
                let (unit, parent_coords) = ReconstructingUnit::new(unit);
                self.reconstructing_units.insert(unit_hash, unit);
                for parent_coord in parent_coords {
                    match self.units_by_coord.get(&parent_coord) {
                        Some(parent_hash) => result.accumulate(self.reconstruct_parent(
                            unit_hash,
                            parent_coord.creator(),
                            *parent_hash,
                        )),
                        None => {
                            self.waiting_for_coord
                                .entry(parent_coord)
                                .or_default()
                                .push(unit_hash);
                            result.add_request(Request::Coord(parent_coord));
                        }
                    }
                }
            }
        }
        result
    }

    /// Add an explicit list of a units' parents, perhaps reconstructing it.
    pub fn add_parents(
        &mut self,
        unit_hash: H::Hash,
        parents: Vec<H::Hash>,
    ) -> ReconstructionResult<H> {
        // If we don't have the unit, just ignore this response.
        match self.reconstructing_units.remove(&unit_hash) {
            Some(unit) => match unit.with_parents(parents) {
                Ok(unit) => ReconstructionResult::reconstructed(unit),
                Err(unit) => {
                    self.reconstructing_units.insert(unit_hash, unit);
                    ReconstructionResult::new()
                }
            },
            None => ReconstructionResult::new(),
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        reconstruction::{
            parents::{Reconstruction, Request},
            ReconstructedUnit,
        },
        units::{random_full_parent_units_up_to, UnitCoord},
        NodeCount, NodeIndex,
    };

    #[test]
    fn reconstructs_initial_units() {
        let mut reconstruction = Reconstruction::new();
        for unit in &random_full_parent_units_up_to(0, NodeCount(4), 43)[0] {
            let unit = unit.unit();
            let (mut reconstructed_units, requests) = reconstruction.add_unit(unit.clone()).into();
            assert!(requests.is_empty());
            assert_eq!(reconstructed_units.len(), 1);
            let reconstructed_unit = reconstructed_units.pop().expect("just checked its there");
            assert_eq!(reconstructed_unit, ReconstructedUnit::initial(unit));
            assert_eq!(reconstructed_unit.parents().item_count(), 0);
        }
    }

    #[test]
    fn reconstructs_units_coming_in_order() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(7, NodeCount(4), 43);
        for units in &dag {
            for unit in units {
                let unit = unit.unit();
                let round = unit.round();
                let (mut reconstructed_units, requests) =
                    reconstruction.add_unit(unit.clone()).into();
                assert!(requests.is_empty());
                assert_eq!(reconstructed_units.len(), 1);
                let reconstructed_unit = reconstructed_units.pop().expect("just checked its there");
                match round {
                    0 => {
                        assert_eq!(reconstructed_unit, ReconstructedUnit::initial(unit));
                        assert_eq!(reconstructed_unit.parents().item_count(), 0);
                    }
                    round => {
                        assert_eq!(reconstructed_unit.parents().item_count(), 4);
                        let parents = dag
                            .get((round - 1) as usize)
                            .expect("the parents are there");
                        for (parent, reconstructed_parent) in
                            parents.iter().zip(reconstructed_unit.parents().values())
                        {
                            assert_eq!(&parent.hash(), reconstructed_parent);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn requests_all_parents() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(1, NodeCount(4), 43);
        let unit = dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit")
            .unit();
        let (reconstructed_units, requests) = reconstruction.add_unit(unit.clone()).into();
        assert!(reconstructed_units.is_empty());
        assert_eq!(requests.len(), 4);
    }

    #[test]
    fn requests_single_parent() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(1, NodeCount(4), 43);
        for unit in dag.get(0).expect("just created").iter().skip(1) {
            let unit = unit.unit();
            reconstruction.add_unit(unit.clone());
        }
        let unit = dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit")
            .unit();
        let (reconstructed_units, requests) = reconstruction.add_unit(unit.clone()).into();
        assert!(reconstructed_units.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests.last().expect("just checked"),
            &Request::Coord(UnitCoord::new(0, NodeIndex(0)))
        );
    }

    #[test]
    fn reconstructs_units_coming_in_reverse_order() {
        let mut reconstruction = Reconstruction::new();
        let mut dag = random_full_parent_units_up_to(7, NodeCount(4), 43);
        dag.reverse();
        for unit in dag.get(0).expect("we have the top units") {
            let unit = unit.unit();
            let (reconstructed_units, requests) = reconstruction.add_unit(unit.clone()).into();
            assert!(reconstructed_units.is_empty());
            assert_eq!(requests.len(), 4);
        }
        let mut total_reconstructed = 0;
        for mut units in dag.into_iter().skip(1) {
            let last_unit = units.pop().expect("we have the unit");
            for unit in units {
                let unit = unit.unit();
                let (reconstructed_units, _) = reconstruction.add_unit(unit.clone()).into();
                total_reconstructed += reconstructed_units.len();
            }
            let unit = last_unit.unit();
            let (reconstructed_units, _) = reconstruction.add_unit(unit.clone()).into();
            total_reconstructed += reconstructed_units.len();
            assert!(reconstructed_units.len() >= 4);
        }
        assert_eq!(total_reconstructed, 4 * 8);
    }

    #[test]
    fn handles_bad_hash() {
        let mut reconstruction = Reconstruction::new();
        let dag = random_full_parent_units_up_to(0, NodeCount(4), 43);
        for unit in dag.get(0).expect("just created") {
            let unit = unit.unit();
            reconstruction.add_unit(unit.clone());
        }
        let other_dag = random_full_parent_units_up_to(1, NodeCount(4), 43);
        let unit = other_dag
            .get(1)
            .expect("just created")
            .last()
            .expect("we have a unit")
            .unit();
        let unit_hash = unit.hash();
        let (reconstructed_units, requests) = reconstruction.add_unit(unit.clone()).into();
        assert!(reconstructed_units.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests.last().expect("just checked"),
            &Request::ParentsOf(unit_hash),
        );
        let parent_hashes: Vec<_> = other_dag
            .get(0)
            .expect("other dag has initial units")
            .iter()
            .map(|unit| unit.hash())
            .collect();
        let (mut reconstructed_units, requests) = reconstruction
            .add_parents(unit_hash, parent_hashes.clone())
            .into();
        assert!(requests.is_empty());
        assert_eq!(reconstructed_units.len(), 1);
        let reconstructed_unit = reconstructed_units.pop().expect("just checked its there");
        assert_eq!(reconstructed_unit.parents().item_count(), 4);
        for (parent, reconstructed_parent) in parent_hashes
            .iter()
            .zip(reconstructed_unit.parents().values())
        {
            assert_eq!(parent, reconstructed_parent);
        }
    }
}
