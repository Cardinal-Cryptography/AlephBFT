use std::collections::HashMap;

use futures::channel::oneshot::{Receiver, Sender, channel};
use log::debug;

pub type TerminatorConnection = (Sender<()>, Receiver<()>);
pub type ShutdownConnection = (Receiver<()>, Option<TerminatorConnection>);

/// Struct that holds connections to offspring and parent components/tasks
/// and enables a clean/synchronized shutdown
pub struct Terminator {
    component_name: &'static str,
    parent_exit: Receiver<()>,
    parent_connection: Option<TerminatorConnection>,
    offspring_exits: HashMap<&'static str, Sender<()>>,
    offspring_connections: HashMap<&'static str, TerminatorConnection>,
}

impl Terminator {
    pub fn new(
        parent_shutdown_connection: ShutdownConnection,
        component_name: &'static str,
    ) -> Self {
        Self {
            component_name,
            parent_exit: parent_shutdown_connection.0,
            parent_connection: parent_shutdown_connection.1,
            offspring_exits: HashMap::<_, _>::new(),
            offspring_connections: HashMap::<_, _>::new(),
        }
    }

    /// Get exit channel for current component
    pub fn get_exit(&mut self) -> &mut Receiver<()> {
        &mut self.parent_exit
    }

    /// Add a connection to an offspring component/task
    pub fn add_offspring_connection(&mut self, name: &'static str) -> Terminator {
        let (exit_send, exit_recv) = channel();
        self.offspring_exits.insert(name, exit_send);

        let (sender, offspring_recv) = channel();
        let (offspring_sender, recv) = channel();

        let endpoint = (sender, recv);
        let offspring_endpoint = (offspring_sender, offspring_recv);

        self.offspring_connections.insert(name, endpoint);
        Terminator::new((exit_recv, Some(offspring_endpoint)), name)
    }

    /// Perform a synchronized shutdown
    pub async fn terminate_sync(self) {
        debug!(
            target: &self.component_name,
            "Terminator preparing for shutdown.",
        );

        // First send exits to descendants
        for (name, exit) in self.offspring_exits {
            if exit.send(()).is_err() {
                debug!(target: self.component_name, "{} already stopped.", name);
            }
        }

        let mut offspring_senders = Vec::<_>::new();
        let mut offspring_receivers = Vec::<_>::new();
        for (name, connection) in self.offspring_connections {
            let (sender, receiver) = connection;
            offspring_senders.push((sender, name));
            offspring_receivers.push((receiver, name));
        }

        // Make sure that all descendants recieved exit and won't be communicating with other components
        for (receiver, name) in offspring_receivers {
            if receiver.await.is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to receive from {}.",
                    name,
                );
            }
        }

        debug!(
            target: &self.component_name,
            "Terminator gathered notifications from descendants.",
        );

        // Notify parent that our subtree is ready for graceful exit
        // and wait for signal that all other components are ready
        if self.parent_connection.is_some() {
            let (sender, receiver) = self.parent_connection.unwrap();
            if sender.send(()).is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to notify parent component.",
                );
            } else {
                debug!(
                    target: &self.component_name,
                    "Terminator notified parent component.",
                );
            }

            if receiver.await.is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to receive from parent component."
                );
            } else {
                debug!(
                    target: &self.component_name,
                    "Terminator recieved shutdown permission from parent component."
                );
            }
        }

        // Notify descendants that exiting is now safe
        for (sender, name) in offspring_senders {
            if sender.send(()).is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to notify {}.",
                    name,
                );
            }
        }

        debug!(
            target: &self.component_name,
            "Terminator sent permits to descendants: ready to exit.",
        );
    }
}
