use futures::Future;
use std::pin::Pin;

/// A handle for waiting the task's completion.
pub type TaskHandle = Pin<Box<dyn Future<Output = Result<(), ()>> + Send>>;

/// An abstraction for an execution engine for Rust's asynchronous tasks.
pub trait SpawnHandle: Clone + Send + 'static {
    /// Run a new task.
    fn spawn(&self, name: &'static str, task: impl Future<Output = ()> + Send + 'static);
    /// Run a new task and returns a handle to it. If there is some error or panic during
    /// execution of the task, the handle should return an error.
    fn spawn_essential(
        &self,
        name: &'static str,
        task: impl Future<Output = ()> + Send + 'static,
    ) -> TaskHandle;
}

/// Abstraction of a task-scheduling logic
///
/// Because the network can be faulty, the task of sending a message must be performed multiple
/// times to ensure that the recipient receives each message.
/// The trait [`TaskScheduler<T>`] describes in what intervals some abstract task of type `T`
/// should be performed.
#[async_trait::async_trait]
pub trait TaskScheduler<T>: Send + Sync {
    fn add_task(&mut self, task: T);
    async fn next_task(&mut self) -> Option<T>;
}
