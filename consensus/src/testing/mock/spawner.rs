use crate::{SpawnHandle, TaskHandle};
use futures::{channel::oneshot, Future};

#[derive(Clone)]
pub struct Spawner {}

impl SpawnHandle for Spawner {
    fn spawn(&self, _name: &str, task: impl Future<Output = ()> + Send + 'static) {
        tokio::spawn(task);
    }

    fn spawn_essential(
        &self,
        _: &str,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle {
        let (res_tx, res_rx) = oneshot::channel();
        tokio::spawn(async move {
            task.await;
            res_tx.send(()).expect("We own the rx.");
        });
        Box::pin(async move { res_rx.await.map_err(|_| ()) })
    }
}

impl Spawner {
    pub fn new() -> Self {
        Spawner {}
    }
}

impl Default for Spawner {
    fn default() -> Self {
        Spawner::new()
    }
}
