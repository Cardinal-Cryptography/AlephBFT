use crate::{
    alerts::{
        handler::Handler, Alert, AlertMessage, AlerterResponse, ForkingNotification, NetworkMessage,
    },
    runway::BackupItem,
    Data, Hasher, MultiKeychain, NodeCount, Receiver, Recipient, Sender, Terminator,
};
use aleph_bft_rmc::{DoublingDelayScheduler, Message as RmcMessage, ReliableMulticast};
use futures::{channel::mpsc, FutureExt, StreamExt};
use log::{debug, error, warn};
use std::time;

const LOG_TARGET: &str = "AlephBFT-alerter";

pub struct Service<H: Hasher, D: Data, MK: MultiKeychain> {
    messages_for_network: Sender<(NetworkMessage<H, D, MK>, Recipient)>,
    messages_from_network: Receiver<NetworkMessage<H, D, MK>>,
    notifications_for_units: Sender<ForkingNotification<H, D, MK::Signature>>,
    alerts_from_units: Receiver<Alert<H, D, MK::Signature>>,
    rmc: ReliableMulticast<H::Hash, MK>,
    messages_for_rmc: Sender<RmcMessage<H::Hash, MK::Signature, MK::PartialMultisignature>>,
    messages_from_rmc: Receiver<RmcMessage<H::Hash, MK::Signature, MK::PartialMultisignature>>,
    keychain: MK,
    items_for_backup: Sender<BackupItem<H, D, MK>>,
    responses_from_backup: Receiver<BackupItem<H, D, MK>>,
    exiting: bool,
}

impl<H: Hasher, D: Data, MK: MultiKeychain> Service<H, D, MK> {
    pub fn new(
        keychain: MK,
        messages_for_network: Sender<(NetworkMessage<H, D, MK>, Recipient)>,
        messages_from_network: Receiver<NetworkMessage<H, D, MK>>,
        notifications_for_units: Sender<ForkingNotification<H, D, MK::Signature>>,
        alerts_from_units: Receiver<Alert<H, D, MK::Signature>>,
        n_members: NodeCount,
        items_for_backup: Sender<BackupItem<H, D, MK>>,
        responses_from_backup: Receiver<BackupItem<H, D, MK>>,
    ) -> Service<H, D, MK> {
        let (messages_for_rmc, messages_from_us) = mpsc::unbounded();
        let (messages_for_us, messages_from_rmc) = mpsc::unbounded();

        let rmc = ReliableMulticast::new(
            messages_from_us,
            messages_for_us,
            keychain.clone(),
            n_members,
            DoublingDelayScheduler::new(time::Duration::from_millis(500)),
        );

        Service {
            messages_for_network,
            messages_from_network,
            notifications_for_units,
            alerts_from_units,
            rmc,
            messages_for_rmc,
            messages_from_rmc,
            keychain,
            items_for_backup,
            responses_from_backup,
            exiting: false,
        }
    }

    fn rmc_message_to_network(
        &mut self,
        message: RmcMessage<H::Hash, MK::Signature, MK::PartialMultisignature>,
    ) {
        self.send_message_for_network(
            AlertMessage::RmcMessage(self.keychain.index(), message),
            Recipient::Everyone,
        );
    }

    fn send_notification_for_units(
        &mut self,
        notification: ForkingNotification<H, D, MK::Signature>,
    ) {
        if self
            .notifications_for_units
            .unbounded_send(notification)
            .is_err()
        {
            warn!(
                target: LOG_TARGET,
                "{:?} Channel with forking notifications should be open",
                self.keychain.index()
            );
            self.exiting = true;
        }
    }

    fn send_message_for_network(
        &mut self,
        message: AlertMessage<H, D, MK::Signature, MK::PartialMultisignature>,
        recipient: Recipient,
    ) {
        if self
            .messages_for_network
            .unbounded_send((message, recipient))
            .is_err()
        {
            warn!(
                target: LOG_TARGET,
                "{:?} Channel with notifications for network should be open",
                self.keychain.index()
            );
            self.exiting = true;
        }
    }

    fn handle_message_from_network(
        &mut self,
        handler: &mut Handler<H, D, MK>,
        message: AlertMessage<H, D, MK::Signature, MK::PartialMultisignature>,
    ) {
        match handler.on_message(message) {
            Ok(Some(AlerterResponse::ForkAlert(alert, recipient))) => {
                self.send_item_to_backup(BackupItem::NetworkAlert(alert, recipient));
            }
            Ok(Some(AlerterResponse::AlertRequest(hash, peer))) => {
                let message = AlertMessage::AlertRequest(self.keychain.index(), hash);
                self.send_message_for_network(message, peer);
            }
            Ok(Some(AlerterResponse::RmcMessage(message))) => {
                if self.messages_for_rmc.unbounded_send(message).is_err() {
                    warn!(
                        target: LOG_TARGET,
                        "{:?} Channel with messages for rmc should be open",
                        self.keychain.index()
                    );
                    self.exiting = true;
                }
            }
            Ok(Some(AlerterResponse::ForkResponse(maybe_notification, hash))) => {
                self.rmc.start_rmc(hash);
                if let Some(notification) = maybe_notification {
                    self.send_notification_for_units(notification);
                }
            }
            Ok(None) => {}
            Err(error) => debug!(target: LOG_TARGET, "{}", error),
        }
    }

    fn handle_response_from_backup(
        &mut self,
        handler: &mut Handler<H, D, MK>,
        response: BackupItem<H, D, MK>,
    ) {
        match response {
            BackupItem::OwnAlert(alert) => {
                let (message, recipient, hash) = handler.on_own_alert(alert);
                self.send_message_for_network(message, recipient);
                self.rmc.start_rmc(hash);
            }
            BackupItem::NetworkAlert(alert, recipient) => {
                self.send_message_for_network(AlertMessage::ForkAlert(alert), recipient);
            }
            BackupItem::MultiSignature(multisigned) => match handler.alert_confirmed(multisigned) {
                Ok(notification) => self.send_notification_for_units(notification),
                Err(error) => warn!(target: LOG_TARGET, "{}", error),
            },
            _ => {}
        }
    }

    fn send_item_to_backup(&mut self, item: BackupItem<H, D, MK>) {
        if self.items_for_backup.unbounded_send(item).is_err() {
            warn!(
                target: LOG_TARGET,
                "{:?} Channel for passing items to backup was closed",
                self.keychain.index()
            );
            self.exiting = true;
        }
    }

    pub async fn run(&mut self, mut handler: Handler<H, D, MK>, mut terminator: Terminator) {
        loop {
            futures::select! {
                message = self.messages_from_network.next() => match message {
                    Some(message) => self.handle_message_from_network(&mut handler, message),
                    None => {
                        error!(target: LOG_TARGET, "{:?} Message stream closed.", self.keychain.index());
                        break;
                    }
                },
                alert = self.alerts_from_units.next() => match alert {
                    Some(alert) => self.send_item_to_backup(BackupItem::OwnAlert(alert)),
                    None => {
                        error!(target: LOG_TARGET, "{:?} Alert stream closed.", self.keychain.index());
                        break;
                    }
                },
                message = self.messages_from_rmc.next() => match message {
                    Some(message) => self.rmc_message_to_network(message),
                    None => {
                        error!(target: LOG_TARGET, "{:?} RMC message stream closed.", self.keychain.index());
                        break;
                    }
                },
                multisigned = self.rmc.next_multisigned_hash().fuse() => self.send_item_to_backup(BackupItem::MultiSignature(multisigned)),
                response = self.responses_from_backup.next() => match response {
                    Some(item) => self.handle_response_from_backup(&mut handler, item),
                    None => {
                        error!(target: LOG_TARGET, "Receiver of responses from backup closed");
                        break;
                    }
                },
                _ = terminator.get_exit().fuse() => {
                    debug!(target: LOG_TARGET, "{:?} received exit signal", self.keychain.index());
                    self.exiting = true;
                },
            }
            if self.exiting {
                debug!(
                    target: LOG_TARGET,
                    "{:?} Alerter decided to exit.",
                    self.keychain.index()
                );
                terminator.terminate_sync().await;
                break;
            }
        }
    }
}
