use futures::{FutureExt, StreamExt};
use std::collections::{hash_map::Entry, HashMap, VecDeque};
use tokio::sync::oneshot;

use crate::{
    extender::ExtenderUnit,
    nodes::{NodeCount, NodeIndex, NodeMap},
    ControlHash, HashT, NodeIdT, NotificationIn, NotificationOut, Receiver, RequestAuxData, Round,
    Sender, Unit,
};
use log::{debug, error};

/// An enum describing the status of a Unit in the Terminal pipeline.
#[derive(Clone, Debug, PartialEq)]
pub enum UnitStatus {
    ReconstructingParents,
    WrongControlHash,
    WaitingParentsInDag,
    InDag,
}

impl Default for UnitStatus {
    fn default() -> UnitStatus {
        UnitStatus::ReconstructingParents
    }
}

/// A Unit struct used in the Terminal. It stores a copy of a unit and apart from that some
/// information on its status, i.e., already reconstructed parents etc.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalUnit<H: HashT> {
    unit: Unit<H>,
    parents: NodeMap<Option<H>>,
    n_miss_par_decoded: NodeCount,
    n_miss_par_dag: NodeCount,
    status: UnitStatus,
}

impl<H: HashT> From<TerminalUnit<H>> for ExtenderUnit<H> {
    fn from(u: TerminalUnit<H>) -> ExtenderUnit<H> {
        ExtenderUnit::new(u.unit.creator, u.unit.round(), u.unit.hash, u.parents)
    }
}

impl<H: HashT> From<TerminalUnit<H>> for Unit<H> {
    fn from(u: TerminalUnit<H>) -> Unit<H> {
        u.unit
    }
}

impl<H: HashT> TerminalUnit<H> {
    // creates a unit from a Control Hash Unit, that has no parents reconstructed yet
    pub(crate) fn blank_from_unit(unit: &Unit<H>) -> Self {
        let n_members = unit.control_hash.n_members();
        let n_parents = unit.control_hash.n_parents();
        TerminalUnit {
            unit: unit.clone(),
            parents: NodeMap::new_with_len(n_members),
            n_miss_par_decoded: n_parents,
            n_miss_par_dag: n_parents,
            status: UnitStatus::ReconstructingParents,
        }
    }

    pub(crate) fn _round(&self) -> Round {
        self.unit.round()
    }

    pub(crate) fn _hash(&self) -> H {
        self.unit.hash
    }

    pub(crate) fn verify_control_hash<Hashing: Fn(&[u8]) -> H>(&self, hashing: &Hashing) -> bool {
        // this will be called only after all parents have been reconstructed

        self.unit.control_hash.hash == ControlHash::combine_hashes(&self.parents, hashing)
    }
}

// This enum could be simplified to just one option and consequently gotten rid off. However,
// this has been made slightly more general (and thus complex) to add Alerts easily in the future.
pub enum TerminalEvent<H: HashT> {
    ParentsReconstructed(H),
    ParentsInDag(H),
}

type SyncClosure<X, Y> = Box<dyn Fn(X) -> Y + Sync + Send + 'static>;

/// A process whose goal is to receive new units and place them in our local Dag. Towards this end
/// this process must orchestrate fetching units from other nodes in case we are missing them and
/// manage the `Alert` mechanism which makes sure `horizontal spam` (unit fork spam) is not possible.
/// Importantly, our local Dag is a set of units that are *guaranteed* to be sooner or later
/// received by all honest nodes in the network.
/// The Terminal receives new units via the new_units_rx channel endpoint and pushes requests for units
/// to the notification_tx channel endpoint.
pub(crate) struct Terminal<H: HashT, NI: NodeIdT> {
    node_id: NI,
    hashing: Box<dyn Fn(&[u8]) -> H + Send>,
    // A channel for receiving notifications (units mainly)
    ntfct_rx: Receiver<NotificationIn<H>>,
    // A channel to push outgoing notifications
    ntfct_tx: Sender<NotificationOut<H>>,
    // A Queue that is necessary to deal with cascading updates to the Dag/Store
    event_queue: VecDeque<TerminalEvent<H>>,
    post_insert: Vec<SyncClosure<TerminalUnit<H>, ()>>,
    // Here we store all the units -- the one in Dag and the ones "hanging".
    unit_store: HashMap<H, TerminalUnit<H>>,

    // TODO: get rid of HashMaps below and just use Vec<Vec<H>> for efficiency

    // In this Map, for each pair (r, pid) we store the first unit made by pid at round r that we ever received.
    // In case of forks, we still store only the first one -- others are ignored (but stored in store anyway of course).
    unit_by_coord: HashMap<(Round, NodeIndex), H>,
    // This stores, for a pair (r, pid) the list of all units (by hash) that await a unit made by pid at
    // round r, as their parent. Once such a unit arrives, we notify all these children.
    children_coord: HashMap<(Round, NodeIndex), Vec<H>>,
    // The same as above, but this time we await for a unit (with a particular hash) to be added to the Dag.
    // Once this happens, we notify all the children.
    children_hash: HashMap<H, Vec<H>>,
}

impl<H: HashT, NI: NodeIdT> Terminal<H, NI> {
    pub(crate) fn new(
        node_id: NI,
        hashing: impl Fn(&[u8]) -> H + Send + 'static,
        ntfct_rx: Receiver<NotificationIn<H>>,
        ntfct_tx: Sender<NotificationOut<H>>,
    ) -> Self {
        Terminal {
            node_id,
            hashing: Box::new(hashing),
            ntfct_rx,
            ntfct_tx,
            event_queue: VecDeque::new(),
            post_insert: Vec::new(),
            unit_store: HashMap::new(),
            unit_by_coord: HashMap::new(),
            children_coord: HashMap::new(),
            children_hash: HashMap::new(),
        }
    }

    // Reconstruct the parent of a unit u (given by hash u_hash) at position pid as p (given by hash p_hash)
    fn reconstruct_parent(&mut self, u_hash: &H, pid: NodeIndex, p_hash: &H) {
        let u = self.unit_store.get_mut(u_hash).unwrap();
        // the above unwraps must succeed, should probably add some debug messages here...

        u.parents[pid] = Some(*p_hash);
        u.n_miss_par_decoded -= NodeCount(1);
        if u.n_miss_par_decoded == NodeCount(0) {
            self.event_queue
                .push_back(TerminalEvent::ParentsReconstructed(*u_hash));
        }
    }

    // update u's count (u given by hash u_hash) of parents present in Dag and trigger the event of
    // adding u itself to the Dag
    fn new_parent_in_dag(&mut self, u_hash: &H) {
        let u = self.unit_store.get_mut(u_hash).unwrap();
        u.n_miss_par_dag -= NodeCount(1);
        if u.n_miss_par_dag == NodeCount(0) {
            self.event_queue
                .push_back(TerminalEvent::ParentsInDag(*u_hash));
        }
    }

    fn add_coord_trigger(&mut self, round: Round, pid: NodeIndex, u_hash: H) {
        let coord = (round, pid);
        if !self.children_coord.contains_key(&coord) {
            self.children_coord.insert((round, pid), Vec::new());
        }
        let wait_list = self.children_coord.get_mut(&coord).unwrap();
        wait_list.push(u_hash);
    }

    fn add_hash_trigger(&mut self, p_hash: &H, u_hash: &H) {
        if !self.children_hash.contains_key(p_hash) {
            self.children_hash.insert(*p_hash, Vec::new());
        }
        let wait_list = self.children_hash.get_mut(p_hash).unwrap();
        wait_list.push(*u_hash);
    }

    fn update_on_store_add(&mut self, u: Unit<H>) {
        let u_hash = u.hash;
        let (u_round, pid) = (u.round(), u.creator);
        self.unit_by_coord.insert((u_round, pid), u_hash);
        if let Some(children) = self.children_coord.remove(&(u_round, pid)) {
            for v_hash in children {
                self.reconstruct_parent(&v_hash, pid, &u_hash);
            }
        }
        if u_round == 0 {
            self.event_queue
                .push_back(TerminalEvent::ParentsReconstructed(u_hash));
        } else {
            let mut coords_to_request = Vec::new();
            for (i, b) in u.control_hash.parents.enumerate() {
                if *b {
                    let coord = (u_round - 1, i);
                    let maybe_hash = self.unit_by_coord.get(&coord).cloned();
                    match maybe_hash {
                        Some(v_hash) => self.reconstruct_parent(&u_hash, i, &v_hash),
                        None => {
                            self.add_coord_trigger(u_round - 1, i, u_hash);
                            coords_to_request.push((u.round() - 1, i).into());
                        }
                    }
                }
            }
            if !coords_to_request.is_empty() {
                let aux_data = RequestAuxData::new(u.creator);
                debug!(target: "rush-terminal", "{} Missing coords {:?} aux {:?}", self.node_id, coords_to_request, aux_data);
                let send_result = self
                    .ntfct_tx
                    .send(NotificationOut::MissingUnits(coords_to_request, aux_data));
                if let Err(e) = send_result {
                    error!(target: "rush-terminal", "{:?} Unable to place a Fetch request: {:?}.", self.node_id, e);
                }
            }
        }
    }

    fn update_on_dag_add(&mut self, u_hash: &H) {
        let u = self
            .unit_store
            .get(u_hash)
            .expect("Unit to be added to dag must be in store")
            .clone();
        self.post_insert.iter().for_each(|f| f(u.clone()));
        if let Some(children) = self.children_hash.remove(u_hash) {
            for v_hash in children {
                self.new_parent_in_dag(&v_hash);
            }
        }
        let mut parent_hashes = Vec::new();
        for p_hash in u.parents.iter().flatten() {
            parent_hashes.push(*p_hash);
        }

        let send_result = self
            .ntfct_tx
            .send(NotificationOut::AddedToDag(*u_hash, parent_hashes));
        if let Err(e) = send_result {
            error!(target: "rush-terminal", "{:?} Unable to place AddedToDag notification: {:?}.", self.node_id, e);
        }
    }
    // We set the correct parent hashes for unit u.
    fn update_on_wrong_hash_response(&mut self, u_hash: H, p_hashes: Vec<H>) {
        let u = self
            .unit_store
            .get_mut(&u_hash)
            .expect("unit with wrong control hash must be in store");
        let mut counter = 0;
        for (i, b) in u.unit.control_hash.parents.enumerate() {
            if *b {
                u.parents[i] = Some(p_hashes[counter]);
                counter += 1;
            }
        }
        u.n_miss_par_decoded = NodeCount(0);
        self.inspect_parents_in_dag(&u_hash);
    }

    fn add_to_store(&mut self, u: Unit<H>) {
        debug!(target: "rush-terminal", "{} Adding to store {} round {:?} index {:?}", self.node_id, u.hash(), u.round, u.creator);
        if let Entry::Vacant(entry) = self.unit_store.entry(u.hash()) {
            entry.insert(TerminalUnit::<H>::blank_from_unit(&u));
            self.update_on_store_add(u);
        }
    }

    fn inspect_parents_in_dag(&mut self, u_hash: &H) {
        let u_parents = self.unit_store.get(&u_hash).unwrap().parents.clone();
        let mut n_parents_in_dag = NodeCount(0);
        for p_hash in u_parents.into_iter().flatten() {
            let maybe_p = self.unit_store.get(&p_hash);
            // p might not be even in store because u might be a unit with wrong control hash
            match maybe_p {
                Some(p) if p.status == UnitStatus::InDag => {
                    n_parents_in_dag += NodeCount(1);
                }
                _ => {
                    self.add_hash_trigger(&p_hash, u_hash);
                }
            }
        }
        let u = self.unit_store.get_mut(&u_hash).unwrap();
        u.n_miss_par_dag -= n_parents_in_dag;
        if u.n_miss_par_dag == NodeCount(0) {
            self.event_queue
                .push_back(TerminalEvent::ParentsInDag(*u_hash));
        } else {
            u.status = UnitStatus::WaitingParentsInDag;
            // when the last parent is added an appropriate TerminalEvent will be triggered
        }
    }

    fn on_wrong_hash_detected(&mut self, u_hash: H) {
        let send_result = self
            .ntfct_tx
            .send(NotificationOut::WrongControlHash(u_hash));
        if let Err(e) = send_result {
            error!(target: "rush-terminal", "{:?} Unable to place a Fetch request: {:?}.", self.node_id, e);
        }
    }

    // This drains the event queue. Note that new events might be added to the queue as the result of
    // handling other events -- for instance when a unit u is waiting for its parent p, and this parent p waits
    // for his parent pp. In this case adding pp to the Dag, will trigger adding p, which in turns triggers
    // adding u.
    fn handle_events(&mut self) {
        while let Some(event) = self.event_queue.pop_front() {
            match event {
                TerminalEvent::ParentsReconstructed(u_hash) => {
                    let u = self.unit_store.get_mut(&u_hash).unwrap();
                    if u.verify_control_hash(&self.hashing) {
                        self.inspect_parents_in_dag(&u_hash);
                    } else {
                        u.status = UnitStatus::WrongControlHash;
                        debug!(target: "rush-terminal", "{} wrong control hash", self.node_id);
                        self.on_wrong_hash_detected(u_hash);
                    }
                }
                TerminalEvent::ParentsInDag(u_hash) => {
                    let u = self.unit_store.get_mut(&u_hash).unwrap();
                    u.status = UnitStatus::InDag;
                    debug!(target: "rush-terminal", "{} Adding to Dag {} round {:?} index {}.", self.node_id, u_hash, u.unit.round, u.unit.creator);
                    self.update_on_dag_add(&u_hash);
                }
            }
        }
    }

    pub(crate) fn register_post_insert_hook(&mut self, hook: SyncClosure<TerminalUnit<H>, ()>) {
        self.post_insert.push(hook);
    }

    pub(crate) async fn run(&mut self, exit: oneshot::Receiver<()>) {
        let mut exit = exit.into_stream();
        loop {
            tokio::select! {
                Some(n) = self.ntfct_rx.recv() => {
                    match n {
                        NotificationIn::NewUnits(units) => {
                            for u in units {
                                self.add_to_store(u);
                                self.handle_events();
                            }
                        },
                        NotificationIn::UnitParents(u_hash, p_hashes) => {
                            self.update_on_wrong_hash_response(u_hash, p_hashes);
                        }
                    }


                }
                _ = exit.next() => {
                    debug!(target: "rush-terminal", "{} received exit signal.", self.node_id);
                    break
                }
            }
        }
    }
}
