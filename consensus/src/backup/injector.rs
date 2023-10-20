use crate::{
    alerts::{AlertData, Error as AlerterError, ForkingNotification, Handler as AlerterHandler},
    backup::BackupItem,
    runway::Runway,
    units::{SignedUnit, UncheckedSignedUnit, UnitCoord},
    Data, Hasher, MultiKeychain, Multisigned, SessionId,
};
use aleph_bft_rmc::{
    DoublingDelayScheduler, Error as RmcError, Handler as RmcHandler, Message as RmcMessage,
    OnStartRmcResponse,
};
use aleph_bft_types::FinalizationHandler;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    fmt::{Display, Formatter},
    marker::PhantomData,
    time::Duration,
};

#[derive(Debug)]
pub enum Error {
    WrongSession(SessionId, SessionId),
    ParentMissing,
    ForkInUnitBackup,
    BadUnit,
    BadAlert,
    AlerterHandler(AlerterError),
    RmcHandler(RmcError),
}

impl From<AlerterError> for Error {
    fn from(error: AlerterError) -> Self {
        Error::AlerterHandler(error)
    }
}

impl From<RmcError> for Error {
    fn from(error: RmcError) -> Self {
        Error::RmcHandler(error)
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Error::WrongSession(actual, expected) => write!(
                f,
                "found unit of wrong session ({:?}) in backup for session {:?}.",
                actual, expected
            ),
            Error::ParentMissing => write!(f, "parent of a unit missing from the backup."),
            Error::ForkInUnitBackup => write!(
                f,
                "forking units not coming from alerts found in the backup."
            ),
            Error::BadUnit => write!(f, "wrongly signed unit found in the backup."),
            Error::BadAlert => write!(f, "wrongly signed alert found in the backup."),
            Error::AlerterHandler(e) => write!(f, "error in alerter handler: {:?}.", e),
            Error::RmcHandler(e) => write!(f, "error in rmc handler: {:?}.", e),
        }
    }
}

pub struct InitialState<H: Hasher, D: Data, MK: MultiKeychain> {
    pub alerter_handler: AlerterHandler<H, D, MK>,
    pub rmc_handler: RmcHandler<H::Hash, MK>,
    pub forking_notifications: Vec<ForkingNotification<H, D, MK::Signature>>,
    pub scheduler:
        DoublingDelayScheduler<RmcMessage<H::Hash, MK::Signature, MK::PartialMultisignature>>,
}

pub struct BackupInjector<H: Hasher, D: Data, MK: MultiKeychain> {
    session_id: SessionId,
    keychain: MK,
    dag: HashSet<UnitCoord>,
    _phantom: PhantomData<(H, D)>,
}

impl<H: Hasher, D: Data, MK: MultiKeychain> BackupInjector<H, D, MK> {
    pub fn new(session_id: SessionId, keychain: MK) -> Self {
        BackupInjector {
            session_id,
            keychain,
            dag: HashSet::new(),
            _phantom: PhantomData,
        }
    }

    fn verify_unit_parents(
        &self,
        unit: &UncheckedSignedUnit<H, D, MK::Signature>,
    ) -> Result<(), Error> {
        let full_unit = unit.as_signable();
        let coord = full_unit.coord();
        if full_unit.session_id() != self.session_id {
            return Err(Error::WrongSession(full_unit.session_id(), self.session_id));
        }
        let parent_ids = &full_unit.as_pre_unit().control_hash().parents_mask;
        for parent_id in parent_ids.elements() {
            let parent = UnitCoord::new(coord.round() - 1, parent_id);
            if !self.dag.contains(&parent) {
                return Err(Error::ParentMissing);
            }
        }
        Ok(())
    }

    fn handle_multisigned(
        &self,
        multisigned: Multisigned<H::Hash, MK>,
        alerter_handler: &mut AlerterHandler<H, D, MK>,
    ) -> Result<Vec<SignedUnit<H, D, MK>>, Error> {
        let units = alerter_handler.alert_confirmed(multisigned)?;
        let mut signed_units = vec![];
        for unit in units {
            // we don't verify parenthood as the units may come from the future
            let signed_unit = unit.check(&self.keychain).map_err(|_| Error::BadUnit)?;
            signed_units.push(signed_unit);
        }
        Ok(signed_units)
    }

    pub fn get_initial_state<FH: FinalizationHandler<D>>(
        mut self,
        backup_data: Vec<BackupItem<H, D, MK>>,
        runway: &mut Runway<H, D, FH, MK>,
    ) -> Result<InitialState<H, D, MK>, Error> {
        let mut alerter_handler = AlerterHandler::new(self.keychain.clone(), self.session_id);
        let mut rmc_handler = RmcHandler::new(self.keychain.clone());
        let mut fork_proofs = HashMap::new();
        let mut rmcs = HashSet::new();

        for item in backup_data {
            match item {
                BackupItem::Unit(unit) => {
                    self.verify_unit_parents(&unit)?;
                    let signed_unit = unit.check(&self.keychain).map_err(|_| Error::BadUnit)?;

                    // in self.dag we store units which were saved as the Unit variant of BackupData
                    // we require that there is no fork in this data, as all of the forking units
                    // are assumed to be stored as legit units of backed up alerts
                    let coord = signed_unit.as_signable().coord();
                    if self.dag.contains(&coord) {
                        return Err(Error::ForkInUnitBackup);
                    }

                    runway.add_unit(signed_unit, false); // synchronous simulation
                    self.dag.insert(coord);
                }
                BackupItem::AlertData(AlertData::OwnAlert(alert)) => {
                    let alert = alert
                        .check(&self.keychain)
                        .map_err(|_| Error::BadAlert)?
                        .into_signable();

                    // synchronous simulation
                    let hash = alert.hash();
                    let forker = alert.forker();
                    let _ = alerter_handler.on_own_alert(alert);
                    runway.mark_forker(forker);

                    // we don't want to send the notification to runway as it would create
                    // a duplicate alert about the forker and pass it to the alerter
                    fork_proofs.remove(&forker);

                    // we may want to start rmc at the end, if we don't encounter the
                    // corresponding multisignature in the backup
                    rmcs.insert(hash);
                }
                BackupItem::AlertData(AlertData::NetworkAlert(alert)) => {
                    let forker = alert.as_signable().forker();
                    if let Ok((Some(proof), hash)) = alerter_handler.on_network_alert(alert)
                    // synchronous simulation
                    {
                        // runway has to know the forker from its storage
                        runway.mark_forker(forker);

                        // we may want to pass this proof to runway after the
                        // simulation so that it creates an appropriate alert and
                        // passes it to the alerter, but only if we can't find
                        // an alert corresponding to this forker in the backup
                        fork_proofs.insert(forker, proof);

                        // similarly, we may need to start rmc on the hash
                        rmcs.insert(hash);
                    }
                }
                BackupItem::AlertData(AlertData::MultisignedHash(multisigned)) => {
                    rmc_handler.on_multisigned_hash(multisigned.clone().into_unchecked())?;

                    let legit_units =
                        self.handle_multisigned(multisigned.clone(), &mut alerter_handler)?;
                    for unit in legit_units {
                        runway.add_unit(unit, true);
                    }

                    // we don't need to start rmc on this hash as we already have the multisignature
                    rmcs.remove(multisigned.as_signable());
                }
            }
        }

        let mut tasks = vec![];
        for hash in rmcs {
            match rmc_handler.on_start_rmc(hash) {
                OnStartRmcResponse::SignedHash(signed) => {
                    tasks.push(RmcMessage::SignedHash(signed.into_unchecked()));
                }
                OnStartRmcResponse::MultisignedHash(multisigned) => {
                    tasks.push(RmcMessage::MultisignedHash(
                        multisigned.clone().into_unchecked(),
                    ));
                    let legit_units = self.handle_multisigned(multisigned, &mut alerter_handler)?;
                    for unit in legit_units {
                        runway.add_unit(unit, true);
                    }
                }
                _ => {}
            }
        }

        let scheduler = DoublingDelayScheduler::with_tasks(tasks, Duration::from_millis(500));

        Ok(InitialState {
            alerter_handler,
            rmc_handler,
            forking_notifications: fork_proofs.into_values().collect(),
            scheduler,
        })
    }
}
