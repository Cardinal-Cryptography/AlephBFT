use std::collections::HashMap;

use futures::channel::oneshot;
use log::debug;

pub type TerminatorConnection = (oneshot::Sender<()>, oneshot::Receiver<()>);
pub type ShutdownConnection = (oneshot::Receiver<()>, Option<TerminatorConnection>);

/// Struct that holds connections to offspring and parent components/tasks
/// and enables a clean/synchronized shutdown
pub struct Terminator {
    component_name: &'static str,
    parent_connection: Option<TerminatorConnection>,
    offspring_connections: HashMap<&'static str, TerminatorConnection>,
}

impl Terminator {
    pub fn new(
        parent_connection: Option<TerminatorConnection>,
        component_name: &'static str,
    ) -> Self {
        Self {
            component_name,
            parent_connection,
            offspring_connections: HashMap::<_, _>::new(),
        }
    }

    /// Add a connection to an offspring component/task
    pub fn add_offspring_connection(&mut self, name: &'static str) -> TerminatorConnection {
        let (sender, offspring_recv) = oneshot::channel();
        let (offspring_sender, recv) = oneshot::channel();

        let endpoint = (sender, recv);
        let offspring_endpoint = (offspring_sender, offspring_recv);

        self.offspring_connections.insert(name, endpoint);
        offspring_endpoint
    }

    /// Perform a synchronized shutdown
    pub async fn terminate_sync(self) {
        let mut offspring_senders = Vec::<_>::new();
        let mut offspring_receivers = Vec::<_>::new();
        for (name, connection) in self.offspring_connections {
            let (sender, receiver) = connection;
            offspring_senders.push((sender, name));
            offspring_receivers.push((receiver, name));
        }

        debug!(
            target: &self.component_name,
            "Terminator preparing for shutdown.",
        );

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
