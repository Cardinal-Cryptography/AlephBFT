use std::collections::HashMap;

use futures::{
    channel::oneshot::{channel, Receiver, Sender},
    future::FusedFuture,
};
use log::debug;

type TerminatorConnection = (Sender<()>, Receiver<()>);

/// Struct that holds connections to offspring and parent components/tasks
/// and enables a clean/synchronized shutdown
pub struct Terminator {
    component_name: &'static str,
    parent_exit: Receiver<()>,
    parent_connection: Option<TerminatorConnection>,
    offspring_connections: HashMap<&'static str, (Sender<()>, TerminatorConnection)>,
}

impl Terminator {
    fn new(
        parent_exit: Receiver<()>,
        parent_connection: Option<TerminatorConnection>,
        component_name: &'static str,
    ) -> Self {
        Self {
            component_name,
            parent_exit,
            parent_connection,
            offspring_connections: HashMap::new(),
        }
    }

    /// Creates a terminator for the root component
    pub fn create_root(exit: Receiver<()>, name: &'static str) -> Self {
        Self::new(exit, None, name)
    }

    /// Get exit channel for current component
    pub fn get_exit(&mut self) -> &mut Receiver<()> {
        &mut self.parent_exit
    }

    /// Add a connection to an offspring component/task
    /// Components should have distinct names
    pub fn add_offspring_connection(&mut self, name: &'static str) -> Terminator {
        let (exit_send, exit_recv) = channel();
        let (sender, offspring_recv) = channel();
        let (offspring_sender, recv) = channel();

        let endpoint = (sender, recv);
        let offspring_endpoint = (offspring_sender, offspring_recv);

        self.offspring_connections
            .insert(name, (exit_send, endpoint));
        Terminator::new(exit_recv, Some(offspring_endpoint), name)
    }

    /// Perform a synchronized shutdown
    pub async fn terminate_sync(self) {
        if !self.parent_exit.is_terminated() {
            debug!(
                target: self.component_name,
                "Terminator has not recieved exit from parent: synchronization canceled.",
            );
            return;
        }

        debug!(
            target: self.component_name,
            "Terminator preparing for shutdown.",
        );

        let mut offspring_senders = Vec::new();
        let mut offspring_receivers = Vec::new();

        // First send exits to descendants
        for (name, (exit, connection)) in self.offspring_connections {
            if exit.send(()).is_err() {
                debug!(target: self.component_name, "{} already stopped.", name);
            }

            let (sender, receiver) = connection;
            offspring_senders.push((sender, name));
            offspring_receivers.push((receiver, name));
        }

        // Make sure that all descendants recieved exit and won't be communicating with other components
        for (receiver, name) in offspring_receivers {
            if receiver.await.is_err() {
                debug!(
                    target: self.component_name,
                    "Terminator failed to receive from {}.",
                    name,
                );
            }
        }

        debug!(
            target: self.component_name,
            "Terminator gathered notifications from descendants.",
        );

        // Notify parent that our subtree is ready for graceful exit
        // and wait for signal that all other components are ready
        if let Some((sender, receiver)) = self.parent_connection {
            if sender.send(()).is_err() {
                debug!(
                    target: self.component_name,
                    "Terminator failed to notify parent component.",
                );
            } else {
                debug!(
                    target: self.component_name,
                    "Terminator notified parent component.",
                );
            }

            if receiver.await.is_err() {
                debug!(
                    target: self.component_name,
                    "Terminator failed to receive from parent component."
                );
            } else {
                debug!(
                    target: self.component_name,
                    "Terminator recieved shutdown permission from parent component."
                );
            }
        }

        // Notify descendants that exiting is now safe
        for (sender, name) in offspring_senders {
            if sender.send(()).is_err() {
                debug!(
                    target: self.component_name,
                    "Terminator failed to notify {}.",
                    name,
                );
            }
        }

        debug!(
            target: self.component_name,
            "Terminator sent permits to descendants: ready to exit.",
        );
    }
}

#[cfg(test)]
mod tests {
    use futures::{channel::oneshot, pin_mut, select, FutureExt};

    use crate::Terminator;

    async fn leaf(mut terminator: Terminator) {
        let _ = terminator.get_exit().await;
        terminator.terminate_sync().await;
    }

    async fn internal_1(mut terminator: Terminator, with_crash: bool) {
        let leaf_handle_1 = leaf(terminator.add_offspring_connection("leaf_1")).fuse();
        let leaf_handle_2 = leaf(terminator.add_offspring_connection("leaf_2")).fuse();

        let leaf_handle_1 = tokio::spawn(leaf_handle_1);
        let leaf_handle_2 = tokio::spawn(leaf_handle_2);

        if with_crash {
            return;
        }

        _ = terminator.get_exit().await;
        terminator.terminate_sync().await;

        let _ = leaf_handle_1.await;
        let _ = leaf_handle_2.await;
    }

    async fn internal_2(mut terminator: Terminator, with_crash: bool) {
        let leaf_handle_1 = leaf(terminator.add_offspring_connection("leaf_1")).fuse();
        let leaf_handle_2 = leaf(terminator.add_offspring_connection("leaf_2")).fuse();
        let internal_handle = internal_1(
            terminator.add_offspring_connection("internal_1"),
            with_crash,
        )
        .fuse();

        pin_mut!(leaf_handle_1);
        pin_mut!(leaf_handle_2);
        pin_mut!(internal_handle);

        select! {
            _ = leaf_handle_1 => assert!(with_crash, "leaf_1 crashed when it wasn't supposed to"),
            _ = leaf_handle_2 => assert!(with_crash, "leaf_2 crashed when it wasn't supposed to"),
            _ = internal_handle => assert!(with_crash, "internal_1 crashed when it wasn't supposed to"),
            _ = terminator.get_exit() => assert!(!with_crash, "exited when we expected internal crash"),
        }

        let terminator_handle = terminator.terminate_sync().fuse();
        pin_mut!(terminator_handle);

        loop {
            select! {
                _ = leaf_handle_1 => {},
                _ = leaf_handle_2 => {},
                _ = internal_handle => {},
                _ = terminator_handle => {},
                complete => break,
            }
        }
    }

    async fn root_component(mut terminator: Terminator, with_crash: bool) {
        let leaf_handle = leaf(terminator.add_offspring_connection("leaf")).fuse();
        let internal_handle = internal_2(
            terminator.add_offspring_connection("internal_2"),
            with_crash,
        )
        .fuse();

        pin_mut!(leaf_handle);
        pin_mut!(internal_handle);

        select! {
            _ = leaf_handle => assert!(with_crash, "leaf crashed when it wasn't supposed to"),
            _ = internal_handle => assert!(with_crash, "internal_2 crashed when it wasn't supposed to"),
            _ = terminator.get_exit() => assert!(!with_crash, "exited when we expected internal crash"),
        }

        let terminator_handle = terminator.terminate_sync().fuse();
        pin_mut!(terminator_handle);

        loop {
            select! {
                _ = leaf_handle => {},
                _ = internal_handle => {},
                _ = terminator_handle => {},
                complete => break,
            }
        }
    }

    #[tokio::test]
    async fn simple_exit() {
        let (exit_tx, exit_rx) = oneshot::channel();
        let terminator = Terminator::create_root(exit_rx, "root");
        exit_tx.send(()).expect("should send");
        root_component(terminator, false).await;
    }

    #[tokio::test]
    async fn component_crash() {
        let (_exit_tx, exit_rx) = oneshot::channel();
        let terminator = Terminator::create_root(exit_rx, "root");
        root_component(terminator, true).await;
    }
}
