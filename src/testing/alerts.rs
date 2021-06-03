use crate::{
    alerts::{run, Alert, AlertConfig, AlertMessage, ForkProof, ForkingNotification},
    network::Recipient,
    nodes::{NodeCount, NodeIndex},
    rmc::Message as RmcMessage,
    testing::mock::{Data, Hasher64, KeyBox, PartialMultisignature, Signature},
    units::{ControlHash, FullUnit, PreUnit, UnitCoord},
    Index, Indexed, NodeMap, Round, Signable, Signed, UncheckedSigned,
};
use futures::{
    channel::{mpsc, oneshot},
    FutureExt, StreamExt,
};
use futures_timer::Delay;
use log::debug;
use std::{collections::HashMap, hash::Hash, time::Duration};

type TestMessage = AlertMessage<Hasher64, Data, Signature, PartialMultisignature>;
type TestAlert = Alert<Hasher64, Data, Signature>;
type TestNotification = ForkingNotification<Hasher64, Data, Signature>;
type TestForkProof = ForkProof<Hasher64, Data, Signature>;
type TestFullUnit = FullUnit<Hasher64, Data>;

enum Input {
    Incoming(TestMessage),
    Alert(TestAlert),
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum Output {
    Outgoing(TestMessage, Recipient),
    Notification(TestNotification),
}

struct Scenario {
    n_members: NodeCount,
    inputs: Vec<Input>,
    expected: HashMap<Output, usize>,
}

impl Scenario {
    fn new(n_members: NodeCount) -> Self {
        Scenario {
            n_members,
            inputs: Vec::new(),
            expected: HashMap::new(),
        }
    }

    fn incoming_message(&mut self, message: TestMessage) -> &mut Self {
        self.inputs.push(Input::Incoming(message));
        self
    }

    fn incoming_alert(&mut self, alert: TestAlert) -> &mut Self {
        self.inputs.push(Input::Alert(alert));
        self
    }

    fn outgoing_message(&mut self, message: TestMessage, recipient: Recipient) -> &mut Self {
        *self
            .expected
            .entry(Output::Outgoing(message, recipient))
            .or_insert(0) += 1;
        self
    }

    fn outgoing_notification(&mut self, notification: TestNotification) -> &mut Self {
        *self
            .expected
            .entry(Output::Notification(notification))
            .or_insert(0) += 1;
        self
    }

    fn check_output(&mut self, output: Output) {
        match self.expected.get_mut(&output) {
            Some(count) => *count -= 1,
            None => debug!("Possibly unnecessary {:?} emitted by alerter.", output),
        }
        if self.expected.get(&output) == Some(&0) {
            self.expected.remove(&output);
        }
    }

    async fn test(mut self, keychain: KeyBox) {
        let (messages_for_network, mut messages_from_alerter) = mpsc::unbounded();
        let (messages_for_alerter, messages_from_network) = mpsc::unbounded();
        let (notifications_for_units, mut notifications_from_alerter) = mpsc::unbounded();
        let (alerts_for_alerter, alerts_from_units) = mpsc::unbounded();
        let (exit_alerter, exit) = oneshot::channel();
        tokio::spawn(run(
            keychain,
            messages_for_network,
            messages_from_network,
            notifications_for_units,
            alerts_from_units,
            AlertConfig {
                max_units_per_alert: 43,
                n_members: self.n_members,
                session_id: 0,
            },
            exit,
        ));

        use Input::*;
        use Output::*;

        for i in &self.inputs {
            match i {
                Incoming(message) => messages_for_alerter
                    .unbounded_send(message.clone())
                    .expect("the message channel works"),
                Alert(alert) => alerts_for_alerter
                    .unbounded_send(alert.clone())
                    .expect("the alert channel works"),
            }
        }
        while !self.expected.is_empty() {
            tokio::select! {
                message = messages_from_alerter.next() => match message {
                    Some((message, recipient)) => self.check_output(Outgoing(message, recipient)),
                    None => panic!("Message stream unexpectedly closed."),
                },
                notification = notifications_from_alerter.next() => match notification {
                    Some(notification) => self.check_output(Output::Notification(notification)),
                    None => panic!("Notification stream unexpectedly closed."),
                },
            }
            debug!("Remaining items: {:?}.", self.expected);
        }
        exit_alerter
            .send(())
            .expect("exit channel shouldn't be closed");
    }
}

struct TestCase {
    keychains: Vec<KeyBox>,
    scenario: Scenario,
}

impl TestCase {
    fn new(n_members: NodeCount) -> Self {
        let mut keychains = Vec::new();
        for i in 0..n_members.0 {
            keychains.push(KeyBox::new(n_members, NodeIndex(i)))
        }
        Self {
            keychains,
            scenario: Scenario::new(n_members),
        }
    }

    fn scenario(&mut self) -> &mut Scenario {
        &mut self.scenario
    }

    fn keychain(&self, node: NodeIndex) -> &KeyBox {
        &self.keychains[node.0]
    }

    async fn unchecked_signed<T: Signable + Index>(
        &self,
        to_sign: T,
        signer: NodeIndex,
    ) -> UncheckedSigned<T, Signature> {
        Signed::sign(to_sign, self.keychain(signer)).await.into()
    }

    async fn indexed_unchecked_signed<T: Signable>(
        &self,
        to_sign: T,
        signer: NodeIndex,
    ) -> UncheckedSigned<Indexed<T>, Signature> {
        Signed::sign(Indexed::new(to_sign, signer), self.keychain(signer))
            .await
            .into()
    }

    fn full_unit(&self, forker: NodeIndex, round: Round, variant: u32) -> TestFullUnit {
        FullUnit::new(
            PreUnit::new(
                forker,
                round,
                ControlHash::new(&NodeMap::new_with_len(self.scenario.n_members)),
            ),
            Data::new(UnitCoord::new(round, forker), variant),
            0,
        )
    }

    async fn fork_proof(&self, forker: NodeIndex, round: Round) -> TestForkProof {
        let u0 = self
            .unchecked_signed(self.full_unit(forker, round, 0), forker)
            .await;
        let u1 = self
            .unchecked_signed(self.full_unit(forker, round, 1), forker)
            .await;
        (u0, u1)
    }

    fn alert(&self, sender: NodeIndex, proof: TestForkProof) -> TestAlert {
        Alert::new(sender, proof, Vec::new())
    }

    async fn run(self, run_as: NodeIndex) {
        let keychain = self.keychain(run_as).clone();
        let mut timeout = Delay::new(Duration::from_millis(500)).fuse();
        futures::select! {
            _ = self.scenario.test(keychain).fuse() => {},
            _ = timeout => {
                panic!("Alerter took too long to emit expected items.");
            },
        }
    }
}

#[tokio::test]
async fn distributes_alert_from_units() {
    let n_members = NodeCount(4);
    let own_index = NodeIndex(0);
    let forker = NodeIndex(3);
    let mut test_case = TestCase::new(n_members);
    let alert = test_case.alert(own_index, test_case.fork_proof(forker, 0).await);
    let signed_alert = test_case.unchecked_signed(alert.clone(), own_index).await;
    test_case
        .scenario()
        .incoming_alert(alert.clone())
        .outgoing_message(AlertMessage::ForkAlert(signed_alert), Recipient::Everyone);
    test_case.run(own_index).await;
}

#[tokio::test]
async fn reacts_to_correctly_incoming_alert() {
    let n_members = NodeCount(4);
    let own_index = NodeIndex(0);
    let alerter_index = NodeIndex(1);
    let forker = NodeIndex(3);
    let mut test_case = TestCase::new(n_members);
    let fork_proof = test_case.fork_proof(forker, 0).await;
    let alert = test_case.alert(alerter_index, fork_proof.clone());
    let signed_alert_hash = test_case
        .indexed_unchecked_signed(Signable::hash(&alert), own_index)
        .await;
    let signed_alert = test_case
        .unchecked_signed(alert.clone(), alerter_index)
        .await;
    test_case
        .scenario()
        .incoming_message(AlertMessage::ForkAlert(signed_alert))
        .outgoing_notification(ForkingNotification::Forker(fork_proof));
    for i in 1..n_members.0 {
        test_case.scenario().outgoing_message(
            AlertMessage::RmcMessage(own_index, RmcMessage::SignedHash(signed_alert_hash.clone())),
            Recipient::Node(NodeIndex(i)),
        );
    }
    test_case.run(own_index).await;
}

#[tokio::test]
async fn notifies_about_finished_alert() {
    let n_members = NodeCount(4);
    let own_index = NodeIndex(0);
    let alerter_index = NodeIndex(1);
    let forker = NodeIndex(3);
    let mut test_case = TestCase::new(n_members);
    let fork_proof = test_case.fork_proof(forker, 0).await;
    let alert = test_case.alert(alerter_index, fork_proof.clone());
    let alert_hash = Signable::hash(&alert);
    let signed_alert = test_case
        .unchecked_signed(alert.clone(), alerter_index)
        .await;
    test_case
        .scenario()
        .incoming_message(AlertMessage::ForkAlert(signed_alert))
        .outgoing_notification(ForkingNotification::Forker(fork_proof));
    for i in 1..n_members.0 - 1 {
        let node_id = NodeIndex(i);
        let signed_alert_hash = test_case
            .indexed_unchecked_signed(alert_hash, node_id)
            .await;
        test_case
            .scenario()
            .incoming_message(AlertMessage::RmcMessage(
                node_id,
                RmcMessage::SignedHash(signed_alert_hash),
            ));
    }
    test_case
        .scenario()
        .outgoing_notification(ForkingNotification::Units(Vec::new()));
    test_case.run(own_index).await;
}
