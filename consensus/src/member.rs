use crate::{
    dag::Request as ReconstructionRequest,
    dissemination::{Addressed, DisseminationMessage, DisseminationTask},
    handle_task_termination,
    network::{Hub as NetworkHub, NetworkData, UnitMessage},
    runway::{self, NetworkIO, RunwayIO, RunwayNotificationOut},
    task_queue::TaskQueue,
    units::{UncheckedSignedUnit, Unit, UnitCoord},
    Config, Data, DataProvider, FinalizationHandler, Hasher, MultiKeychain, Network, NodeIndex,
    OrderedUnit, Receiver, Recipient, Round, Sender, Signature, SpawnHandle, Terminator,
    UnitFinalizationHandler,
};
use aleph_bft_types::NodeMap;
use futures::{channel::mpsc, pin_mut, AsyncRead, AsyncWrite, FutureExt, StreamExt};
use futures_timer::Delay;
use itertools::Itertools;
use log::{debug, error, info, trace, warn};
use rand::{prelude::SliceRandom, Rng};
use std::{
    collections::HashSet,
    fmt::{self, Debug},
    marker::PhantomData,
    time::Duration,
};

#[derive(Eq, PartialEq, Debug)]
struct RepeatableTask<H: Hasher, D: Data, S: Signature> {
    task: DisseminationTask<H, D, S>,
    counter: usize,
}

impl<H: Hasher, D: Data, S: Signature> fmt::Display for RepeatableTask<H, D, S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "RepeatableTask({:?}, counter {})",
            self.task, self.counter
        )
    }
}

impl<H: Hasher, D: Data, S: Signature> RepeatableTask<H, D, S> {
    fn new(task: DisseminationTask<H, D, S>) -> Self {
        Self { task, counter: 0 }
    }
}

enum TaskDetails<H: Hasher, D: Data, S: Signature> {
    Cancel,
    Perform {
        message: UnitMessage<H, D, S>,
        recipients: Vec<Recipient>,
        reschedule: Duration,
    },
}

/// This adapter allows to map an implementation of [`FinalizationHandler`] onto implementation of [`UnitFinalizationHandler`].
pub struct FinalizationHandlerAdapter<FH, D, H> {
    finalization_handler: FH,
    _phantom: PhantomData<(D, H)>,
}

impl<FH, D, H> From<FH> for FinalizationHandlerAdapter<FH, D, H> {
    fn from(value: FH) -> Self {
        Self {
            finalization_handler: value,
            _phantom: PhantomData,
        }
    }
}

impl<D: Data, H: Hasher, FH: FinalizationHandler<D>> UnitFinalizationHandler
    for FinalizationHandlerAdapter<FH, D, H>
{
    type Data = D;
    type Hasher = H;

    fn batch_finalized(&mut self, batch: Vec<OrderedUnit<Self::Data, Self::Hasher>>) {
        for unit in batch {
            if let Some(data) = unit.data {
                self.finalization_handler.data_finalized(data)
            }
        }
    }
}

#[derive(Clone)]
pub struct LocalIO<DP: DataProvider, UFH: UnitFinalizationHandler, US: AsyncWrite, UL: AsyncRead> {
    data_provider: DP,
    finalization_handler: UFH,
    unit_saver: US,
    unit_loader: UL,
}

impl<
        H: Hasher,
        DP: DataProvider,
        FH: FinalizationHandler<DP::Output>,
        US: AsyncWrite,
        UL: AsyncRead,
    > LocalIO<DP, FinalizationHandlerAdapter<FH, DP::Output, H>, US, UL>
{
    pub fn new(
        data_provider: DP,
        finalization_handler: FH,
        unit_saver: US,
        unit_loader: UL,
    ) -> Self {
        Self {
            data_provider,
            finalization_handler: finalization_handler.into(),
            unit_saver,
            unit_loader,
        }
    }
}

impl<DP: DataProvider, UFH: UnitFinalizationHandler, US: AsyncWrite, UL: AsyncRead>
    LocalIO<DP, UFH, US, UL>
{
    pub fn new_with_unit_finalization_handler(
        data_provider: DP,
        finalization_handler: UFH,
        unit_saver: US,
        unit_loader: UL,
    ) -> Self {
        Self {
            data_provider,
            finalization_handler,
            unit_saver,
            unit_loader,
        }
    }
}

struct MemberStatus<'a, H: Hasher, D: Data, S: Signature> {
    task_queue: &'a TaskQueue<RepeatableTask<H, D, S>>,
    not_resolved_parents: &'a HashSet<H::Hash>,
    not_resolved_coords: &'a HashSet<UnitCoord>,
}

impl<'a, H: Hasher, D: Data, S: Signature> MemberStatus<'a, H, D, S> {
    fn new(
        task_queue: &'a TaskQueue<RepeatableTask<H, D, S>>,
        not_resolved_parents: &'a HashSet<H::Hash>,
        not_resolved_coords: &'a HashSet<UnitCoord>,
    ) -> Self {
        Self {
            task_queue,
            not_resolved_parents,
            not_resolved_coords,
        }
    }
}

impl<'a, H: Hasher, D: Data, S: Signature> fmt::Display for MemberStatus<'a, H, D, S>
where
    H: Hasher,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use DisseminationTask::*;
        let mut count_coord_request: usize = 0;
        let mut count_parents_request: usize = 0;
        let mut count_rebroadcast: usize = 0;
        for task in self.task_queue.iter().map(|st| &st.task) {
            match task {
                Request(ReconstructionRequest::Coord(_)) => count_coord_request += 1,
                Request(ReconstructionRequest::ParentsOf(_)) => count_parents_request += 1,
                Broadcast(_) => count_rebroadcast += 1,
            }
        }
        let long_time_pending_tasks: Vec<_> = self
            .task_queue
            .iter()
            .filter(|st| st.counter >= 5)
            .collect();
        write!(f, "Member status report: ")?;
        write!(f, "task queue content: ")?;
        write!(
            f,
            "CoordRequest - {}, ParentsRequest - {}, UnitBroadcast - {}",
            count_coord_request, count_parents_request, count_rebroadcast,
        )?;
        if !self.not_resolved_coords.is_empty() {
            write!(
                f,
                "; not_resolved_coords.len() - {}",
                self.not_resolved_coords.len()
            )?;
        }
        if !self.not_resolved_parents.is_empty() {
            write!(
                f,
                "; not_resolved_parents.len() - {}",
                self.not_resolved_parents.len()
            )?;
        }

        static ITEMS_PRINT_LIMIT: usize = 10;

        if !long_time_pending_tasks.is_empty() {
            write!(f, "; pending tasks with counter >= 5 -")?;
            write!(f, " {}", {
                long_time_pending_tasks
                    .iter()
                    .take(ITEMS_PRINT_LIMIT)
                    .join(", ")
            })?;

            if let Some(remaining) = long_time_pending_tasks.len().checked_sub(ITEMS_PRINT_LIMIT) {
                write!(f, " and {remaining} more")?
            }
        }
        write!(f, ".")?;
        Ok(())
    }
}

struct Member<H, D, S>
where
    H: Hasher,
    D: Data,
    S: Signature,
{
    config: Config,
    task_queue: TaskQueue<RepeatableTask<H, D, S>>,
    not_resolved_parents: HashSet<H::Hash>,
    not_resolved_coords: HashSet<UnitCoord>,
    peers: Vec<Recipient>,
    unit_messages_for_network: Sender<(UnitMessage<H, D, S>, Recipient)>,
    unit_messages_from_network: Receiver<UnitMessage<H, D, S>>,
    notifications_for_runway: Sender<DisseminationMessage<H, D, S>>,
    notifications_from_runway: Receiver<RunwayNotificationOut<H, D, S>>,
    resolved_requests: Receiver<ReconstructionRequest<H>>,
    exiting: bool,
    top_units: NodeMap<Round>,
}

impl<H, D, S> Member<H, D, S>
where
    H: Hasher,
    D: Data,
    S: Signature,
{
    fn new(
        config: Config,
        unit_messages_for_network: Sender<(UnitMessage<H, D, S>, Recipient)>,
        unit_messages_from_network: Receiver<UnitMessage<H, D, S>>,
        notifications_for_runway: Sender<DisseminationMessage<H, D, S>>,
        notifications_from_runway: Receiver<RunwayNotificationOut<H, D, S>>,
        resolved_requests: Receiver<ReconstructionRequest<H>>,
    ) -> Self {
        let n_members = config.n_members();
        let peers = (0..n_members.0)
            .map(NodeIndex)
            .filter(|x| *x != config.node_ix())
            .map(Recipient::Node)
            .collect();

        Self {
            config,
            task_queue: TaskQueue::new(),
            not_resolved_parents: HashSet::new(),
            not_resolved_coords: HashSet::new(),
            peers,
            unit_messages_for_network,
            unit_messages_from_network,
            notifications_for_runway,
            notifications_from_runway,
            resolved_requests,
            exiting: false,
            top_units: NodeMap::with_size(n_members),
        }
    }

    fn on_create(&mut self, u: UncheckedSignedUnit<H, D, S>) {
        self.send_unit_message(Addressed::new(
            UnitMessage::Unit(u),
            vec![Recipient::Everyone],
        ));
    }

    fn on_unit_discovered(&mut self, new_unit: UncheckedSignedUnit<H, D, S>) {
        let unit_creator = new_unit.as_signable().creator();
        let unit_round = new_unit.as_signable().round();
        if self
            .top_units
            .get(unit_creator)
            .map(|round| round < &unit_round)
            .unwrap_or(true)
        {
            self.top_units.insert(unit_creator, unit_round);
            let task = RepeatableTask::new(DisseminationTask::Broadcast(new_unit));
            let delay = self.delay(&task.task, task.counter);
            self.task_queue.schedule_in(task, delay)
        }
    }

    fn on_request_coord(&mut self, coord: UnitCoord) {
        trace!(target: "AlephBFT-member", "{:?} Dealing with missing coord notification {:?}.", self.index(), coord);
        if !self.not_resolved_coords.insert(coord) {
            return;
        }

        self.task_queue
            .schedule_now(RepeatableTask::new(DisseminationTask::Request(
                ReconstructionRequest::Coord(coord),
            )));
        self.trigger_tasks();
    }

    fn on_request_parents(&mut self, u_hash: H::Hash) {
        if !self.not_resolved_parents.insert(u_hash) {
            return;
        }

        self.task_queue
            .schedule_now(RepeatableTask::new(DisseminationTask::Request(
                ReconstructionRequest::ParentsOf(u_hash),
            )));
        self.trigger_tasks();
    }

    fn trigger_tasks(&mut self) {
        while let Some(mut task) = self.task_queue.pop_due_task() {
            match self.task_details(&task.task, task.counter) {
                TaskDetails::Cancel => (),
                TaskDetails::Perform {
                    message,
                    recipients,
                    reschedule,
                } => {
                    self.send_unit_message(Addressed::new(message, recipients));

                    task.counter += 1;
                    self.task_queue.schedule_in(task, reschedule)
                }
            }
        }
    }

    fn random_peers(&self, n: usize) -> Vec<Recipient> {
        self.peers
            .choose_multiple(&mut rand::thread_rng(), n)
            .cloned()
            .collect()
    }

    fn index(&self) -> NodeIndex {
        self.config.node_ix()
    }

    fn send_unit_message(&mut self, message: Addressed<UnitMessage<H, D, S>>) {
        for recipient in message.recipients() {
            if self
                .unit_messages_for_network
                .unbounded_send((message.message().clone(), recipient.clone()))
                .is_err()
            {
                warn!(target: "AlephBFT-member", "{:?} Channel to network should be open", self.index());
                self.exiting = true;
            }
        }
    }

    /// Given a task and the number of times it was performed, returns `Cancel` if the task is no longer active,
    /// `Delay(Duration)` if the task is active, but cannot be performed right now, and
    /// `Perform { message, recipient, reschedule }` if the task is to send `message` to `recipient` and it should
    /// be rescheduled after `reschedule`.
    fn task_details(
        &mut self,
        task: &DisseminationTask<H, D, S>,
        counter: usize,
    ) -> TaskDetails<H, D, S> {
        match self.still_valid(task) {
            false => TaskDetails::Cancel,
            true => TaskDetails::Perform {
                message: self.message(task),
                recipients: self.recipients(task, counter),
                reschedule: self.delay(task, counter),
            },
        }
    }

    fn message(&self, task: &DisseminationTask<H, D, S>) -> UnitMessage<H, D, S> {
        use DisseminationTask::*;
        match task {
            Request(request) => {
                DisseminationMessage::Request(self.index(), (*request).clone()).into()
            }
            Broadcast(unit) => DisseminationMessage::Unit(unit.clone()).into(),
        }
    }

    fn recipients(&self, task: &DisseminationTask<H, D, S>, counter: usize) -> Vec<Recipient> {
        use DisseminationTask::*;
        match task {
            Request(ReconstructionRequest::Coord(_)) => {
                self.random_peers((self.config.delay_config().coord_request_recipients)(
                    counter,
                ))
            }
            Request(ReconstructionRequest::ParentsOf(_)) => {
                self.random_peers((self.config.delay_config().parent_request_recipients)(
                    counter,
                ))
            }
            Broadcast(_) => vec![Recipient::Everyone],
        }
    }

    fn still_valid(&self, task: &DisseminationTask<H, D, S>) -> bool {
        use DisseminationTask::*;
        match task {
            Request(ReconstructionRequest::Coord(coord)) => {
                self.not_resolved_coords.contains(coord)
            }
            Request(ReconstructionRequest::ParentsOf(hash)) => {
                self.not_resolved_parents.contains(hash)
            }
            Broadcast(unit) => {
                Some(&unit.as_signable().round())
                    == self.top_units.get(unit.as_signable().creator())
            }
        }
    }

    /// Most tasks use `requests_interval` (see [crate::DelayConfig]) as their delay.
    ///
    /// The first exception is [Task::UnitBroadcast] - this one picks a random delay between
    /// `unit_rebroadcast_interval_min` and `unit_rebroadcast_interval_max`.
    ///
    /// The other exception is [Task::CoordRequest] - this one uses the configurable
    /// `coord_request_delay` schedule.
    fn delay(&self, task: &DisseminationTask<H, D, S>, counter: usize) -> Duration {
        use DisseminationTask::*;
        match task {
            Broadcast(_) => {
                let low = self.config.delay_config().unit_rebroadcast_interval_min;
                let high = self.config.delay_config().unit_rebroadcast_interval_max;
                let millis = rand::thread_rng().gen_range(low.as_millis()..high.as_millis());
                Duration::from_millis(millis as u64)
            }
            Request(ReconstructionRequest::Coord(_)) => {
                (self.config.delay_config().coord_request_delay)(counter)
            }
            Request(ReconstructionRequest::ParentsOf(_)) => {
                (self.config.delay_config().parent_request_delay)(counter)
            }
        }
    }

    fn on_unit_message_from_units(&mut self, message: RunwayNotificationOut<H, D, S>) {
        use ReconstructionRequest::*;
        use RunwayNotificationOut::*;
        match message {
            NewSelfUnit(u) => self.on_create(u),
            NewAnyUnit(u) => self.on_unit_discovered(u),
            Request(request) => match request {
                Coord(coord) => self.on_request_coord(coord),
                ParentsOf(u_hash) => self.on_request_parents(u_hash),
            },
            Message(message) => self.send_unit_message(message.into()),
        }
    }

    fn status_report(&self) {
        let status = MemberStatus::new(
            &self.task_queue,
            &self.not_resolved_parents,
            &self.not_resolved_coords,
        );
        info!(target: "AlephBFT-member", "{}", status);
    }

    async fn run(mut self, mut terminator: Terminator) {
        let ticker_delay = self.config.delay_config().tick_interval;
        let mut ticker = Delay::new(ticker_delay).fuse();
        let status_ticker_delay = Duration::from_secs(10);
        let mut status_ticker = Delay::new(status_ticker_delay).fuse();

        loop {
            futures::select! {
                event = self.notifications_from_runway.next() => match event {
                    Some(message) => {
                        self.on_unit_message_from_units(message);
                    },
                    None => {
                        error!(target: "AlephBFT-member", "{:?} Unit message stream from Runway closed.", self.index());
                        break;
                    },
                },

                event = self.resolved_requests.next() => match event {
                    Some(request) => match request {
                        ReconstructionRequest::Coord(coord) => {
                            self.not_resolved_coords.remove(&coord);
                        },
                        ReconstructionRequest::ParentsOf(u_hash) => {
                            self.not_resolved_parents.remove(&u_hash);
                        },
                    },
                    None => {
                        error!(target: "AlephBFT-member", "{:?} Resolved-requests stream from Runway closed.", self.index());
                        break;
                    }
                },

                event = self.unit_messages_from_network.next() => match event {
                    Some(message) => {
                        self.send_notification_to_runway(message.into())
                    },
                    None => {
                        error!(target: "AlephBFT-member", "{:?} Unit message stream from network closed.", self.index());
                        break;
                    },
                },

                _ = &mut ticker => {
                    self.trigger_tasks();
                    ticker = Delay::new(ticker_delay).fuse();
                },

                _ = &mut status_ticker => {
                    self.status_report();
                    status_ticker = Delay::new(status_ticker_delay).fuse();
                },

                _ = terminator.get_exit().fuse() => {
                    debug!(target: "AlephBFT-member", "{:?} received exit signal", self.index());
                    self.exiting = true;
                },
            }
            if self.exiting {
                debug!(target: "AlephBFT-member", "{:?} Member decided to exit.", self.index());
                terminator.terminate_sync().await;
                break;
            }
        }

        debug!(target: "AlephBFT-member", "{:?} Member stopped.", self.index());
    }

    fn send_notification_to_runway(&mut self, notification: DisseminationMessage<H, D, S>) {
        if self
            .notifications_for_runway
            .unbounded_send(notification)
            .is_err()
        {
            warn!(target: "AlephBFT-member", "{:?} Sender to runway with DisseminationMessage messages should be open", self.index());
            self.exiting = true;
        }
    }
}

/// Starts the consensus algorithm as an async task. It stops establishing consensus for new data items after
/// reaching the threshold specified in [`Config::max_round`] or upon receiving a stop signal from `exit`.
/// For a detailed description of the consensus implemented by `run_session` see
/// [docs for devs](https://cardinal-cryptography.github.io/AlephBFT/index.html)
/// or the [original paper](https://arxiv.org/abs/1908.05156).
///
/// Please note that in order to fulfill the constraint [`UnitFinalizationHandler<Data = DP::Output, Hasher
/// = H>`] it is enough to provide implementation of [`FinalizationHandler<DP::Output>`]. We provide
/// implementation of [`UnitFinalizationHandler<Data = DP::Output, Hasher = H>`] for anything that satisfies
/// the trait [`FinalizationHandler<DP::Output>`] (by means of [`FinalizationHandlerAdapter`]). Implementing
/// [`UnitFinalizationHandler`] directly is considered less stable since it exposes intrisics which might be
/// subject to change. Implement [`FinalizationHandler<DP::Output>`] instead, unless you absolutely know
/// what you are doing.
pub async fn run_session<
    DP: DataProvider,
    UFH: UnitFinalizationHandler<Data = DP::Output>,
    US: AsyncWrite + Send + Sync + 'static,
    UL: AsyncRead + Send + Sync + 'static,
    N: Network<NetworkData<UFH::Hasher, DP::Output, MK::Signature, MK::PartialMultisignature>>,
    SH: SpawnHandle,
    MK: MultiKeychain,
>(
    config: Config,
    local_io: LocalIO<DP, UFH, US, UL>,
    network: N,
    keychain: MK,
    spawn_handle: SH,
    mut terminator: Terminator,
) {
    let index = config.node_ix();
    info!(target: "AlephBFT-member", "{:?} Starting a new session.", index);
    debug!(target: "AlephBFT-member", "{:?} Spawning party for a session.", index);

    let (alert_messages_for_alerter, alert_messages_from_network) = mpsc::unbounded();
    let (alert_messages_for_network, alert_messages_from_alerter) = mpsc::unbounded();
    let (unit_messages_for_units, unit_messages_from_network) = mpsc::unbounded();
    let (unit_messages_for_network, unit_messages_from_units) = mpsc::unbounded();
    let (runway_messages_for_runway, runway_messages_from_network) = mpsc::unbounded();
    let (runway_messages_for_network, runway_messages_from_runway) = mpsc::unbounded();
    let (resolved_requests_tx, resolved_requests_rx) = mpsc::unbounded();

    debug!(target: "AlephBFT-member", "{:?} Spawning network.", index);
    let network_terminator = terminator.add_offspring_connection("AlephBFT-network");

    let network_handle = spawn_handle
        .spawn_essential("member/network", async move {
            NetworkHub::new(
                network,
                unit_messages_from_units,
                unit_messages_for_units,
                alert_messages_from_alerter,
                alert_messages_for_alerter,
            )
            .run(network_terminator)
            .await
        })
        .fuse();
    pin_mut!(network_handle);
    debug!(target: "AlephBFT-member", "{:?} Network spawned.", index);

    debug!(target: "AlephBFT-member", "{:?} Initializing Runway.", index);
    let runway_terminator = terminator.add_offspring_connection("AlephBFT-runway");
    let network_io = NetworkIO {
        alert_messages_for_network,
        alert_messages_from_network,
        unit_messages_from_network: runway_messages_from_network,
        unit_messages_for_network: runway_messages_for_network,
        resolved_requests: resolved_requests_tx,
    };
    let runway_io = RunwayIO::new(
        local_io.data_provider,
        local_io.finalization_handler,
        local_io.unit_saver,
        local_io.unit_loader,
    );
    let spawn_copy = spawn_handle.clone();
    let config_copy = config.clone();
    let runway_handle = spawn_handle
        .spawn_essential("member/runway", async move {
            runway::run(
                config_copy,
                runway_io,
                keychain.clone(),
                spawn_copy,
                network_io,
                runway_terminator,
            )
            .await
        })
        .fuse();
    pin_mut!(runway_handle);
    debug!(target: "AlephBFT-member", "{:?} Runway spawned.", index);

    debug!(target: "AlephBFT-member", "{:?} Initializing Member.", index);
    let member = Member::new(
        config,
        unit_messages_for_network,
        unit_messages_from_network,
        runway_messages_for_runway,
        runway_messages_from_runway,
        resolved_requests_rx,
    );
    let member_terminator = terminator.add_offspring_connection("AlephBFT-member");
    let member_handle = spawn_handle
        .spawn_essential("member", async move {
            member.run(member_terminator).await;
        })
        .fuse();
    pin_mut!(member_handle);
    debug!(target: "AlephBFT-member", "{:?} Member initialized.", index);

    futures::select! {
        _ = network_handle => {
            error!(target: "AlephBFT-member", "{:?} Network-hub terminated early.", index);
        },

        _ = runway_handle => {
            error!(target: "AlephBFT-member", "{:?} Runway terminated early.", index);
        },

        _ = member_handle => {
            error!(target: "AlephBFT-member", "{:?} Member terminated early.", index);
        },

        _ = terminator.get_exit().fuse() => {
            debug!(target: "AlephBFT-member", "{:?} exit channel was called.", index);
        },
    }

    debug!(target: "AlephBFT-member", "{:?} Run ending.", index);

    terminator.terminate_sync().await;

    handle_task_termination(network_handle, "AlephBFT-member", "Network", index).await;
    handle_task_termination(runway_handle, "AlephBFT-member", "Runway", index).await;
    handle_task_termination(member_handle, "AlephBFT-member", "Member", index).await;

    info!(target: "AlephBFT-member", "{:?} Session ended.", index);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        testing::{gen_config, gen_delay_config},
        DelayConfig,
    };
    use aleph_bft_mock::{Hasher64, Signature};
    use aleph_bft_types::NodeCount;
    use futures::channel::mpsc::unbounded;
    use itertools::Itertools;
    use std::sync::Arc;

    fn mock_member(
        node_ix: NodeIndex,
        node_count: NodeCount,
        delay_config: DelayConfig,
    ) -> Member<Hasher64, u32, Signature> {
        let config = gen_config(node_ix, node_count, delay_config);
        let (unit_messages_for_network_sx, _) = unbounded();
        let (_, unit_messages_from_network_rx) = unbounded();
        let (notifications_for_runway_sx, _) = unbounded();
        let (_, notifications_from_runway_rx) = unbounded();
        let (_, resolved_requests_rx) = unbounded();

        Member::new(
            config,
            unit_messages_for_network_sx,
            unit_messages_from_network_rx,
            notifications_for_runway_sx,
            notifications_from_runway_rx,
            resolved_requests_rx,
        )
    }

    #[test]
    fn delay_for_coord_request() {
        let mut delay_config = gen_delay_config();
        delay_config.coord_request_delay = Arc::new(|t| Duration::from_millis(123 + t as u64));

        let member = mock_member(NodeIndex(7), NodeCount(20), delay_config);

        let delay = member.delay(
            &DisseminationTask::Request(ReconstructionRequest::Coord(UnitCoord::new(
                1,
                NodeIndex(3),
            ))),
            10,
        );

        assert_eq!(delay, Duration::from_millis(133));
    }

    #[test]
    fn delay_for_parent_request() {
        let mut delay_config = gen_delay_config();
        delay_config.parent_request_delay = Arc::new(|t| Duration::from_millis(123 + t as u64));

        let member = mock_member(NodeIndex(7), NodeCount(20), delay_config);

        let delay = member.delay(
            &DisseminationTask::Request(ReconstructionRequest::ParentsOf(Hasher64::hash(&[0x0]))),
            10,
        );

        assert_eq!(delay, Duration::from_millis(133));
    }

    #[test]
    fn recipients_for_coord_request() {
        let node_ix = NodeIndex(7);
        let mut delay_config = gen_delay_config();
        delay_config.coord_request_recipients = Arc::new(|t| 10 - t);

        let member = mock_member(node_ix, NodeCount(20), delay_config);

        let request = DisseminationTask::Request(ReconstructionRequest::Coord(UnitCoord::new(
            1,
            NodeIndex(3),
        )));
        let recipients = member.recipients(&request, 3);

        assert_eq!(recipients.len(), 7);
        assert_eq!(
            recipients.iter().cloned().unique().collect::<Vec<_>>(),
            recipients
        );
        assert!(!recipients.contains(&Recipient::Node(node_ix)));
    }

    #[test]
    fn recipients_for_parent_request() {
        let node_ix = NodeIndex(7);
        let mut delay_config = gen_delay_config();
        delay_config.parent_request_recipients = Arc::new(|t| 10 - t);

        let member = mock_member(node_ix, NodeCount(20), delay_config);

        let request =
            DisseminationTask::Request(ReconstructionRequest::ParentsOf(Hasher64::hash(&[0x0])));
        let recipients = member.recipients(&request, 3);

        assert_eq!(recipients.len(), 7);
        assert_eq!(
            recipients.iter().cloned().unique().collect::<Vec<_>>(),
            recipients
        );
        assert!(!recipients.contains(&Recipient::Node(node_ix)));
    }

    #[test]
    fn at_most_n_members_recipients_for_coord_request() {
        let mut delay_config = gen_delay_config();
        delay_config.coord_request_recipients = Arc::new(move |_| 30);

        let member = mock_member(NodeIndex(7), NodeCount(20), delay_config);

        let request = DisseminationTask::Request(ReconstructionRequest::Coord(UnitCoord::new(
            1,
            NodeIndex(3),
        )));
        let recipients = member.recipients(&request, 10);

        assert_eq!(recipients.len(), member.config.n_members().0 - 1);
    }

    #[test]
    fn no_recipients_for_coord_request_in_one_node_setup() {
        let mut delay_config = gen_delay_config();
        delay_config.coord_request_recipients = Arc::new(move |_| 30);

        let member = mock_member(NodeIndex(0), NodeCount(1), delay_config);

        let request = DisseminationTask::Request(ReconstructionRequest::Coord(UnitCoord::new(
            1,
            NodeIndex(3),
        )));
        let recipients = member.recipients(&request, 10);

        assert_eq!(recipients, vec![]);
    }
}
