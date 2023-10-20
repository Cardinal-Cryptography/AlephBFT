use std::{fmt, fmt::Debug, io::Read, marker::PhantomData};

use codec::{Decode, Error as CodecError};
use futures::channel::oneshot;
use log::{error, info, warn};

use crate::{backup::BackupItem, Data, Hasher, MultiKeychain, NodeIndex, Round};

const LOG_TARGET: &str = "AlephBFT-backup-loader";

/// Backup read error. Could be either caused by io error from `BackupReader`, or by decoding.
#[derive(Debug)]
enum LoaderError {
    IO(std::io::Error),
    Codec(CodecError),
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoaderError::IO(err) => {
                write!(
                    f,
                    "received IO error while reading from backup source: {}",
                    err
                )
            }
            LoaderError::Codec(err) => {
                write!(f, "received Codec error while decoding backup: {}", err)
            }
        }
    }
}

impl From<std::io::Error> for LoaderError {
    fn from(err: std::io::Error) -> Self {
        Self::IO(err)
    }
}

impl From<CodecError> for LoaderError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

pub struct BackupLoader<H: Hasher, D: Data, MK: MultiKeychain, R: Read> {
    backup: R,
    index: NodeIndex,
    _phantom: PhantomData<(H, D, MK)>,
}

impl<H: Hasher, D: Data, MK: MultiKeychain, R: Read> BackupLoader<H, D, MK, R> {
    pub fn new(backup: R, index: NodeIndex) -> BackupLoader<H, D, MK, R> {
        BackupLoader {
            backup,
            index,
            _phantom: PhantomData,
        }
    }

    fn load(&mut self) -> Result<Vec<BackupItem<H, D, MK>>, LoaderError> {
        let mut buf = Vec::new();
        self.backup.read_to_end(&mut buf)?;
        let input = &mut &buf[..];
        let mut result = Vec::new();
        while !input.is_empty() {
            result.push(<BackupItem<H, D, MK>>::decode(input)?);
        }
        Ok(result)
    }

    fn on_shutdown(&self, starting_round: oneshot::Sender<Option<Round>>) {
        if starting_round.send(None).is_err() {
            warn!(target: LOG_TARGET, "Could not send `None` starting round.");
        }
    }

    fn verify_backup_and_collection_rounds(
        &self,
        next_round_backup: Round,
        next_round_collection: Round,
    ) -> Option<Round> {
        if next_round_backup < next_round_collection {
            // Our newest unit doesn't appear in the backup. This indicates a serious issue, for example
            // a different node running with the same pair of keys. It's safer not to continue.
            error!(
                target: LOG_TARGET, "Backup state behind unit collection state. Next round inferred from: collection: {:?}, backup: {:?}",
                next_round_collection,
                next_round_backup,
            );
            return None;
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

        Some(next_round_backup)
    }

    pub async fn run(
        &mut self,
        loaded_data: oneshot::Sender<Vec<BackupItem<H, D, MK>>>,
        starting_round: oneshot::Sender<Option<Round>>,
        next_round_collection: oneshot::Receiver<Round>,
    ) {
        let items = match self.load() {
            Ok(items) => items,
            Err(e) => {
                error!(target: LOG_TARGET, "error when loading backup: {:?}.", e);
                self.on_shutdown(starting_round);
                return;
            }
        };

        let next_round_backup: Round = items
            .iter()
            .filter_map(|i| match i {
                BackupItem::Unit(u) => Some(u),
                BackupItem::AlertData(_) => None,
            })
            .filter(|u| u.as_signable().creator() == self.index)
            .map(|u| u.as_signable().round())
            .max()
            .map(|round| round + 1)
            .unwrap_or(0);

        info!(
            target: LOG_TARGET,
            "Loaded {:?} items from backup. Able to continue from round: {:?}.",
            items.len(),
            next_round_backup
        );

        if loaded_data.send(items).is_err() {
            error!(target: LOG_TARGET, "Could not send loaded items");
            self.on_shutdown(starting_round);
            return;
        }

        let next_round_collection = match next_round_collection.await {
            Ok(round) => round,
            Err(e) => {
                error!(
                    target: LOG_TARGET,
                    "Unable to receive response from unit collection: {}", e
                );
                self.on_shutdown(starting_round);
                return;
            }
        };

        info!(
            target: LOG_TARGET,
            "Next round inferred from collection: {:?}", next_round_collection
        );

        let next_round = match self
            .verify_backup_and_collection_rounds(next_round_backup, next_round_collection)
        {
            Some(next_round) => next_round,
            None => {
                self.on_shutdown(starting_round);
                return;
            }
        };

        if let Err(e) = starting_round.send(Some(next_round)) {
            error!(target: LOG_TARGET, "Could not send starting round: {:?}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use codec::Encode;
    use futures::channel::oneshot;
    use itertools::Itertools;

    use aleph_bft_mock::{Data, Hasher64, Keychain, Loader, Signature};

    use crate::{
        backup::{BackupItem, BackupLoader},
        units::{
            create_units, creator_set, preunit_to_unchecked_signed_unit, preunit_to_unit,
            UncheckedSignedUnit as GenericUncheckedSignedUnit,
        },
        NodeCount, NodeIndex, Round, SessionId,
    };

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;
    type TestBackupItem = BackupItem<Hasher64, Data, Keychain>;
    struct PrepareTestResponse<F: futures::Future> {
        task: F,
        loaded_data_rx: oneshot::Receiver<Vec<BackupItem<Hasher64, Data, Keychain>>>,
        highest_response_tx: oneshot::Sender<Round>,
        starting_round_rx: oneshot::Receiver<Option<Round>>,
    }

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

    fn encode_all(items: Vec<TestBackupItem>) -> Vec<Vec<u8>> {
        items.iter().map(|u| u.encode()).collect()
    }

    fn prepare_test(encoded_items: Vec<u8>) -> PrepareTestResponse<impl futures::Future> {
        let (loaded_data_tx, loaded_data_rx) = oneshot::channel();
        let (starting_round_tx, starting_round_rx) = oneshot::channel();
        let (highest_response_tx, highest_response_rx) = oneshot::channel();

        let task = {
            let mut backup_loader = BackupLoader::new(Loader::new(encoded_items), NODE_ID);

            async move {
                backup_loader
                    .run(loaded_data_tx, starting_round_tx, highest_response_rx)
                    .await
            }
        };

        PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        }
    }

    #[tokio::test]
    async fn nothing_loaded_nothing_collected_succeeds() {
        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(Vec::new());

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(0)));
        assert_eq!(loaded_data_rx.await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn something_loaded_nothing_collected_succeeds() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.clone().into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();
        let items = units.into_iter().map(BackupItem::Unit).collect_vec();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_data_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn something_loaded_something_collected_succeeds() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.clone().into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(5).unwrap();
        handle.await.unwrap();
        let items = units.into_iter().map(BackupItem::Unit).collect_vec();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_data_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn nothing_loaded_something_collected_fails() {
        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(Vec::new());

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(1).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_data_rx.await, Ok(Vec::new()));
    }

    #[tokio::test]
    async fn loaded_smaller_then_collected_fails() {
        let units: Vec<_> = produce_units(3, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.clone().into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(4).unwrap();
        handle.await.unwrap();
        let items = units.into_iter().map(BackupItem::Unit).collect_vec();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_data_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn dropped_collection_fails() {
        let units: Vec<_> = produce_units(3, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.clone().into_iter().map(BackupItem::Unit).collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_items);

        let handle = tokio::spawn(async {
            task.await;
        });

        drop(highest_response_tx);
        handle.await.unwrap();
        let items = units.into_iter().map(BackupItem::Unit).collect_vec();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_data_rx.await, Ok(items));
    }

    #[tokio::test]
    async fn backup_with_corrupted_encoding_fails() {
        let units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let items: Vec<_> = units.into_iter().map(BackupItem::Unit).collect();
        let mut item_encodings = encode_all(items);
        let unit2_encoding_len = item_encodings[2].len();
        item_encodings[2].resize(unit2_encoding_len - 1, 0); // remove the last byte
        let encoded_items = item_encodings.into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_items);
        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_data_rx.await.is_err());
    }

    #[tokio::test]
    async fn backup_with_duplicate_unit_succeeds() {
        let mut units: Vec<_> = produce_units(5, SESSION_ID).into_iter().flatten().collect();
        let unit2_duplicate = units[2].clone();
        units.insert(3, unit2_duplicate);
        let items: Vec<_> = units.clone().into_iter().map(BackupItem::Unit).collect();
        let encoded_units = encode_all(items.clone()).into_iter().flatten().collect();

        let PrepareTestResponse {
            task,
            loaded_data_rx,
            highest_response_tx,
            starting_round_rx,
        } = prepare_test(encoded_units);

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();
        handle.await.unwrap();
        let items = units.into_iter().map(BackupItem::Unit).collect_vec();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_data_rx.await, Ok(items));
    }
}
