use crate::{units::UncheckedSignedUnit, Data, Hasher, Round, Sender, Signature};
use codec::{Decode, Encode, Error as CodecError};
use futures::channel::oneshot;
use log::{error, warn};
use std::io::{Read, Write};

#[derive(Debug)]
pub enum LoaderError {
    IO(std::io::Error),
    Codec(CodecError),
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

pub trait UnitSaver<H: Hasher, D: Data, S: Signature> {
    fn save(&mut self, unit: UncheckedSignedUnit<H, D, S>) -> Result<(), std::io::Error>;
}

pub trait UnitLoader<H: Hasher, D: Data, S: Signature> {
    fn load(self) -> Result<Vec<UncheckedSignedUnit<H, D, S>>, LoaderError>;
}

impl<H: Hasher, D: Data, S: Signature, W: Write> UnitSaver<H, D, S> for W {
    fn save(&mut self, unit: UncheckedSignedUnit<H, D, S>) -> Result<(), std::io::Error> {
        self.write_all(&unit.encode())?;
        self.flush()?;
        Ok(())
    }
}

impl<H: Hasher, D: Data, S: Signature, R: Read> UnitLoader<H, D, S> for R {
    fn load(mut self) -> Result<Vec<UncheckedSignedUnit<H, D, S>>, LoaderError> {
        let mut buf = Vec::new();
        self.read_to_end(&mut buf)?;
        let input = &mut &buf[..];
        let mut result = vec![];
        while !input.is_empty() {
            result.push(<UncheckedSignedUnit<H, D, S>>::decode(input)?);
        }
        Ok(result)
    }
}

fn _load_backup<H: Hasher, D: Data, S: Signature, UL: UnitLoader<H, D, S>>(
    unit_loader: UL,
) -> Result<(Vec<UncheckedSignedUnit<H, D, S>>, Round), LoaderError> {
    let (rounds, units): (Vec<_>, Vec<_>) = unit_loader
        .load()?
        .into_iter()
        .map(|u| (u.as_signable().coord().round(), u))
        .unzip();
    let next_round = if let Some(round) = rounds.into_iter().max() {
        round + 1
    } else {
        0
    };
    Ok((units, next_round))
}

fn _on_shutdown(starting_round_tx: oneshot::Sender<Option<Round>>) {
    if starting_round_tx.send(None).is_err() {
        warn!(target: "AlephBFT-runway", "Coulnd not send `None` starting round.");
    }
}

pub async fn _run_loading_mechanism<H: Hasher, D: Data, S: Signature, UL: UnitLoader<H, D, S>>(
    unit_loader: UL,
    loaded_unit_tx: Sender<UncheckedSignedUnit<H, D, S>>,
    starting_round_tx: oneshot::Sender<Option<Round>>,
    highest_response_rx: oneshot::Receiver<Round>,
) {
    let (units, next_round) = match _load_backup(unit_loader) {
        Ok((units, next_round)) => (units, next_round),
        Err(e) => {
            error!(target: "AlephBFT-runway", "unable to load unit backup: {:?}", e);
            _on_shutdown(starting_round_tx);
            return;
        }
    };

    let highest_response = match highest_response_rx.await {
        Ok(highest_response) => highest_response,
        Err(e) => {
            error!(target: "AlephBFT-runway", "unable to receive response from unit collections: {:?}", e);
            _on_shutdown(starting_round_tx);
            return;
        }
    };

    let starting_round = next_round;
    if starting_round < highest_response {
        error!(target: "AlephBFT-runway", "backup and unit collection missmatch. Backup got: {:?}, collection got: {:?}", starting_round, highest_response);
        _on_shutdown(starting_round_tx);
        return;
    };

    for u in units {
        if let Err(e) = loaded_unit_tx.unbounded_send(u) {
            error!(target: "AlephBFT-runway", "could not send loaded unit: {:?}", e);
            _on_shutdown(starting_round_tx);
            return;
        }
    }

    if let Err(e) = starting_round_tx.send(Some(starting_round)) {
        error!(target: "AlephBFT-runway", "could not send starting round: {:?}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::_run_loading_mechanism as run_loading_mechanism;
    use crate::{
        units::{
            add_units, create_units, creator_set, preunit_to_unchecked_signed_unit,
            preunit_to_unit, UncheckedSignedUnit as GenericUncheckedSignedUnit,
        },
        NodeCount, NodeIndex, Receiver, Round,
    };
    use aleph_bft_mock::{Data, Hasher64, Keychain, Loader, Signature};
    use codec::Encode;
    use futures::{
        channel::{mpsc::unbounded, oneshot},
        StreamExt,
    };

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;

    async fn prepare_test(
        units_n: u16,
        is_corrupted: bool,
    ) -> (
        impl futures::Future,
        Receiver<UncheckedSignedUnit>,
        oneshot::Sender<Round>,
        oneshot::Receiver<Option<Round>>,
        Vec<UncheckedSignedUnit>,
    ) {
        let node_id = NodeIndex(0);
        let n_members = NodeCount(4);
        let session_id = 43;

        let mut encoded_data = vec![];
        let mut data = vec![];

        let mut creators = creator_set(n_members);
        let keychain = Keychain::new(n_members, node_id);

        for round in 0..units_n {
            let pre_units = create_units(creators.iter(), round);

            let unit =
                preunit_to_unchecked_signed_unit(pre_units[0].clone().0, session_id, &keychain)
                    .await;
            if is_corrupted {
                let backup = unit.clone().encode();
                encoded_data.extend_from_slice(&backup[..backup.len() - 1]);
            } else {
                encoded_data.append(&mut unit.clone().encode());
            }
            data.push(unit);

            let new_units: Vec<_> = pre_units
                .into_iter()
                .map(|(pre_unit, _)| preunit_to_unit(pre_unit, session_id))
                .collect();
            for creator in creators.iter_mut() {
                add_units(creator, &new_units);
            }
        }
        let unit_loader = Loader::new(encoded_data);
        let (loaded_unit_tx, loaded_unit_rx) = unbounded();
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
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(0, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(0)));
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn something_loaded_nothing_collected() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test(5, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        for unit in data {
            assert_eq!(loaded_unit_rx.next().await, Some(unit));
        }
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn something_loaded_something_collected() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, data) =
            prepare_test(5, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(5).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(Some(5)));
        for unit in data {
            assert_eq!(loaded_unit_rx.next().await, Some(unit));
        }
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn nothing_loaded_something_collected() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(0, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(1).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn loaded_smaller_then_collected() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(3, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(4).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn nothing_collected() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(3, false).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        drop(highest_response_tx);

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }

    #[tokio::test]
    async fn corrupted_backup() {
        let (task, mut loaded_unit_rx, highest_response_tx, starting_round_rx, _) =
            prepare_test(5, true).await;

        let handle = tokio::spawn(async {
            task.await;
        });

        highest_response_tx.send(0).unwrap();

        handle.await.unwrap();

        assert_eq!(starting_round_rx.await, Ok(None));
        assert_eq!(loaded_unit_rx.try_next().unwrap(), None);
    }
}
