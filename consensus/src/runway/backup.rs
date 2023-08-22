use crate::{
    alerts::Alert,
    units::{UncheckedSignedUnit, UnitCoord},
    Data, Hasher, MultiKeychain, Multisigned, NodeIndex, Receiver, Round, Sender, SessionId,
    Terminator,
};

use codec::{Decode, Encode, Error as CodecError};
use futures::{channel::oneshot, FutureExt, StreamExt};
use log::{debug, error, info, warn};
use std::{
    collections::HashSet,
    fmt,
    fmt::Debug,
    io::{Read, Write},
    marker::PhantomData,
};

const LOG_TARGET: &str = "AlephBFT-backup";

#[derive(Clone, Debug, Decode, Encode, PartialEq)]
pub enum BackupItem<H: Hasher, D: Data, MK: MultiKeychain> {
    Unit(UncheckedSignedUnit<H, D, MK::Signature>),
    AlertData(AlertData<H, D, MK>),
}

#[derive(Clone, Debug, Decode, Encode, PartialEq)]
pub enum AlertData<H: Hasher, D: Data, MK: MultiKeychain> {
    OwnAlert(Alert<H, D, MK::Signature>),
    NetworkAlert(Alert<H, D, MK::Signature>),
    MultisignedHash(Multisigned<H::Hash, MK>),
}

/// Backup load error. Could be either caused by io error from Reader, or by decoding.
#[derive(Debug)]
pub enum BackupReadError {
    IO(std::io::Error),
    Codec(CodecError),
    InconsistentData(UnitCoord),
    WrongSession(UnitCoord, SessionId, SessionId),
}

impl fmt::Display for BackupReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupReadError::IO(err) => {
                write!(
                    f,
                    "received IO error while reading from backup source: {}",
                    err
                )
            }
            BackupReadError::Codec(err) => {
                write!(f, "received Codec error while decoding backup: {}", err)
            }
            BackupReadError::InconsistentData(coord) => {
                write!(
                    f,
                    "inconsistent backup data. Unit from round {:?} of creator {:?} is missing a parent in backup.",
                    coord.round(), coord.creator()
                )
            }
            BackupReadError::WrongSession(coord, expected_session, actual_session) => {
                write!(
                    f,
                    "unit from round {:?} of creator {:?} has a wrong session id in backup. Expected: {:?} got: {:?}",
                    coord.round(), coord.creator(), expected_session, actual_session
                )
            }
        }
    }
}

impl From<std::io::Error> for BackupReadError {
    fn from(err: std::io::Error) -> Self {
        Self::IO(err)
    }
}

impl From<CodecError> for BackupReadError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

/// Abstraction over backup writing mechanism
pub struct BackupWriter<W: Write, H: Hasher, D: Data, MK: MultiKeychain> {
    inner: W,
    _phantom: PhantomData<(H, D, MK)>,
}

/// Abstraction over backup reading mechanism
pub struct BackupReader<R: Read, H: Hasher, D: Data, MK: MultiKeychain> {
    inner: R,
    _phantom: PhantomData<(H, D, MK)>,
}

impl<W: Write, H: Hasher, D: Data, MK: MultiKeychain> BackupWriter<W, H, D, MK> {
    pub fn new(write: W) -> Self {
        Self {
            inner: write,
            _phantom: PhantomData,
        }
    }

    pub fn save(&mut self, item: BackupItem<H, D, MK>) -> Result<(), std::io::Error> {
        self.inner.write_all(&item.encode())?;
        self.inner.flush()?;
        Ok(())
    }
}

impl<R: Read, H: Hasher, D: Data, MK: MultiKeychain> BackupReader<R, H, D, MK> {
    pub fn new(read: R) -> Self {
        Self {
            inner: read,
            _phantom: PhantomData,
        }
    }

    fn load(mut self) -> Result<Vec<BackupItem<H, D, MK>>, BackupReadError> {
        let mut buf = Vec::new();
        self.inner.read_to_end(&mut buf)?;
        let input = &mut &buf[..];
        let mut result = Vec::new();
        while !input.is_empty() {
            result.push(<BackupItem<H, D, MK>>::decode(input)?);
        }
        Ok(result)
    }
}

fn load_backup<H: Hasher, D: Data, MK: MultiKeychain, R: Read>(
    backup_reader: BackupReader<R, H, D, MK>,
    session_id: SessionId,
) -> Result<Vec<BackupItem<H, D, MK>>, BackupReadError> {
    // TODO(A0-544): perform some processing of alerts and multisignatures
    let loaded_items: Vec<_> = backup_reader.load()?;
    let units = loaded_items.iter().filter_map(|item| match item {
        BackupItem::Unit(unit) => Some(unit),
        _ => None,
    });
    let mut already_loaded_coords = HashSet::new();

    for unit in units {
        let full_unit = unit.as_signable();
        let coord = full_unit.coord();

        if full_unit.session_id() != session_id {
            return Err(BackupReadError::WrongSession(
                coord,
                session_id,
                full_unit.session_id(),
            ));
        }

        let parent_ids = &full_unit.as_pre_unit().control_hash().parents_mask;

        // Sanity check: verify that all unit's parents appeared in backup before it.
        for parent_id in parent_ids.elements() {
            let parent = UnitCoord::new(coord.round() - 1, parent_id);
            if !already_loaded_coords.contains(&parent) {
                return Err(BackupReadError::InconsistentData(coord));
            }
        }

        already_loaded_coords.insert(coord);
    }

    Ok(loaded_items)
}

fn on_shutdown(starting_round_tx: oneshot::Sender<Option<Round>>) {
    if starting_round_tx.send(None).is_err() {
        warn!(target: LOG_TARGET, "Could not send `None` starting round.");
    }
}

/// Loads backup data from `backup_reader` and awaits on response from unit collection.
/// It sends all loaded items by `loaded_items_tx`.
/// If loaded units are compatible with the unit collection result (meaning the highest unit is from at least
/// round from unit collection + 1) it sends `Some(starting_round)` by
/// `starting_round_tx`. If items are not compatible it sends `None` by `starting_round_tx`
pub async fn run_loading_mechanism<'a, H: Hasher, D: Data, MK: MultiKeychain, R: Read>(
    backup_reader: BackupReader<R, H, D, MK>,
    index: NodeIndex,
    session_id: SessionId,
    loaded_items_tx: oneshot::Sender<Vec<BackupItem<H, D, MK>>>,
    starting_round_tx: oneshot::Sender<Option<Round>>,
    next_round_collection_rx: oneshot::Receiver<Round>,
) {
    // TODO(A0-544): perform some processing of alerts and multisignatures
    let items = match load_backup(backup_reader, session_id) {
        Ok(items) => items,
        Err(e) => {
            error!(target: LOG_TARGET, "unable to load backup data: {}", e);
            on_shutdown(starting_round_tx);
            return;
        }
    };
    let units: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            BackupItem::Unit(unit) => Some(unit),
            _ => None,
        })
        .collect();

    let next_round_backup: Round = units
        .iter()
        .filter(|u| u.as_signable().creator() == index)
        .map(|u| u.as_signable().round())
        .max()
        .map(|round| round + 1)
        .unwrap_or(0);

    info!(
        target: LOG_TARGET,
        "Loaded {:?} units from backup. Able to continue from round: {:?}.",
        units.len(),
        next_round_backup
    );

    if loaded_items_tx.send(items).is_err() {
        error!(target: LOG_TARGET, "Could not send loaded items");
        on_shutdown(starting_round_tx);
        return;
    }

    let next_round_collection = match next_round_collection_rx.await {
        Ok(round) => round,
        Err(e) => {
            error!(
                target: LOG_TARGET,
                "Unable to receive response from unit collection: {}", e
            );
            on_shutdown(starting_round_tx);
            return;
        }
    };

    info!(
        target: LOG_TARGET,
        "Next round inferred from collection: {:?}", next_round_collection
    );

    if next_round_backup < next_round_collection {
        // Our newest unit doesn't appear in the backup. This indicates a serious issue, for example
        // a different node running with the same pair of keys. It's safer not to continue.
        error!(
            target: LOG_TARGET, "Backup state behind unit collection state. Next round inferred from: collection: {:?}, backup: {:?}",
            next_round_collection,
            next_round_backup,
        );
        on_shutdown(starting_round_tx);
        return;
    };

    if next_round_collection < next_round_backup {
        // Our newest unit didn't reach any peer, but it resides in our backup. One possible reason
        // is that our node was taken down after saving the unit, but before broadcasting it.
        warn!(
            target: LOG_TARGET, "Backup state ahead of than unit collection state. Next round inferred from: collection: {:?}, backup: {:?}",
            next_round_backup,
            next_round_collection
        );
    }

    if let Err(e) = starting_round_tx.send(Some(next_round_backup)) {
        error!(target: LOG_TARGET, "Could not send starting round: {:?}", e);
    }
}

/// A task responsible for saving units and alert data into backup.
/// It waits for items to appear in `incoming_backup_items`, and writes them to backup.
/// It announces a successful write through `outgoing_units_for_runway` or `outgoing_items_for_alerter`.
pub async fn run_saving_mechanism<'a, H: Hasher, D: Data, MK: MultiKeychain, W: Write>(
    mut backup_writer: BackupWriter<W, H, D, MK>,
    mut incoming_backup_items: Receiver<BackupItem<H, D, MK>>,
    outgoing_units_for_runway: Sender<UncheckedSignedUnit<H, D, MK::Signature>>,
    outgoing_items_for_alerter: Sender<AlertData<H, D, MK>>,
    mut terminator: Terminator,
) {
    let mut terminator_exit = false;
    loop {
        futures::select! {
            item_to_save = incoming_backup_items.next() => {
                let item_to_save = match item_to_save {
                    Some(item) => item,
                    None => {
                        error!(target: LOG_TARGET, "Receiver of items to save closed early");
                        break;
                    },
                };
                if let Err(e) = backup_writer.save(item_to_save.clone()) {
                    error!(target: LOG_TARGET, "Couldn't save item to backup: {:?}", e);
                    break;
                }
                match item_to_save {
                    BackupItem::Unit(unit) => {
                        if outgoing_units_for_runway.unbounded_send(unit).is_err() {
                            error!(target: LOG_TARGET, "Couldn't respond with saved unit to runway");
                            break;
                        }
                    },
                    BackupItem::AlertData(item) => {
                        if outgoing_items_for_alerter.unbounded_send(item).is_err() {
                            error!(target: LOG_TARGET, "Couldn't respond with saved item to alerter");
                            break;
                        }
                    },
                };
            },
            _ = terminator.get_exit().fuse() => {
                debug!(target: LOG_TARGET, "Backup saver received exit signal.");
                terminator_exit = true;
            }
        }

        if terminator_exit {
            debug!(target: LOG_TARGET, "Backup saver decided to exit.");
            terminator.terminate_sync().await;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{run_loading_mechanism, BackupReader};
    use crate::{
        runway::backup::BackupItem,
        units::{
            create_units, creator_set, preunit_to_unchecked_signed_unit, preunit_to_unit,
            UncheckedSignedUnit as GenericUncheckedSignedUnit,
        },
        NodeCount, NodeIndex, Round, SessionId,
    };
    use aleph_bft_mock::{Data, Hasher64, Keychain, Loader, Signature};
    use codec::Encode;
    use futures::channel::oneshot;

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;
    type TestBackupItem = BackupItem<Hasher64, Data, Keychain>;

    const SESSION_ID: SessionId = 43;
    const NODE_ID: NodeIndex = NodeIndex(0);
    const N_MEMBERS: NodeCount = NodeCount(4);

    fn produce_units(rounds: usize, session_id: SessionId) -> Vec<Vec<UncheckedSignedUnit>> {
        let mut creators = creator_set(N_MEMBERS);
        let keychains: Vec<_> = (0..N_MEMBERS.0)
            .map(|id| Keychain::new(N_MEMBERS, NodeIndex(id)))
            .collect();

        let mut units_per_round = Vec::with_capacity(rounds);

        for round in 0..rounds {
            let pre_units = create_units(creators.iter(), round as Round);

            let units: Vec<_> = pre_units
                .iter()
                .map(|(pre_unit, _)| preunit_to_unit(pre_unit.clone(), session_id))
                .collect();
            for creator in creators.iter_mut() {
                creator.add_units(&units);
            }

            let mut unchecked_signed_units = Vec::with_capacity(pre_units.len());
            for ((pre_unit, _), keychain) in pre_units.into_iter().zip(keychains.iter()) {
                unchecked_signed_units.push(preunit_to_unchecked_signed_unit(
                    pre_unit, session_id, keychain,
                ))
            }

            units_per_round.push(unchecked_signed_units);
        }

        // units_per_round[i][j] is the unit produced in round i by creator j
        units_per_round
    }

    fn units_of_creator(
        units: Vec<Vec<UncheckedSignedUnit>>,
        creator: NodeIndex,
    ) -> Vec<UncheckedSignedUnit> {
        units
            .into_iter()
            .map(|units_per_round| units_per_round[creator.0].clone())
            .collect()
    }

    fn encode_all(items: Vec<TestBackupItem>) -> Vec<Vec<u8>> {
        items.iter().map(|u| u.encode()).collect()
    }

    fn prepare_test(
        encoded_items: Vec<u8>,
    ) -> (
        impl futures::Future,
        oneshot::Receiver<Vec<TestBackupItem>>,
        oneshot::Sender<Round>,
        oneshot::Receiver<Option<Round>>,
    ) {
        let backup_reader = BackupReader::new(Loader::new(encoded_items));
        let (loaded_items_tx, loaded_items_rx) = oneshot::channel();
        let (starting_round_tx, starting_round_rx) = oneshot::channel();
        let (highest_response_tx, highest_response_rx) = oneshot::channel();

        (
            run_loading_mechanism(
                backup_reader,
                NODE_ID,
                SESSION_ID,
                loaded_items_tx,
                starting_round_tx,
                highest_response_rx,
            ),
            loaded_items_rx,
            highest_response_tx,
            starting_round_rx,
        )
    }

    #[tokio::test]
    async fn nothing_loaded_nothing_collected_succeeds() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(Vec::new());

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(0)));
        assert_eq!(loaded_unit_rx.await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn something_loaded_nothing_collected_succeeds() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_unit_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn something_loaded_something_collected_succeeds() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(5).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_unit_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn nothing_loaded_something_collected_fails() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(Vec::new());

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(1).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn loaded_smaller_then_collected_fails() {
        let units: Vec<_> = produce_units(3, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(4).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn dropped_collection_fails() {
        let units: Vec<_> = produce_units(3, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        drop(highest_response_tx);
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn backup_with_corrupted_encoding_fails() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let mut item_encodings = encode_all(items);
        let unit2_encoding_len = item_encodings[2].len();
        item_encodings[2].resize(unit2_encoding_len - 1, 0); // remove the last byte
        let encoded_items = item_encodings.into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);
        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn backup_with_missing_parent_fails() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let mut items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        items.remove(2); // it is a parent of all units of round 3
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);
        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn backup_with_duplicate_unit_succeeds() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let mut items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let item2_duplicate = items[2].clone();
        items.insert(3, item2_duplicate);
        let encoded_units = encode_all(items.clone()).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_units);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_unit_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn backup_with_units_of_one_creator_fails() {
        let units = units_of_creator(produce_units(5, SESSION_ID), NodeIndex(NODE_ID.0 + 1));
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn backup_with_wrong_session_fails() {
        let units: Vec<_> = produce_units(5, SESSION_ID + 1)
            .into_iter()
            .flatten()
            .collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx) =
            prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }
}
