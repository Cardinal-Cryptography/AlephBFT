use crate::{
    alerts::{Alert, ForkingNotification, NetworkMessage},
    creation,
    dag::{Dag, DagResult, DagStatus, DagUnit, Request as ReconstructionRequest},
    dissemination::{Request, Responder, Response},
    extension::Ordering,
    handle_task_termination,
    member::UnitMessage,
    units::{
        SignedUnit, UncheckedSignedUnit, Unit, UnitCoord, UnitStore, UnitStoreStatus, Validator,
        WrappedUnit,
    },
    Config, Data, DataProvider, Hasher, Index, Keychain, MultiKeychain, NodeIndex, Receiver, Round,
    Sender, Signature, SpawnHandle, Terminator, UncheckedSigned,
};
use aleph_bft_types::{Recipient, UnitFinalizationHandler};
use futures::{
    channel::{mpsc, oneshot},
    future::pending,
    pin_mut, AsyncRead, AsyncWrite, Future, FutureExt, StreamExt,
};
use futures_timer::Delay;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use std::{
    cmp::max,
    collections::HashSet,
    convert::TryFrom,
    fmt::{Display, Formatter, Result as FmtResult},
    marker::PhantomData,
    time::Duration,
};

mod collection;

use crate::backup::{BackupLoader, BackupSaver};
#[cfg(feature = "initial_unit_collection")]
use collection::{Collection, IO as CollectionIO};
pub use collection::{NewestUnitResponse, Salt};

pub(crate) enum RunwayNotificationOut<H: Hasher, D: Data, S: Signature> {
    /// A new unit was generated by this runway
    NewSelfUnit(UncheckedSignedUnit<H, D, S>),
    /// A new unit was generated by this runway or imported from outside and added to the DAG
    NewAnyUnit(UncheckedSignedUnit<H, D, S>),
    Request(Request<H>),
    Response(Response<H, D, S>, NodeIndex),
}

pub(crate) enum RunwayNotificationIn<H: Hasher, D: Data, S: Signature> {
    NewUnit(UncheckedSignedUnit<H, D, S>),
    Request(Request<H>, NodeIndex),
    Response(Response<H, D, S>),
}

impl<H: Hasher, D: Data, S: Signature> TryFrom<UnitMessage<H, D, S>>
    for RunwayNotificationIn<H, D, S>
{
    type Error = ();

    fn try_from(message: UnitMessage<H, D, S>) -> Result<Self, Self::Error> {
        let result = match message {
            UnitMessage::NewUnit(u) => RunwayNotificationIn::NewUnit(u),
            UnitMessage::RequestCoord(node_id, coord) => {
                RunwayNotificationIn::Request(Request::Coord(coord), node_id)
            }
            UnitMessage::RequestParents(node_id, u_hash) => {
                RunwayNotificationIn::Request(Request::Parents(u_hash), node_id)
            }
            UnitMessage::ResponseCoord(u) => RunwayNotificationIn::Response(Response::Coord(u)),
            UnitMessage::ResponseParents(u_hash, parents) => {
                RunwayNotificationIn::Response(Response::Parents(u_hash, parents))
            }
            UnitMessage::RequestNewest(node_id, salt) => {
                RunwayNotificationIn::Request(Request::NewestUnit(node_id, salt), node_id)
            }
            UnitMessage::ResponseNewest(response) => {
                RunwayNotificationIn::Response(Response::NewestUnit(response))
            }
        };
        Ok(result)
    }
}

type CollectionResponse<H, D, MK> = UncheckedSigned<
    NewestUnitResponse<H, D, <MK as Keychain>::Signature>,
    <MK as Keychain>::Signature,
>;

struct Runway<FH, MK>
where
    FH: UnitFinalizationHandler,
    MK: MultiKeychain,
{
    own_id: NodeIndex,
    missing_coords: HashSet<UnitCoord>,
    missing_parents: HashSet<<FH::Hasher as Hasher>::Hash>,
    store: UnitStore<DagUnit<FH::Hasher, FH::Data, MK>>,
    dag: Dag<FH::Hasher, FH::Data, MK>,
    ordering: Ordering<MK, FH>,
    responder: Responder<FH::Hasher, FH::Data, MK>,
    alerts_for_alerter: Sender<Alert<FH::Hasher, FH::Data, MK::Signature>>,
    notifications_from_alerter: Receiver<ForkingNotification<FH::Hasher, FH::Data, MK::Signature>>,
    unit_messages_from_network: Receiver<RunwayNotificationIn<FH::Hasher, FH::Data, MK::Signature>>,
    unit_messages_for_network: Sender<RunwayNotificationOut<FH::Hasher, FH::Data, MK::Signature>>,
    responses_for_collection: Sender<CollectionResponse<FH::Hasher, FH::Data, MK>>,
    resolved_requests: Sender<Request<FH::Hasher>>,
    parents_for_creator: Sender<DagUnit<FH::Hasher, FH::Data, MK>>,
    backup_units_for_saver: Sender<DagUnit<FH::Hasher, FH::Data, MK>>,
    backup_units_from_saver: Receiver<DagUnit<FH::Hasher, FH::Data, MK>>,
    new_units_from_creation: Receiver<SignedUnit<FH::Hasher, FH::Data, MK>>,
    exiting: bool,
}

struct RunwayStatus<'a, H: Hasher> {
    missing_coords: &'a HashSet<UnitCoord>,
    missing_parents: &'a HashSet<H::Hash>,
    dag_status: DagStatus,
    store_status: UnitStoreStatus,
}

impl<'a, H: Hasher> RunwayStatus<'a, H> {
    fn short_report(&self) -> String {
        let rounds_behind = max(self.dag_status.top_round(), self.store_status.top_round())
            - self.store_status.top_round();
        let missing_coords = self.missing_coords.len();
        match (rounds_behind, missing_coords) {
            (0..=2, 0) => "healthy".to_string(),
            (0..=2, 1..) => format!("syncing - missing {missing_coords} unit(s)"),
            (3.., 0) => format!("behind by {rounds_behind} rounds"),
            _ => format!(
                "syncing - missing {missing_coords} unit(s) and behind by {rounds_behind} rounds"
            ),
        }
    }

    fn format_missing_coords(c: &[(usize, Round)]) -> String {
        c.iter()
            .sorted()
            .chunk_by(|(creator, _)| *creator)
            .into_iter()
            .map(|(creator, rounds)| {
                // compress consecutive rounds into one interval to shorten logs
                let mut intervals: Vec<(Round, Round)> = Vec::new();
                for (_, round) in rounds {
                    if matches!(intervals.last(), Some(interval) if interval.1 == round-1) {
                        intervals.last_mut().unwrap().1 = *round;
                    } else {
                        intervals.push((*round, *round));
                    }
                }

                let intervals_str = intervals
                    .into_iter()
                    .map(|(begin, end)| {
                        if begin == end {
                            format!("{begin}")
                        } else if begin + 1 == end {
                            format!("{begin}, {end}")
                        } else {
                            format!("[{begin}-{end}]")
                        }
                    })
                    .format(", ");

                format!("{{Creator {creator}: {intervals_str}}}")
            })
            .join(", ")
    }
}

impl<'a, H: Hasher> Display for RunwayStatus<'a, H> {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        write!(f, "Runway status report: {}", self.short_report())?;
        if !self.missing_coords.is_empty() {
            let v_coords: Vec<(usize, Round)> = self
                .missing_coords
                .iter()
                .map(|uc| (uc.creator().into(), uc.round()))
                .collect();
            write!(
                f,
                "; missing coords - {}",
                Self::format_missing_coords(&v_coords)
            )?;
        }
        if !self.missing_parents.is_empty() {
            write!(f, "; missing parents - {:?}", self.missing_parents)?;
        }
        write!(f, ".")?;
        Ok(())
    }
}

struct RunwayConfig<UFH: UnitFinalizationHandler, MK: MultiKeychain> {
    finalization_handler: UFH,
    backup_units_for_saver: Sender<DagUnit<UFH::Hasher, UFH::Data, MK>>,
    backup_units_from_saver: Receiver<DagUnit<UFH::Hasher, UFH::Data, MK>>,
    alerts_for_alerter: Sender<Alert<UFH::Hasher, UFH::Data, MK::Signature>>,
    notifications_from_alerter:
        Receiver<ForkingNotification<UFH::Hasher, UFH::Data, MK::Signature>>,
    unit_messages_from_network:
        Receiver<RunwayNotificationIn<UFH::Hasher, UFH::Data, MK::Signature>>,
    unit_messages_for_network: Sender<RunwayNotificationOut<UFH::Hasher, UFH::Data, MK::Signature>>,
    responses_for_collection: Sender<CollectionResponse<UFH::Hasher, UFH::Data, MK>>,
    parents_for_creator: Sender<DagUnit<UFH::Hasher, UFH::Data, MK>>,
    resolved_requests: Sender<Request<UFH::Hasher>>,
    new_units_from_creation: Receiver<SignedUnit<UFH::Hasher, UFH::Data, MK>>,
}

type BackupUnits<UFH, MK> = Vec<
    UncheckedSignedUnit<
        <UFH as UnitFinalizationHandler>::Hasher,
        <UFH as UnitFinalizationHandler>::Data,
        <MK as Keychain>::Signature,
    >,
>;

impl<UFH, MK> Runway<UFH, MK>
where
    UFH: UnitFinalizationHandler,
    MK: MultiKeychain,
{
    fn new(config: RunwayConfig<UFH, MK>, keychain: MK, validator: Validator<MK>) -> Self {
        let n_members = keychain.node_count();
        let own_id = keychain.index();
        let RunwayConfig {
            finalization_handler,
            backup_units_for_saver,
            backup_units_from_saver,
            alerts_for_alerter,
            notifications_from_alerter,
            unit_messages_from_network,
            unit_messages_for_network,
            responses_for_collection,
            parents_for_creator,
            resolved_requests,
            new_units_from_creation,
        } = config;
        let store = UnitStore::new(n_members);
        let dag = Dag::new(validator);
        let ordering = Ordering::new(finalization_handler);

        Runway {
            own_id,
            store,
            dag,
            ordering,
            missing_coords: HashSet::new(),
            missing_parents: HashSet::new(),
            responder: Responder::new(keychain),
            resolved_requests,
            alerts_for_alerter,
            notifications_from_alerter,
            unit_messages_from_network,
            unit_messages_for_network,
            parents_for_creator,
            backup_units_for_saver,
            backup_units_from_saver,
            responses_for_collection,
            new_units_from_creation,
            exiting: false,
        }
    }

    fn index(&self) -> NodeIndex {
        self.own_id
    }

    fn handle_dag_result(&mut self, result: DagResult<UFH::Hasher, UFH::Data, MK>) {
        let DagResult {
            units,
            requests,
            alerts,
        } = result;
        for unit in units {
            self.on_unit_reconstructed(unit);
        }
        for request in requests {
            self.on_reconstruction_request(request);
        }
        for alert in alerts {
            if self.alerts_for_alerter.unbounded_send(alert).is_err() {
                warn!(target: "AlephBFT-runway", "{:?} Channel to alerter should be open", self.index());
                self.exiting = true;
            }
        }
    }

    fn on_unit_received(
        &mut self,
        unit: UncheckedSignedUnit<UFH::Hasher, UFH::Data, MK::Signature>,
    ) {
        let result = self.dag.add_unit(unit, &self.store);
        self.handle_dag_result(result);
    }

    fn on_unit_message(
        &mut self,
        message: RunwayNotificationIn<UFH::Hasher, UFH::Data, MK::Signature>,
    ) {
        match message {
            RunwayNotificationIn::NewUnit(u) => {
                trace!(target: "AlephBFT-runway", "{:?} New unit received {:?}.", self.index(), &u);
                self.on_unit_received(u)
            }

            RunwayNotificationIn::Request(request, node_id) => {
                match self.responder.handle_request(request, &self.store) {
                    Ok(response) => self.send_message_for_network(RunwayNotificationOut::Response(
                        response, node_id,
                    )),
                    Err(err) => {
                        trace!(target: "AlephBFT-runway", "Not answering request from node {:?}: {}.", node_id, err)
                    }
                }
            }

            RunwayNotificationIn::Response(res) => match res {
                Response::Coord(u) => {
                    trace!(target: "AlephBFT-runway", "{:?} Fetch response received {:?}.", self.index(), &u);
                    self.on_unit_received(u)
                }
                Response::Parents(u_hash, parents) => {
                    trace!(target: "AlephBFT-runway", "{:?} Response parents received {:?}.", self.index(), u_hash);
                    self.on_parents_response(u_hash, parents)
                }
                Response::NewestUnit(response) => {
                    trace!(target: "AlephBFT-runway", "{:?} Response newest unit received from {:?}.", self.index(), response.index());
                    let res = self.responses_for_collection.unbounded_send(response);
                    if res.is_err() {
                        debug!(target: "AlephBFT-runway", "{:?} Could not send response to collection ({:?}).", self.index(), res)
                    }
                }
            },
        }
    }

    fn resolve_missing_coord(&mut self, coord: &UnitCoord) {
        if self.missing_coords.remove(coord) {
            self.send_resolved_request_notification(Request::Coord(*coord));
        }
    }

    fn on_parents_response(
        &mut self,
        u_hash: <UFH::Hasher as Hasher>::Hash,
        parents: Vec<UncheckedSignedUnit<UFH::Hasher, UFH::Data, MK::Signature>>,
    ) {
        if self.store.unit(&u_hash).is_some() {
            trace!(target: "AlephBFT-runway", "{:?} We got parents response but already imported the unit.", self.index());
            return;
        }
        let result = self.dag.add_parents(u_hash, parents, &self.store);
        self.handle_dag_result(result);
    }

    fn on_forking_notification(
        &mut self,
        notification: ForkingNotification<UFH::Hasher, UFH::Data, MK::Signature>,
    ) {
        let result = self
            .dag
            .process_forking_notification(notification, &self.store);
        self.handle_dag_result(result);
    }

    fn resolve_missing_parents(&mut self, u_hash: &<UFH::Hasher as Hasher>::Hash) {
        if self.missing_parents.remove(u_hash) {
            self.send_resolved_request_notification(Request::Parents(*u_hash));
        }
    }

    fn on_reconstruction_request(&mut self, request: ReconstructionRequest<UFH::Hasher>) {
        use ReconstructionRequest::*;
        match request {
            Coord(coord) => {
                self.on_missing_coord(coord);
            }
            ParentsOf(h) => {
                self.on_wrong_control_hash(h);
            }
        }
    }

    fn on_unit_reconstructed(&mut self, unit: DagUnit<UFH::Hasher, UFH::Data, MK>) {
        let unit_hash = unit.hash();
        trace!(target: "AlephBFT-runway", "Unit {:?} {} reconstructed.", unit_hash, unit.coord());
        if self.backup_units_for_saver.unbounded_send(unit).is_err() {
            error!(target: "AlephBFT-runway", "{:?} A unit couldn't be sent to backup: {:?}.", self.index(), unit_hash);
        }
    }

    fn on_unit_backup_saved(&mut self, unit: DagUnit<UFH::Hasher, UFH::Data, MK>) {
        let unit_hash = unit.hash();
        self.store.insert(unit.clone());
        self.dag.finished_processing(&unit_hash);
        self.resolve_missing_parents(&unit_hash);
        self.resolve_missing_coord(&unit.coord());
        if self
            .parents_for_creator
            .unbounded_send(unit.clone())
            .is_err()
        {
            warn!(target: "AlephBFT-runway", "Creator channel should be open.");
            self.exiting = true;
        }
        let unpacked_unit = unit.clone().unpack();
        self.send_message_for_network(RunwayNotificationOut::NewAnyUnit(
            unpacked_unit.clone().into(),
        ));

        if unit.creator() == self.index() {
            trace!(target: "AlephBFT-runway", "{:?} Sending a unit {:?}.", self.index(), unit.hash());
            self.send_message_for_network(RunwayNotificationOut::NewSelfUnit(unpacked_unit.into()));
        }
        self.ordering.add_unit(unit.clone());
    }

    fn on_missing_coord(&mut self, coord: UnitCoord) {
        trace!(target: "AlephBFT-runway", "{:?} Dealing with missing coord notification {:?}.", self.index(), coord);
        if self.store.canonical_unit(coord).is_none() {
            let new_request = self.missing_coords.insert(coord);
            if new_request {
                self.send_message_for_network(RunwayNotificationOut::Request(Request::Coord(
                    coord,
                )));
            }
        }
    }

    fn on_wrong_control_hash(&mut self, u_hash: <UFH::Hasher as Hasher>::Hash) {
        trace!(target: "AlephBFT-runway", "{:?} Dealing with wrong control hash notification {:?}.", self.index(), u_hash);
        if self.missing_parents.insert(u_hash) {
            self.send_message_for_network(RunwayNotificationOut::Request(Request::Parents(u_hash)));
        }
    }

    fn send_message_for_network(
        &mut self,
        notification: RunwayNotificationOut<UFH::Hasher, UFH::Data, MK::Signature>,
    ) {
        if self
            .unit_messages_for_network
            .unbounded_send(notification)
            .is_err()
        {
            warn!(target: "AlephBFT-runway", "{:?} unit_messages_for_network channel should be open", self.index());
            self.exiting = true;
        }
    }

    fn send_resolved_request_notification(&mut self, notification: Request<UFH::Hasher>) {
        if self.resolved_requests.unbounded_send(notification).is_err() {
            warn!(target: "AlephBFT-runway", "{:?} resolved_requests channel should be open", self.index());
            self.exiting = true;
        }
    }

    fn status(&self) -> RunwayStatus<'_, UFH::Hasher> {
        RunwayStatus {
            missing_coords: &self.missing_coords,
            missing_parents: &self.missing_parents,
            dag_status: self.dag.status(),
            store_status: self.store.status(),
        }
    }

    fn status_report(&self) {
        info!(target: "AlephBFT-runway", "{}", self.status());
    }

    async fn run(
        mut self,
        data_from_backup: oneshot::Receiver<BackupUnits<UFH, MK>>,
        mut terminator: Terminator,
    ) {
        let index = self.index();
        let data_from_backup = data_from_backup.fuse();
        pin_mut!(data_from_backup);

        let status_ticker_delay = Duration::from_secs(10);
        let mut status_ticker = Delay::new(status_ticker_delay).fuse();

        match data_from_backup.await {
            Ok(units) => {
                for unit in units {
                    self.on_unit_received(unit);
                }
            }
            Err(e) => {
                error!(target: "AlephBFT-runway", "{:?} Units message from backup channel closed: {:?}", index, e);
                return;
            }
        }

        debug!(target: "AlephBFT-runway", "{:?} Runway started.", index);
        loop {
            futures::select! {
                signed_unit = self.new_units_from_creation.next() => match signed_unit {
                    Some(signed_unit) => self.on_unit_received(signed_unit.into()),
                    None => {
                        error!(target: "AlephBFT-runway", "{:?} Creation stream closed.", index);
                        break;
                    }
                },

                notification = self.notifications_from_alerter.next() => match notification {
                    Some(notification) => {
                        trace!(target: "AlephBFT-runway", "Received alerter notification: {:?}.", notification);
                        self.on_forking_notification(notification);
                    },
                    None => {
                        error!(target: "AlephBFT-runway", "{:?} Alert notification stream closed.", index);
                        break;
                    }
                },

                event = self.unit_messages_from_network.next() => match event {
                    Some(event) => self.on_unit_message(event),
                    None => {
                        error!(target: "AlephBFT-runway", "{:?} Unit message stream closed.", index);
                        break;
                    }
                },

                message = self.backup_units_from_saver.next() => match message {
                    Some(unit) => self.on_unit_backup_saved(unit),
                    None => {
                        error!(target: "AlephBFT-runway", "{:?} Saved units receiver closed.", index);
                    }
                },

                _ = &mut status_ticker => {
                    self.status_report();
                    status_ticker = Delay::new(status_ticker_delay).fuse();
                },

                _ = terminator.get_exit().fuse() => {
                    debug!(target: "AlephBFT-runway", "{:?} received exit signal", index);
                    self.exiting = true;
                }
            }

            if self.exiting {
                debug!(target: "AlephBFT-runway", "{:?} Runway decided to exit.", index);
                terminator.terminate_sync().await;
                break;
            }
        }

        debug!(target: "AlephBFT-runway", "{:?} Run ended.", index);
    }
}

pub(crate) struct NetworkIO<H: Hasher, D: Data, MK: MultiKeychain> {
    pub(crate) alert_messages_for_network: Sender<(NetworkMessage<H, D, MK>, Recipient)>,
    pub(crate) alert_messages_from_network: Receiver<NetworkMessage<H, D, MK>>,
    pub(crate) unit_messages_for_network: Sender<RunwayNotificationOut<H, D, MK::Signature>>,
    pub(crate) unit_messages_from_network: Receiver<RunwayNotificationIn<H, D, MK::Signature>>,
    pub(crate) resolved_requests: Sender<Request<H>>,
}

#[cfg(feature = "initial_unit_collection")]
fn initial_unit_collection<'a, H: Hasher, D: Data, MK: MultiKeychain>(
    keychain: &'a MK,
    validator: &'a Validator<MK>,
    unit_messages_for_network: &Sender<RunwayNotificationOut<H, D, MK::Signature>>,
    unit_collection_sender: oneshot::Sender<Round>,
    responses_from_runway: Receiver<CollectionResponse<H, D, MK>>,
    resolved_requests: Sender<Request<H>>,
) -> Result<impl Future<Output = ()> + 'a, ()> {
    let (collection, salt) = Collection::new(keychain, validator);
    let notification = RunwayNotificationOut::Request(Request::NewestUnit(keychain.index(), salt));

    if let Err(e) = unit_messages_for_network.unbounded_send(notification) {
        error!(target: "AlephBFT-runway", "Unable to send the newest unit request: {}", e);
        return Err(());
    };

    let collection = CollectionIO::new(
        unit_collection_sender,
        responses_from_runway,
        resolved_requests,
        collection,
    );
    Ok(collection.run())
}

#[cfg(not(feature = "initial_unit_collection"))]
fn trivial_start(
    starting_round_sender: oneshot::Sender<Round>,
) -> Result<impl Future<Output = ()>, ()> {
    if let Err(e) = starting_round_sender.send(0) {
        error!(target: "AlephBFT-runway", "Unable to send the starting round: {}", e);
        return Err(());
    }
    Ok(async {})
}

pub struct RunwayIO<
    MK: MultiKeychain,
    W: AsyncWrite + Send + Sync + 'static,
    R: AsyncRead + Send + Sync + 'static,
    DP: DataProvider,
    UFH: UnitFinalizationHandler,
> {
    pub data_provider: DP,
    pub finalization_handler: UFH,
    pub backup_write: W,
    pub backup_read: R,
    _phantom: PhantomData<MK::Signature>,
}

impl<
        MK: MultiKeychain,
        W: AsyncWrite + Send + Sync + 'static,
        R: AsyncRead + Send + Sync + 'static,
        DP: DataProvider,
        UFH: UnitFinalizationHandler,
    > RunwayIO<MK, W, R, DP, UFH>
{
    pub fn new(
        data_provider: DP,
        finalization_handler: UFH,
        backup_write: W,
        backup_read: R,
    ) -> Self {
        RunwayIO {
            data_provider,
            finalization_handler,
            backup_write,
            backup_read,
            _phantom: PhantomData,
        }
    }
}

pub(crate) async fn run<US, UL, MK, DP, UFH, SH>(
    config: Config,
    runway_io: RunwayIO<MK, US, UL, DP, UFH>,
    keychain: MK,
    spawn_handle: SH,
    network_io: NetworkIO<UFH::Hasher, DP::Output, MK>,
    mut terminator: Terminator,
) where
    US: AsyncWrite + Send + Sync + 'static,
    UL: AsyncRead + Send + Sync + 'static,
    DP: DataProvider,
    UFH: UnitFinalizationHandler<Data = DP::Output>,
    MK: MultiKeychain,
    SH: SpawnHandle,
{
    let RunwayIO {
        data_provider,
        finalization_handler,
        backup_write,
        backup_read,
        _phantom: _,
    } = runway_io;

    let (new_units_for_runway, new_units_from_creation) = mpsc::unbounded();

    let (parents_for_creator, parents_from_runway) = mpsc::unbounded();
    let creation_terminator = terminator.add_offspring_connection("creator");
    let creation_config = config.clone();
    let (starting_round_sender, starting_round) = oneshot::channel();

    let creation_keychain = keychain.clone();
    let creation_handle = spawn_handle
        .spawn_essential("runway/creation", async move {
            creation::run(
                creation_config,
                creation::IO {
                    outgoing_units: new_units_for_runway,
                    incoming_parents: parents_from_runway,
                    data_provider,
                },
                creation_keychain,
                starting_round,
                creation_terminator,
            )
            .await
        })
        .shared();
    let creator_handle_for_panic = creation_handle.clone();
    let creator_panic_handle = async move {
        if creator_handle_for_panic.await.is_err() {
            return;
        }
        pending().await
    }
    .fuse();
    pin_mut!(creator_panic_handle);
    let creation_handle = creation_handle.fuse();

    let (backup_units_for_saver, backup_units_from_runway) = mpsc::unbounded();
    let (backup_units_for_runway, backup_units_from_saver) = mpsc::unbounded();

    let backup_saver_terminator = terminator.add_offspring_connection("AlephBFT-backup-saver");
    let backup_saver_handle = spawn_handle.spawn_essential("runway/backup_saver", {
        let mut backup_saver = BackupSaver::new(
            backup_units_from_runway,
            backup_units_for_runway,
            backup_write,
        );
        async move {
            backup_saver.run(backup_saver_terminator).await;
        }
    });
    let mut backup_saver_handle = backup_saver_handle.fuse();

    let (alert_notifications_for_units, notifications_from_alerter) = mpsc::unbounded();
    let (alerts_for_alerter, alerts_from_units) = mpsc::unbounded();

    let alerter_terminator = terminator.add_offspring_connection("AlephBFT-alerter");
    let alerter_keychain = keychain.clone();
    let alert_messages_for_network = network_io.alert_messages_for_network;
    let alert_messages_from_network = network_io.alert_messages_from_network;
    let alerter_handler =
        crate::alerts::Handler::new(alerter_keychain.clone(), config.session_id());

    let mut alerter_service = crate::alerts::Service::new(
        alerter_keychain,
        crate::alerts::IO {
            messages_for_network: alert_messages_for_network,
            messages_from_network: alert_messages_from_network,
            notifications_for_units: alert_notifications_for_units,
            alerts_from_units,
        },
        alerter_handler,
    );

    let mut alerter_handle = spawn_handle
        .spawn_essential("runway/alerter", async move {
            alerter_service.run(alerter_terminator).await;
        })
        .fuse();

    let index = keychain.index();
    let validator = Validator::new(config.session_id(), keychain.clone(), config.max_round());
    let (responses_for_collection, responses_from_runway) = mpsc::unbounded();
    let (unit_collections_sender, unit_collection_result) = oneshot::channel();
    let (loaded_data_tx, loaded_data_rx) = oneshot::channel();
    let session_id = config.session_id();

    let backup_loading_handle = spawn_handle
        .spawn_essential("runway/loading", {
            let mut backup_loader = BackupLoader::new(backup_read, index, session_id);
            async move {
                backup_loader
                    .run(
                        loaded_data_tx,
                        starting_round_sender,
                        unit_collection_result,
                    )
                    .await
            }
        })
        .fuse();
    pin_mut!(backup_loading_handle);

    #[cfg(feature = "initial_unit_collection")]
    let starting_round_handle = match initial_unit_collection(
        &keychain,
        &validator,
        &network_io.unit_messages_for_network,
        unit_collections_sender,
        responses_from_runway,
        network_io.resolved_requests.clone(),
    ) {
        Ok(handle) => handle.fuse(),
        Err(_) => return,
    };
    #[cfg(not(feature = "initial_unit_collection"))]
    let starting_round_handle = match trivial_start(unit_collections_sender) {
        Ok(handle) => handle.fuse(),
        Err(_) => return,
    };
    pin_mut!(starting_round_handle);

    let runway_handle = spawn_handle
        .spawn_essential("runway", {
            let runway_config = RunwayConfig {
                finalization_handler,
                backup_units_for_saver,
                backup_units_from_saver,
                alerts_for_alerter,
                notifications_from_alerter,
                unit_messages_from_network: network_io.unit_messages_from_network,
                unit_messages_for_network: network_io.unit_messages_for_network,
                parents_for_creator,
                responses_for_collection,
                resolved_requests: network_io.resolved_requests,
                new_units_from_creation,
            };
            let runway_terminator = terminator.add_offspring_connection("AlephBFT-runway");
            let validator = validator.clone();
            let keychain = keychain.clone();
            let runway = Runway::new(runway_config, keychain, validator);

            async move { runway.run(loaded_data_rx, runway_terminator).await }
        })
        .fuse();
    pin_mut!(runway_handle);

    loop {
        futures::select! {
            _ = runway_handle => {
                debug!(target: "AlephBFT-runway", "{:?} Runway task terminated early.", index);
                break;
            },
            _ = alerter_handle => {
                debug!(target: "AlephBFT-runway", "{:?} Alerter task terminated early.", index);
                break;
            },
            _ = creator_panic_handle => {
                debug!(target: "AlephBFT-runway", "{:?} creator task terminated early with its task being dropped.", index);
                break;
            },
            _ = backup_saver_handle => {
                debug!(target: "AlephBFT-runway", "{:?} Backup saving task terminated early.", index);
                break;
            },
            _ = starting_round_handle => {
                debug!(target: "AlephBFT-runway", "{:?} Starting round task terminated.", index);
            },
            _ = backup_loading_handle => {
                debug!(target: "AlephBFT-runway", "{:?} Backup loading task terminated.", index);
            },
            _ = terminator.get_exit().fuse() => {
                break;
            }
        }
    }

    debug!(target: "AlephBFT-runway", "{:?} Ending run.", index);
    terminator.terminate_sync().await;

    handle_task_termination(creation_handle, "AlephBFT-runway", "Creator", index).await;
    handle_task_termination(alerter_handle, "AlephBFT-runway", "Alerter", index).await;
    handle_task_termination(runway_handle, "AlephBFT-runway", "Runway", index).await;
    handle_task_termination(backup_saver_handle, "AlephBFT-runway", "BackupSaver", index).await;

    debug!(target: "AlephBFT-runway", "{:?} Runway ended.", index);
}

#[cfg(test)]
mod tests {
    use crate::runway::RunwayStatus;
    use aleph_bft_mock::Hasher64;

    #[test]
    pub fn formats_missing_coords() {
        let format_missing_coords = RunwayStatus::<Hasher64>::format_missing_coords;
        assert_eq!(format_missing_coords(&[]), "");
        assert_eq!(format_missing_coords(&[(0, 13)]), "{Creator 0: 13}");
        assert_eq!(
            format_missing_coords(&[(0, 1), (0, 2)]),
            "{Creator 0: 1, 2}"
        );
        assert_eq!(
            format_missing_coords(&[(0, 1), (0, 2), (0, 3)]),
            "{Creator 0: [1-3]}"
        );
        assert_eq!(
            format_missing_coords(&[
                (0, 1),
                (0, 3),
                (0, 4),
                (0, 5),
                (0, 6),
                (0, 9),
                (0, 10),
                (0, 12)
            ]),
            "{Creator 0: 1, [3-6], 9, 10, 12}"
        );
        assert_eq!(
            format_missing_coords(&[(1, 3), (0, 1), (1, 1), (3, 0)]),
            "{Creator 0: 1}, {Creator 1: 1, 3}, {Creator 3: 0}"
        );
    }
}
