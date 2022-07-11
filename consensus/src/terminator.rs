use futures::channel::oneshot;
use log::debug;

pub type TerminatorConnection = (oneshot::Sender<()>, oneshot::Receiver<()>);
pub type ShutdownConnection = (oneshot::Receiver<()>, Option<TerminatorConnection>);

/// Struct that holds connections to offspring and parent components/tasks
/// and enables a clean/synchronized shutdown
pub struct Terminator {
    component_name: String,
    parent_connection: Option<TerminatorConnection>,
    offspring_connections: Vec<TerminatorConnection>,
    offspring_names: Vec<String>,
}

impl Terminator {
    pub fn new(parent_connection: Option<TerminatorConnection>, component_name: &str) -> Self {
        Self {
            component_name: String::from(component_name),
            parent_connection,
            offspring_connections: Vec::<_>::new(),
            offspring_names: Vec::<_>::new(),
        }
    }

    /// Add a connection to an offspring component/task
    pub fn add_offspring_connection(&mut self, name: &str) -> TerminatorConnection {
        let (sender, offspring_recv) = oneshot::channel();
        let (offspring_sender, recv) = oneshot::channel();

        let endpoint = (sender, recv);
        let offspring_endpoint = (offspring_sender, offspring_recv);

        self.offspring_connections.push(endpoint);
        self.offspring_names.push(name.to_string());
        offspring_endpoint
    }

    /// Perform a synchronized shutdown
    pub async fn terminate_sync(self) {
        let (offspring_senders, offspring_receivers): (Vec<_>, Vec<_>) =
            self.offspring_connections.into_iter().unzip();
        let offspring_senders: Vec<_> = offspring_senders
            .into_iter()
            .zip(self.offspring_names.clone().into_iter())
            .collect();
        let offspring_receivers: Vec<_> = offspring_receivers
            .into_iter()
            .zip(self.offspring_names.clone().into_iter())
            .collect();

        debug!(
            target: &self.component_name,
            "Terminator preparing for shutdown.",
        );

        // Make sure that all descendants recieved exit and won't be communicating with other components
        for (receiver, name) in offspring_receivers {
            if receiver.await.is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to receive from {}. Clean shutdown unsuccessful.",
                    name,
                );
                return;
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
                    "Terminator failed to notify parent component. Clean shutdown unsuccessful.",
                );
            }

            debug!(
                target: &self.component_name,
                "Terminator notified parent component.",
            );

            if receiver.await.is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to receive from parent component. Clean shutdown unsuccessful."
                );
                return;
            }

            debug!(
                target: &self.component_name,
                "Terminator recieved shutdown permission from parent component."
            );
        }

        // Notify descendants that exiting is now safe
        for (sender, name) in offspring_senders {
            if sender.send(()).is_err() {
                debug!(
                    target: &self.component_name,
                    "Terminator failed to notify {}. Clean exit unsuccessful.",
                    name,
                );
                return;
            }
        }

        debug!(
            target: &self.component_name,
            "Terminator sent permits to descendants: ready to exit.",
        );
    }
}
