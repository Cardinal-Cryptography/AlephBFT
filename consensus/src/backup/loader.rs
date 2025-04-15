use std::{
    fmt::{self, Debug},
    pin::Pin,
};

use codec::{Decode, Error as CodecError};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite};
use log::{debug, info};

use crate::{
    backup::{BackupSaver as GenericBackupSaver, LOG_TARGET},
    consensus::{Consensus, ConsensusResult},
    creation::Creator,
    units::{UncheckedSignedUnit, Unit, WrappedUnit},
    Keychain, MultiKeychain, Round, UnitFinalizationHandler,
};

/// Backup read error. Could be either caused by io error from `BackupReader`, or by decoding.
#[derive(Debug)]
pub enum LoaderError {
    IORead(std::io::Error),
    IOWrite(std::io::Error),
    Codec(CodecError),
    AlertsUnimplemented,
    ReconstructionFailure(usize, usize),
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoaderError::IORead(err) => {
                write!(
                    f,
                    "received IO error while reading from backup source: {}",
                    err
                )
            }
            LoaderError::IOWrite(err) => {
                write!(f, "received IO error while writing to new backup: {}", err)
            }
            LoaderError::Codec(err) => {
                write!(f, "received Codec error while decoding backup: {}", err)
            }
            LoaderError::AlertsUnimplemented => {
                write!(
                    f,
                    "attempted to load backup with forks, this is not yet implemented"
                )
            }
            LoaderError::ReconstructionFailure(expected, reconstructed) => {
                write!(
                    f,
                    "there were {} units saved, but only {} got reconstructed",
                    expected, reconstructed
                )
            }
        }
    }
}

impl From<CodecError> for LoaderError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

type BackupSaver<W, UFH, MK> = GenericBackupSaver<
    W,
    UncheckedSignedUnit<
        <UFH as UnitFinalizationHandler>::Hasher,
        <UFH as UnitFinalizationHandler>::Data,
        <MK as Keychain>::Signature,
    >,
>;

/// The state that resulted from reading the backup.
pub struct BackupResult<W, UFH, MK>
where
    W: AsyncWrite,
    UFH: UnitFinalizationHandler,
    MK: MultiKeychain,
{
    pub consensus: Consensus<UFH, MK>,
    pub creator: Creator<UFH::Hasher>,
    pub saver: BackupSaver<W, UFH, MK>,
    pub starting_round: Round,
}

/// Loads units from backup simulating how they would be processed during normal operation.
/// NOTE: currently does not support forks and alerts.
pub struct BackupLoader<W, UFH, MK, R>
where
    W: AsyncWrite,
    UFH: UnitFinalizationHandler,
    MK: MultiKeychain,
    R: AsyncRead,
{
    backup: Pin<Box<R>>,
    consensus: Consensus<UFH, MK>,
    creator: Creator<UFH::Hasher>,
    saver: BackupSaver<W, UFH, MK>,
    starting_round: Round,
}

impl<W, UFH, MK, R> BackupLoader<W, UFH, MK, R>
where
    W: AsyncWrite,
    UFH: UnitFinalizationHandler,
    MK: MultiKeychain,
    R: AsyncRead,
{
    /// Create a new backup loader, which will apply all changes to the provided components.
    pub fn new(
        backup: R,
        consensus: Consensus<UFH, MK>,
        creator: Creator<UFH::Hasher>,
        saver: BackupSaver<W, UFH, MK>,
    ) -> Self {
        BackupLoader {
            backup: Box::pin(backup),
            consensus,
            creator,
            saver,
            starting_round: 0,
        }
    }

    async fn load(
        &mut self,
    ) -> Result<Vec<UncheckedSignedUnit<UFH::Hasher, UFH::Data, MK::Signature>>, LoaderError> {
        let mut buf = Vec::new();
        self.backup
            .read_to_end(&mut buf)
            .await
            .map_err(LoaderError::IORead)?;
        let input = &mut &buf[..];
        let mut result = Vec::new();
        while !input.is_empty() {
            result
                .push(<UncheckedSignedUnit<UFH::Hasher, UFH::Data, MK::Signature>>::decode(input)?);
        }
        Ok(result)
    }

    async fn process_unit(
        &mut self,
        unit: UncheckedSignedUnit<UFH::Hasher, UFH::Data, MK::Signature>,
    ) -> Result<(), LoaderError> {
        let ConsensusResult {
            units,
            alerts,
            messages,
        } = self.consensus.process_incoming_unit(unit);
        debug!(target: LOG_TARGET, "Dropping {} messages.", messages.len());
        if !alerts.is_empty() {
            return Err(LoaderError::AlertsUnimplemented);
        }
        for unit in units {
            self.saver
                .save_item(unit.clone().unpack().into())
                .await
                .map_err(LoaderError::IOWrite)?;
            self.creator.add_unit(&unit);
            let round = unit.round();
            if self.consensus.on_unit_backup_saved(unit).is_some() {
                // this was our unit since we are trying to broadcast it, update the starting round
                self.starting_round = round + 1;
            }
        }
        Ok(())
    }

    /// Load the backup and return all the modified components or fail.
    pub async fn load_backup(mut self) -> Result<BackupResult<W, UFH, MK>, LoaderError> {
        let units = self.load().await?;
        let decoded_units = units.len();
        for unit in units {
            self.process_unit(unit).await?;
        }
        let BackupLoader {
            backup: _,
            consensus,
            creator,
            saver,
            starting_round,
        } = self;
        let reconstructed = consensus.status().units_in_dag();
        if decoded_units != reconstructed {
            return Err(LoaderError::ReconstructionFailure(
                decoded_units,
                reconstructed,
            ));
        }
        info!(target: LOG_TARGET, "Loaded {} units from backup.", reconstructed);
        Ok(BackupResult {
            consensus,
            creator,
            saver,
            starting_round,
        })
    }
}

#[cfg(test)]
mod tests {
    use codec::Encode;

    use aleph_bft_mock::{Data, FinalizationHandler, Hasher64, Keychain, Loader, Saver, Signature};

    use crate::{
        backup::{
            loader::LoaderError, BackupLoader as GenericBackupLoader, BackupResult, BackupSaver,
        },
        consensus::Consensus,
        creation::Creator,
        interface::FinalizationHandlerAdapter,
        testing::gen_delay_config,
        units::{
            random_full_parent_reconstrusted_units_up_to,
            UncheckedSignedUnit as GenericUncheckedSignedUnit, Validator, WrappedUnit,
        },
        NodeCount, NodeIndex, Receiver, Round, SessionId,
    };

    type UncheckedSignedUnit = GenericUncheckedSignedUnit<Hasher64, Data, Signature>;
    type BackupLoader = GenericBackupLoader<
        Saver,
        FinalizationHandlerAdapter<FinalizationHandler, Data, Hasher64>,
        Keychain,
        Loader,
    >;

    const SESSION_ID: SessionId = 43;
    const NODE_ID: NodeIndex = NodeIndex(0);
    const N_MEMBERS: NodeCount = NodeCount(4);
    const MAX_ROUND: Round = 2137;

    fn produce_units(rounds: Round, session_id: SessionId) -> Vec<Vec<UncheckedSignedUnit>> {
        let keychains: Vec<_> = (0..N_MEMBERS.0)
            .map(|id| Keychain::new(N_MEMBERS, NodeIndex(id)))
            .collect();
        random_full_parent_reconstrusted_units_up_to(rounds, N_MEMBERS, session_id, &keychains)
            .into_iter()
            .map(|round_units| {
                round_units
                    .into_iter()
                    .map(|unit| unit.unpack().into())
                    .collect()
            })
            .collect()
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

    fn encode_all(items: Vec<UncheckedSignedUnit>) -> Vec<Vec<u8>> {
        items.iter().map(|u| u.encode()).collect()
    }

    fn prepare_loader(backup: Loader) -> (BackupLoader, Receiver<Data>) {
        let keychain = Keychain::new(N_MEMBERS, NODE_ID);
        let validator = Validator::new(SESSION_ID, keychain, MAX_ROUND);
        let (finalization_handler, finalized_data) = FinalizationHandler::new();
        let finalization_handler: FinalizationHandlerAdapter<_, _, _> = finalization_handler.into();
        let delay_config = gen_delay_config();
        let consensus = Consensus::new(keychain, validator, finalization_handler, delay_config);
        let creator = Creator::new(NODE_ID, N_MEMBERS);
        let saver = BackupSaver::new(Saver::new());
        let loader = BackupLoader::new(backup, consensus, creator, saver);
        (loader, finalized_data)
    }

    #[tokio::test]
    async fn loads_nothing() {
        let (loader, _finalized_data) = prepare_loader(Loader::new(Vec::new()));
        let BackupResult {
            consensus,
            creator,
            saver: _,
            starting_round,
        } = loader.load_backup().await.expect("should load correctly");
        assert_eq!(creator.current_round(), 0);
        assert_eq!(consensus.status().units_in_dag(), 0);
        assert_eq!(starting_round, 0);
    }

    #[tokio::test]
    async fn loads_some_units() {
        let current_round = 5;
        let items: Vec<_> = produce_units(current_round, SESSION_ID)
            .into_iter()
            .flatten()
            .collect();
        let encoded_items = encode_all(items.clone()).into_iter().flatten().collect();

        let (loader, _finalized_data) = prepare_loader(Loader::new(encoded_items));
        let BackupResult {
            consensus,
            creator,
            saver: _,
            starting_round,
        } = loader.load_backup().await.expect("should load correctly");
        assert_eq!(consensus.status().units_in_dag(), items.len());
        assert_eq!(creator.current_round(), current_round);
        assert_eq!(starting_round, current_round + 1);
    }

    #[tokio::test]
    async fn backup_with_corrupted_encoding_fails() {
        let current_round = 5;
        let items: Vec<_> = produce_units(current_round, SESSION_ID)
            .into_iter()
            .flatten()
            .collect();
        let mut item_encodings = encode_all(items);
        let unit2_encoding_len = item_encodings[2].len();
        item_encodings[2].resize(unit2_encoding_len - 1, 0); // remove the last byte
        let encoded_items = item_encodings.into_iter().flatten().collect();

        let (loader, _finalized_data) = prepare_loader(Loader::new(encoded_items));
        assert!(matches!(
            loader.load_backup().await,
            Err(LoaderError::Codec(_))
        ));
    }

    #[tokio::test]
    async fn backup_with_missing_parent_fails() {
        let current_round = 5;
        let mut items: Vec<_> = produce_units(current_round, SESSION_ID)
            .into_iter()
            .flatten()
            .collect();
        items.remove(2); // it is a parent of all units of round 3
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (loader, _finalized_data) = prepare_loader(Loader::new(encoded_items));
        assert!(matches!(
            loader.load_backup().await,
            Err(LoaderError::ReconstructionFailure(_, _))
        ));
    }

    #[tokio::test]
    async fn backup_with_units_of_one_creator_fails() {
        let current_round = 5;
        let items = units_of_creator(
            produce_units(current_round, SESSION_ID),
            NodeIndex(NODE_ID.0 + 1),
        );
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (loader, _finalized_data) = prepare_loader(Loader::new(encoded_items));
        assert!(matches!(
            loader.load_backup().await,
            Err(LoaderError::ReconstructionFailure(_, _))
        ));
    }

    #[tokio::test]
    async fn backup_with_wrong_session_fails() {
        let current_round = 5;
        let items: Vec<_> = produce_units(current_round, SESSION_ID + 1)
            .into_iter()
            .flatten()
            .collect();
        let encoded_items = encode_all(items).into_iter().flatten().collect();

        let (loader, _finalized_data) = prepare_loader(Loader::new(encoded_items));
        assert!(matches!(
            loader.load_backup().await,
            Err(LoaderError::ReconstructionFailure(_, _))
        ));
    }
}
