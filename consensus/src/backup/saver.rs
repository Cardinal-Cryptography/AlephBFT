use std::{marker::PhantomData, pin::Pin};

use crate::{
    dag::DagUnit,
    units::{UncheckedSignedUnit, WrappedUnit},
    Data, Hasher, MultiKeychain, Receiver, Sender, Terminator,
};
use codec::Encode;
use futures::{AsyncWrite, AsyncWriteExt, FutureExt, StreamExt};
use log::{debug, error};

const LOG_TARGET: &str = "AlephBFT-backup-saver";

/// A saver for arbitrary encodeable items.
pub struct BackupSaver<W: AsyncWrite, D: Encode> {
    backup: Pin<Box<W>>,
    _phantom: PhantomData<D>,
}

impl<W: AsyncWrite, D: Encode> BackupSaver<W, D> {
    /// A new backup saver using the given writer.
    pub fn new(backup: W) -> Self {
        BackupSaver {
            backup: Box::pin(backup),
            _phantom: PhantomData,
        }
    }

    /// Save a single item.
    pub async fn save_item(&mut self, item: D) -> Result<(), std::io::Error> {
        self.backup.write_all(&item.encode()).await?;
        self.backup.flush().await
    }
}

/// Component responsible for saving units into backup.
/// It waits for items to appear on its receivers, and writes them to backup.
/// It announces a successful write through an appropriate response sender.
pub struct SaverService<H: Hasher, D: Data, MK: MultiKeychain, W: AsyncWrite> {
    units_from_consensus: Receiver<DagUnit<H, D, MK>>,
    responses_for_consensus: Sender<DagUnit<H, D, MK>>,
    saver: BackupSaver<W, UncheckedSignedUnit<H, D, MK::Signature>>,
}

impl<H: Hasher, D: Data, MK: MultiKeychain, W: AsyncWrite> SaverService<H, D, MK, W> {
    pub fn new(
        units_from_consensus: Receiver<DagUnit<H, D, MK>>,
        responses_for_consensus: Sender<DagUnit<H, D, MK>>,
        saver: BackupSaver<W, UncheckedSignedUnit<H, D, MK::Signature>>,
    ) -> Self {
        SaverService {
            units_from_consensus,
            responses_for_consensus,
            saver,
        }
    }

    async fn save_unit(&mut self, unit: &DagUnit<H, D, MK>) -> Result<(), std::io::Error> {
        self.saver.save_item(unit.clone().unpack().into()).await
    }

    /// Run the saving service.
    pub async fn run(&mut self, mut terminator: Terminator) {
        let mut terminator_exit = false;
        loop {
            futures::select! {
                unit = self.units_from_consensus.next() => {
                    let item = match unit {
                        Some(unit) => unit,
                        None => {
                            error!(target: LOG_TARGET, "receiver of units to save closed early");
                            break;
                        },
                    };
                    if let Err(e) = self.save_unit(&item).await {
                        error!(target: LOG_TARGET, "couldn't save item to backup: {:?}", e);
                        break;
                    }
                    if self.responses_for_consensus.unbounded_send(item).is_err() {
                        error!(target: LOG_TARGET, "couldn't respond with saved unit to consensus");
                        break;
                    }
                },
                _ = terminator.get_exit().fuse() => {
                    debug!(target: LOG_TARGET, "backup saver received exit signal.");
                    terminator_exit = true;
                }
            }

            if terminator_exit {
                debug!(target: LOG_TARGET, "backup saver decided to exit.");
                terminator.terminate_sync().await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use futures::{
        channel::{mpsc, oneshot},
        StreamExt,
    };

    use aleph_bft_mock::{Data, Hasher64, Keychain, Saver};

    use crate::{
        backup::{BackupSaver, SaverService},
        dag::ReconstructedUnit,
        units::{creator_set, preunit_to_signed_unit, TestingSignedUnit},
        NodeCount, Terminator,
    };

    type TestUnit = ReconstructedUnit<TestingSignedUnit>;
    type TestSaverService = SaverService<Hasher64, Data, Keychain, Saver>;
    struct PrepareSaverResponse<F: futures::Future> {
        task: F,
        units_for_saver: mpsc::UnboundedSender<TestUnit>,
        units_from_saver: mpsc::UnboundedReceiver<TestUnit>,
        exit_tx: oneshot::Sender<()>,
    }

    fn prepare_saver() -> PrepareSaverResponse<impl futures::Future> {
        let (units_for_saver, units_from_consensus) = mpsc::unbounded();
        let (units_for_consensus, units_from_saver) = mpsc::unbounded();
        let (exit_tx, exit_rx) = oneshot::channel();
        let backup = Saver::new();

        let task = {
            let mut saver: TestSaverService = SaverService::new(
                units_from_consensus,
                units_for_consensus,
                BackupSaver::new(backup),
            );

            async move {
                saver.run(Terminator::create_root(exit_rx, "saver")).await;
            }
        };

        PrepareSaverResponse {
            task,
            units_for_saver,
            units_from_saver,
            exit_tx,
        }
    }

    #[tokio::test]
    async fn test_proper_relative_responses_ordering() {
        let node_count = NodeCount(5);
        let PrepareSaverResponse {
            task,
            units_for_saver,
            mut units_from_saver,
            exit_tx,
        } = prepare_saver();

        let handle = tokio::spawn(async {
            task.await;
        });

        let creators = creator_set(node_count);
        let keychains: Vec<_> = node_count
            .into_iterator()
            .map(|id| Keychain::new(node_count, id))
            .collect();
        let units: Vec<TestUnit> = node_count
            .into_iterator()
            .map(|id| {
                ReconstructedUnit::initial(preunit_to_signed_unit(
                    creators[id.0].create_unit(0).unwrap(),
                    0,
                    &keychains[id.0],
                ))
            })
            .collect();

        for u in units.iter() {
            units_for_saver.unbounded_send(u.clone()).unwrap();
        }

        for u in units {
            let u_backup = units_from_saver.next().await.unwrap();
            assert_eq!(u, u_backup);
        }

        exit_tx.send(()).unwrap();
        handle.await.unwrap();
    }
}
