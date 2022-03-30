#![cfg(test)]
mod alerts;
mod byzantine;
mod consensus;
mod crash;
mod creation;
mod dag;
pub(crate) mod mock;
mod unreliable;

use futures::channel::oneshot;

pub fn init_log() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::max())
        .is_test(true)
        .try_init();
}

pub fn complete_oneshot<T: std::fmt::Debug>(t: T) -> oneshot::Receiver<T> {
    let (tx, rx) = oneshot::channel();
    tx.send(t).unwrap();
    rx
}
