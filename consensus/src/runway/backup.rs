use crate::{units::UncheckedSignedUnit, Data, Hasher, Round, Signature};
use codec::{Decode, Encode, Error as CodecError};
use futures::channel::oneshot;
use log::{error, info, warn};
use std::{
    fmt,
    io::{Read, Write},
    marker::PhantomData,
};

/// Backup load error. Could be either caused by io error from Reader, or by decoding.
#[derive(Debug)]
pub enum LoaderError {
    IO(std::io::Error),
    Codec(CodecError),
    MissingRounds(Round, usize),
    DuplicateRounds(Round, usize),
    MissingAndDuplicateRounds(usize),
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoaderError::IO(err) => {
                write!(f, "Got IO error while reading from UnitLoader: {}", err)
            }

            LoaderError::Codec(err) => {
                write!(f, "Got Codec error while docoding backup: {}", err)
            }

            LoaderError::MissingRounds(round, n_units) => {
                write!(f, "Some units are missing. Loaded less units than the highest round + 1. Got round {:?} and {:?} units", round, n_units)
            }

            LoaderError::DuplicateRounds(round, n_units) => {
                write!(f, "Some units are duplicate. Loaded more units than the highest round + 1. Got round {:?} and {:?} units", round, n_units)
            }

            LoaderError::MissingAndDuplicateRounds(n_units) => {
                write!(f, "Loaded the same number of units as the highest round + 1 but some units are missing and some are duplicate. Got {:?} units", n_units)
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

/// Abstraction over Unit backup saving mechanism
pub struct UnitSaver<W: Write, H: Hasher, D: Data, S: Signature> {
    inner: W,
    _phantom: PhantomData<(H, D, S)>,
}

/// Abstraction over Unit backup loading mechanism
pub struct UnitLoader<R: Read, H: Hasher, D: Data, S: Signature> {
    inner: R,
    _phantom: PhantomData<(H, D, S)>,
}

impl<W: Write, H: Hasher, D: Data, S: Signature> UnitSaver<W, H, D, S> {
    pub fn new(write: W) -> Self {
        Self {
            inner: write,
            _phantom: PhantomData,
        }
    }

    pub fn save(&mut self, unit: UncheckedSignedUnit<H, D, S>) -> Result<(), std::io::Error> {
        self.inner.write_all(&unit.encode())?;
        self.inner.flush()?;
        Ok(())
    }
}

impl<R: Read, H: Hasher, D: Data, S: Signature> UnitLoader<R, H, D, S> {
    pub fn new(read: R) -> Self {
        Self {
            inner: read,
            _phantom: PhantomData,
        }
    }

    fn load(mut self) -> Result<Vec<UncheckedSignedUnit<H, D, S>>, LoaderError> {
        let mut buf = Vec::new();
        self.inner.read_to_end(&mut buf)?;
        let input = &mut &buf[..];
        let mut result = Vec::new();
        while !input.is_empty() {
            result.push(<UncheckedSignedUnit<H, D, S>>::decode(input)?);
        }
        Ok(result)
    }
}

fn load_backup<H: Hasher, D: Data, S: Signature, R: Read>(
    unit_loader: UnitLoader<R, H, D, S>,
) -> Result<(Vec<UncheckedSignedUnit<H, D, S>>, Round), LoaderError> {
    let (mut rounds, units): (Vec<_>, Vec<_>) = unit_loader
        .load()?
        .into_iter()
        .map(|u| (u.as_signable().coord().round(), u))
        .unzip();
    rounds.sort_unstable();
    if let Some(&max) = rounds.last() {
        if max as usize + 1 < rounds.len() {
            return Err(LoaderError::DuplicateRounds(max, rounds.len()));
        }
        if max as usize + 1 > rounds.len() {
            return Err(LoaderError::MissingRounds(max, rounds.len()));
        }
        if !rounds.iter().cloned().eq((0..).take(rounds.len())) {
            Err(LoaderError::MissingAndDuplicateRounds(rounds.len()))
        } else {
            Ok((units, max + 1))
        }
    } else {
        Ok((Vec::new(), 0))
    }
}

fn on_shutdown(starting_round_tx: oneshot::Sender<Option<Round>>) {
    if starting_round_tx.send(None).is_err() {
        warn!(target: "AlephBFT-unit-backup", "Could not send `None` starting round.");
    }
}

/// Loads Unit data from `unit_loader` and awaits on response from unit collection.
/// It sends all loaded units by `loaded_unit_tx`.
/// If loaded Units are compatible with the unit collection result (meaning the highest unit is from at least
/// round from unit collection + 1) it sends `Some(starting_round)` by
/// `starting_round_tx`. If Units are not compatible it sends `None` by `starting_round_tx`
pub async fn run_loading_mechanism<H: Hasher, D: Data, S: Signature, R: Read>(
    unit_loader: UnitLoader<R, H, D, S>,
    loaded_unit_tx: oneshot::Sender<Vec<UncheckedSignedUnit<H, D, S>>>,
    starting_round_tx: oneshot::Sender<Option<Round>>,
    next_round_collection_rx: oneshot::Receiver<Round>,
) {
    let (units, next_round_backup) = match load_backup(unit_loader) {
        Ok((units, round)) => (units, round),
        Err(e) => {
            error!(target: "AlephBFT-unit-backup", "unable to load unit backup: {}", e);
            on_shutdown(starting_round_tx);
            return;
        }
    };
    info!(target: "AlephBFT-unit-backup", "loaded units from backup. Loaded {:?} units", units.len());

    if let Err(e) = loaded_unit_tx.send(units) {
        error!(target: "AlephBFT-unit-backup", "could not send loaded units: {:?}", e);
        on_shutdown(starting_round_tx);
        return;
    }

    let next_round_collection = match next_round_collection_rx.await {
        Ok(round) => round,
        Err(e) => {
            error!(target: "AlephBFT-unit-backup", "unable to receive response from unit collections: {}", e);
            on_shutdown(starting_round_tx);
            return;
        }
    };
    info!(target: "AlephBFT-unit-backup", "received next round from unit collection: {:?}", next_round_collection);

    if next_round_backup < next_round_collection {
        error!(target: "AlephBFT-unit-backup", "backup lower than unit collection result. Backup got: {:?}, collection got: {:?}", next_round_backup, next_round_collection);
        on_shutdown(starting_round_tx);
        return;
    };

    if next_round_collection < next_round_backup {
        warn!(target: "AlephBFT-unit-backup", "unit collection result lower than backup. Backup got: {:?}, collection got: {:?}", next_round_backup, next_round_collection);
    }

    if let Err(e) = starting_round_tx.send(Some(next_round_backup)) {
        error!(target: "AlephBFT-unit-backup", "could not send starting round: {:?}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::{run_loading_mechanism, UnitLoader};
    use crate::{
        units::{
            create_units, creator_set, preunit_to_unchecked_signed_unit, preunit_to_unit,
            UncheckedSignedUnit as GenericUncheckedSignedUnit,
        },
        NodeCount, NodeIndex, Round,
    };
    use aleph_bft_mock::{Data, Hasher64, Keychain, Loader, Signature};
    use codec::Encode;
    use futures::channel::oneshot::{self, Receiver, Sender};

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;

    async fn prepare_test(
        units: Vec<(u16, bool)>,
    ) -> (
        impl futures::Future,
        Receiver<Vec<UncheckedSignedUnit>>,
        Sender<Round>,
        Receiver<Option<Round>>,
        Vec<UncheckedSignedUnit>,
    ) {
        let node_id = NodeIndex(0);
        let n_members = NodeCount(4);
        let session_id = 43;

        let mut encoded_data = Vec::new();
        let mut data = Vec::new();

        let mut creators = creator_set(n_members);
        let keychain = Keychain::new(n_members, node_id);

        for (round, (ammount, is_corrupted)) in units.into_iter().enumerate() {
            let pre_units = create_units(creators.iter(), round as Round);

            let unit =
                preunit_to_unchecked_signed_unit(pre_units[0].clone().0, session_id, &keychain)
                    .await;
            for _ in 0..ammount {
                if is_corrupted {
                    let backup = unit.clone().encode();
                    encoded_data.extend_from_slice(&backup[..backup.len() - 1]);
                } else {
                    encoded_data.append(&mut unit.clone().encode());
                }
            }
            data.push(unit);

            let new_units: Vec<_> = pre_units
                .into_iter()
                .map(|(pre_unit, _)| preunit_to_unit(pre_unit, session_id))
                .collect();
            for creator in creators.iter_mut() {
                creator.add_units(&new_units);
            }
        }
        let unit_loader = UnitLoader::new(Loader::new(encoded_data));
        let (loaded_unit_tx, loaded_unit_rx) = oneshot::channel();
        let (starting_round_tx, starting_round_rx) = oneshot::channel();
        let (highest_response_tx, highest_response_rx) = oneshot::channel();

        (
            run_loading_mechanism(
                unit_loader,
                loaded_unit_tx,
                starting_round_tx,
                highest_response_rx,
            ),
            loaded_unit_rx,
            highest_response_tx,
            starting_round_rx,
            data,
        )
    }

    #[tokio::test]
    async fn nothing_loaded_nothing_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test(Vec::new()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(0)));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn something_loaded_nothing_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test((0..5).map(|_| (1, false)).collect()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn something_loaded_something_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test((0..5).map(|_| (1, false)).collect()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(5).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn nothing_loaded_something_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test(Vec::new()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(1).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn loaded_smaller_then_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test((0..3).map(|_| (1, false)).collect()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(4).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn nothing_collected() {
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test((0..3).map(|_| (1, false)).collect()).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        drop(highest_response_tx);

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.await, Ok(data));
    }

    #[tokio::test]
    async fn corrupted_backup_codec() {
        let mut units = vec![(1, true)];
        units.extend(&mut (0..4).map(|_| (1, false)));
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(units).await;
        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn corrupted_backup_missing() {
        let mut units = vec![(0, false)];
        units.extend(&mut (0..4).map(|_| (1, false)));
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(units).await;
        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn corrupted_backup_duplicate() {
        let mut units = vec![(2, false)];
        units.extend(&mut (0..4).map(|_| (1, false)));
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(units).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }

    #[tokio::test]
    async fn corrupted_backup_missing_and_duplicate() {
        let mut units = vec![(0, false)];
        units.extend(&mut (0..3).map(|_| (1, false)));
        units.push((2, false));
        let (task, loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(units).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert!(loaded_unit_rx.await.is_err());
    }
}
