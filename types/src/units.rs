use async_trait::async_trait;
use codec::Codec;

/// An abstraction for reading saved data that can be Encoded and Decoded
#[async_trait]
pub trait Reader<T: Codec + Send + Sync>: Send + Sync {
    /// Returns Some(T) containing next element to read.
    /// Return None if there is no elements left to read.
    fn next(&mut self) -> Option<T>;
}

/// An abstraction for saving data that can be Encoded and Decoded
#[async_trait]
pub trait Backup<T: Codec + Send + Sync>: Send + Sync {
    /// Saves data `T`. Upon ending needs to guarantee that `data` is saved.
    async fn save(&self, data: T);
}

/// A type to generate a structure for saving data that needs to be backed up.
/// Example implementation:
/// ```rust
/// struct BackupImpl {
///     tx: UnboundedSender<Vec<u8>>,
/// }
///
/// #[async_trait]
/// impl<T: Codec + Send + Sized + Sync + 'static> Backup<T> for BackupImpl {
///     async fn save(&self, data: T) {
///         if let Err(e) = self.tx.unbounded_send(data.encode()) {
///             ...
///         }
///     }
/// }
///
/// struct IntoBackupImpl {
///     tx: UnboundedSender<Vec<u8>>,
/// }
///
/// impl IntoBackup for IntoBackupImpl {
///     type Into = BackupImpl;
///
///     fn into_backup<T: Codec + Send + Sized + Sync + 'static>(self) -> Self::Into {
///         Backup { tx: self.tx }
///     }
/// }
///
/// impl IntoBackupImpl {
///     fn new(tx: UnboundedSender<Vec<u8>>) -> IntoBackupImpl {
///         IntoBackupImpl { tx }
///     }
/// }
/// ```
pub trait IntoBackup {
    /// Type implementing `Backup<T>`
    type Into;

    /// Consume the object and return struct `Backup` used for saving elements of type T
    fn into_backup<T: Codec + Send + Sync + 'static>(self) -> Self::Into
    where
        Self::Into: Backup<T>;
}

/// A type to generate a structure for reading data that was backed up.
/// Example implementation:
/// ```rust
/// struct ReaderImpl {
///     data: Vec<Vec<u8>>,
///     index: usize,
/// }
///
/// #[async_trait]
/// impl<T: Codec + Send + Sized + Sync> Reader<T> for ReaderImpl {
///     fn next(&mut self) -> Option<T> {
///         match self.data.get_mut(self.index) {
///             Some(encoded) => {
///                 self.index += 1;
///                 T::decode(&mut encoded.as_slice()).ok()
///             }
///             None => None,
///         }
///     }
/// }
///
/// struct IntoReaderImpl {
///     data: Vec<Vec<u8>>,
/// }
///
/// impl IntoReader for IntoReaderImpl {
///     type Into = ReaderImpl;
///
///     fn into_reader<T: Codec + Send + Sized + Sync + 'static>(self) -> Self::Into {
///         ReaderImpl {
///             data: self.data,
///             index: 0,
///         }
///     }
/// }
///
/// impl IntoReaderImpl {
///     fn new(data: Vec<Vec<u8>>) -> IntoReaderImpl {
///         IntoReaderImpl { data }
///     }
/// }
///
/// ```
pub trait IntoReader {
    /// Type implementing `Reader<T>`
    type Into;

    /// Consume the object and return struct `Reader` used for reading saved elements of type T
    fn into_reader<T: Codec + Send + Sync + 'static>(self) -> Self::Into
    where
        Self::Into: Reader<T>;
}
