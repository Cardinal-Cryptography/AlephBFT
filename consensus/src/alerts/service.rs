use crate::{
    alerts::{
        handler::Handler, io::IO, Alert, AlertConfig, AlertMessage, AlerterResponse,
        ForkingNotification, NetworkMessage,
    },
    Data, Hasher, MultiKeychain, Multisigned, Receiver, Recipient, Sender, Terminator,
};
use aleph_bft_rmc::{DoublingDelayScheduler, Message as RmcMessage, ReliableMulticast};
use futures::{channel::mpsc, FutureExt, StreamExt};
use log::{debug, error, warn};
use std::time;

pub struct Service<H: Hasher, D: Data, MK: MultiKeychain> {
    handler: Handler<H, D, MK>,
    io: IO<H, D, MK>,
}

impl<H: Hasher, D: Data, MK: MultiKeychain> Service<H, D, MK> {
    pub fn new(
        keychain: MK,
        messages_for_network: Sender<(NetworkMessage<H, D, MK>, Recipient)>,
        messages_from_network: Receiver<NetworkMessage<H, D, MK>>,
        notifications_for_units: Sender<ForkingNotification<H, D, MK::Signature>>,
        alerts_from_units: Receiver<Alert<H, D, MK::Signature>>,
        config: AlertConfig,
    ) -> Service<H, D, MK> {
        let n_members = config.n_members;
        let handler = Handler::new(keychain.clone(), config);
        let (messages_for_rmc, messages_from_us) = mpsc::unbounded();
        let (messages_for_us, messages_from_rmc) = mpsc::unbounded();

        let io = IO {
            messages_for_network,
            messages_from_network,
            notifications_for_units,
            alerts_from_units,
            rmc: ReliableMulticast::new(
                messages_from_us,
                messages_for_us,
                keychain,
                n_members,
                DoublingDelayScheduler::new(time::Duration::from_millis(500)),
            ),
            messages_from_rmc,
            messages_for_rmc,
            alerter_index: handler.index(),
        };

        Service { handler, io }
    }

    pub fn handle_message_from_network(
        &mut self,
        message: AlertMessage<H, D, MK::Signature, MK::PartialMultisignature>,
    ) {
        match self.handler.on_message(message) {
            Ok(Some(AlerterResponse::ForkAlert(alert, recipient))) => {
                self.io.send_message_for_network(
                    AlertMessage::ForkAlert(alert),
                    recipient,
                    &mut self.handler.exiting,
                );
            }
            Ok(Some(AlerterResponse::AlertRequest(hash, peer))) => {
                let message = AlertMessage::AlertRequest(self.handler.index(), hash);
                self.io
                    .send_message_for_network(message, peer, &mut self.handler.exiting);
            }
            Ok(Some(AlerterResponse::RmcMessage(message))) => {
                if self.io.messages_for_rmc.unbounded_send(message).is_err() {
                    warn!(target: "AlephBFT-alerter", "{:?} Channel with messages for rmc should be open", self.handler.index());
                    self.handler.exiting = true;
                }
            }
            Ok(Some(AlerterResponse::ForkResponse(maybe_notification, hash))) => {
                self.io.rmc.start_rmc(hash);
                if let Some(notification) = maybe_notification {
                    self.io
                        .send_notification_for_units(notification, &mut self.handler.exiting);
                }
            }
            Ok(None) => {}
            Err(error) => debug!(target: "AlephBFT-alerter", "{}", error),
        }
    }

    pub fn handle_alert_from_runway(&mut self, alert: Alert<H, D, MK::Signature>) {
        let (message, recipient, hash) = self.handler.on_own_alert(alert);
        self.io
            .send_message_for_network(message, recipient, &mut self.handler.exiting);
        self.io.rmc.start_rmc(hash);
    }

    pub fn handle_message_from_rmc(
        &mut self,
        message: RmcMessage<H::Hash, MK::Signature, MK::PartialMultisignature>,
    ) {
        self.io
            .rmc_message_to_network(message, &mut self.handler.exiting)
    }

    pub fn handle_multisigned(&mut self, multisigned: Multisigned<H::Hash, MK>) {
        match self.handler.alert_confirmed(multisigned) {
            Ok(notification) => self
                .io
                .send_notification_for_units(notification, &mut self.handler.exiting),
            Err(error) => warn!(target: "AlephBFT-alerter", "{}", error),
        }
    }

    pub async fn run(&mut self, mut terminator: Terminator) {
        loop {
            futures::select! {
                message = self.io.messages_from_network.next() => match message {
                    Some(message) => self.handle_message_from_network(message),
                    None => {
                        error!(target: "AlephBFT-alerter", "{:?} Message stream closed.", self.handler.index());
                        break;
                    }
                },
                alert = self.io.alerts_from_units.next() => match alert {
                    Some(alert) => self.handle_alert_from_runway(alert),
                    None => {
                        error!(target: "AlephBFT-alerter", "{:?} Alert stream closed.", self.handler.index());
                        break;
                    }
                },
                message = self.io.messages_from_rmc.next() => match message {
                    Some(message) => self.handle_message_from_rmc(message),
                    None => {
                        error!(target: "AlephBFT-alerter", "{:?} RMC message stream closed.", self.handler.index());
                        break;
                    }
                },
                multisigned = self.io.rmc.next_multisigned_hash().fuse() => self.handle_multisigned(multisigned),
                _ = &mut terminator.get_exit() => {
                    debug!(target: "AlephBFT-alerter", "{:?} received exit signal", self.handler.index());
                    self.handler.exiting = true;
                },
            }
            if self.handler.exiting {
                debug!(target: "AlephBFT-alerter", "{:?} Alerter decided to exit.", self.handler.index());
                terminator.terminate_sync().await;
                break;
            }
        }
    }
}
