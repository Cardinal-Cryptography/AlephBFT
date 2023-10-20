use codec::{Decode, Encode};
use std::fmt::Debug;

pub use injector::{BackupInjector, InitialState};
pub use loader::BackupLoader;
pub use saver::BackupSaver;

use crate::{alerts::AlertData, units::UncheckedSignedUnit, Data, Hasher, MultiKeychain};

mod injector;
mod loader;
mod saver;

#[derive(Clone, Debug, Decode, Encode, PartialEq)]
pub enum BackupItem<H: Hasher, D: Data, MK: MultiKeychain> {
    Unit(UncheckedSignedUnit<H, D, MK::Signature>),
    AlertData(AlertData<H, D, MK>),
}
