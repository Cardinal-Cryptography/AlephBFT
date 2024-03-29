use crate::{
    creation::Creator as GenericCreator,
    units::{
        ControlHash as GenericControlHash, FullUnit as GenericFullUnit, PreUnit as GenericPreUnit,
        UncheckedSignedUnit as GenericUncheckedSignedUnit, Unit as GenericUnit,
    },
    Hasher, NodeCount, NodeIndex, NodeMap, Round, SessionId, Signed,
};
use aleph_bft_mock::{Data, Hasher64, Keychain, Signature};

type ControlHash = GenericControlHash<Hasher64>;
type Creator = GenericCreator<Hasher64>;
type PreUnit = GenericPreUnit<Hasher64>;
type Unit = GenericUnit<Hasher64>;
type FullUnit = GenericFullUnit<Hasher64, Data>;
type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;

pub fn creator_set(n_members: NodeCount) -> Vec<Creator> {
    (0..n_members.0)
        .map(|i| Creator::new(NodeIndex(i), n_members))
        .collect()
}

pub fn create_preunits<'a, C: Iterator<Item = &'a Creator>>(
    creators: C,
    round: Round,
) -> Vec<(PreUnit, Vec<<Hasher64 as Hasher>::Hash>)> {
    creators
        .map(|c| c.create_unit(round).expect("Creation should succeed."))
        .collect()
}

fn preunit_to_full_unit(preunit: PreUnit, session_id: SessionId) -> FullUnit {
    FullUnit::new(preunit, rand::random(), session_id)
}

pub fn preunit_to_unit(preunit: PreUnit, session_id: SessionId) -> Unit {
    preunit_to_full_unit(preunit, session_id).unit()
}

impl Creator {
    pub fn add_units(&mut self, units: &[Unit]) {
        for unit in units {
            self.add_unit(unit);
        }
    }
}

pub fn full_unit_to_unchecked_signed_unit(
    full_unit: FullUnit,
    keychain: &Keychain,
) -> UncheckedSignedUnit {
    Signed::sign(full_unit, keychain).into()
}

pub fn preunit_to_unchecked_signed_unit(
    pu: PreUnit,
    session_id: SessionId,
    keychain: &Keychain,
) -> UncheckedSignedUnit {
    full_unit_to_unchecked_signed_unit(preunit_to_full_unit(pu, session_id), keychain)
}

fn initial_preunit(n_members: NodeCount, node_id: NodeIndex) -> PreUnit {
    PreUnit::new(
        node_id,
        0,
        ControlHash::new(&vec![None; n_members.0].into()),
    )
}

fn random_initial_units(n_members: NodeCount, session_id: SessionId) -> Vec<FullUnit> {
    n_members
        .into_iterator()
        .map(|node_id| initial_preunit(n_members, node_id))
        .map(|preunit| preunit_to_full_unit(preunit, session_id))
        .collect()
}

pub fn random_unit_with_parents(creator: NodeIndex, parents: &Vec<FullUnit>) -> FullUnit {
    let representative_parent = parents.last().expect("there are parents");
    let n_members = representative_parent.as_pre_unit().n_members();
    let session_id = representative_parent.session_id();
    let round = representative_parent.round() + 1;
    let mut parent_map = NodeMap::with_size(n_members);
    for parent in parents {
        parent_map.insert(parent.creator(), parent.hash());
    }
    let control_hash = ControlHash::new(&parent_map);
    preunit_to_full_unit(PreUnit::new(creator, round, control_hash), session_id)
}

pub fn random_full_parent_units_up_to(
    round: Round,
    n_members: NodeCount,
    session_id: SessionId,
) -> Vec<Vec<FullUnit>> {
    let mut result = vec![random_initial_units(n_members, session_id)];
    for _ in 0..round {
        let units = n_members
            .into_iterator()
            .map(|node_id| {
                random_unit_with_parents(node_id, result.last().expect("previous round present"))
            })
            .collect();
        result.push(units);
    }
    result
}
