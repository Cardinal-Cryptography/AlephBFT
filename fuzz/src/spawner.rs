use aleph_bft::{DelayConfig, SpawnHandle, TaskHandle};
use aleph_bft_mock::Spawner as MockSpawner;
use futures::{
    task::{Context, Poll},
    Future,
};
use futures_timer::Delay;
use parking_lot::Mutex;
use std::{
    pin::Pin,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};
use tokio::task::yield_now;

#[derive(Clone)]
pub struct Spawner {
    spawner: Arc<MockSpawner>,
    idle_mx: Arc<Mutex<()>>,
    wake_flag: Arc<AtomicBool>,
    delay: Duration,
}

struct SpawnFuture<T> {
    task: Pin<Box<T>>,
    wake_flag: Arc<AtomicBool>,
}

impl<T> SpawnFuture<T> {
    fn new(task: T, wake_flag: Arc<AtomicBool>) -> Self {
        SpawnFuture {
            task: Box::pin(task),
            wake_flag,
        }
    }
}

impl<T: Future<Output = ()> + Send + 'static> Future for SpawnFuture<T> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.wake_flag
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Future::poll(self.task.as_mut(), cx)
    }
}

impl SpawnHandle for Spawner {
    fn spawn(&self, name: &'static str, task: impl Future<Output = ()> + Send + 'static) {
        // NOTE this is magic - member creates too much background noise
        if name == "member" {
            self.spawner.spawn(name, task)
        } else {
            let wrapped = self.wrap_task(task);
            self.spawner.spawn(name, wrapped)
        }
    }

    fn spawn_essential(
        &self,
        name: &'static str,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle {
        // NOTE this is magic - member creates too much background noise
        if name == "member" {
            self.spawner.spawn_essential(name, task)
        } else {
            let wrapped = self.wrap_task(task);
            self.spawner.spawn_essential(name, wrapped)
        }
    }
}

impl Spawner {
    pub async fn wait(&self) {
        self.spawner.wait().await
    }

    pub fn new(delay_config: &DelayConfig) -> Self {
        Spawner {
            spawner: Arc::new(MockSpawner::new()),
            idle_mx: Arc::new(Mutex::new(())),
            wake_flag: Arc::new(AtomicBool::new(false)),
            // NOTE this is a magic value used to allow fuzzing tests be able to process enough messages from the PlaybackNetwork
            delay: 10 * delay_config.tick_interval,
        }
    }

    pub async fn wait_idle(&self) {
        let _ = self.idle_mx.lock();

        self.wake_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        loop {
            Delay::new(self.delay).await;
            yield_now().await;
            // try to verify if any other task was attempting to wake up
            // it assumes that we are using a single-threaded runtime for scheduling our Futures
            if self
                .wake_flag
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                break;
            }
        }
        self.wake_flag
            .store(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn wrap_task(
        &self,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> impl Future<Output = ()> + Send + 'static {
        SpawnFuture::new(task, self.wake_flag.clone())
    }
}
