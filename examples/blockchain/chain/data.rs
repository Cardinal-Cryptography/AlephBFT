use crate::{chain::BlockNum, network::NetworkData};
use aleph_bft::OrderedBatch;
use codec::Encode;
use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use log::debug;
use parking_lot::Mutex;
use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::Hasher,
    sync::Arc,
};

pub(crate) type Data = BlockNum;

pub(crate) struct DataStore {
    current_block: Arc<Mutex<BlockNum>>,
    available_blocks: HashSet<BlockNum>,
    message_requirements: HashMap<u64, HashSet<BlockNum>>,
    dependent_messages: HashMap<BlockNum, Vec<u64>>,
    pending_messages: HashMap<u64, NetworkData>,
    messages_for_member: UnboundedSender<NetworkData>,
}

impl DataStore {
    pub(crate) fn new(
        current_block: Arc<Mutex<BlockNum>>,
        messages_for_member: UnboundedSender<NetworkData>,
    ) -> Self {
        let available_blocks = (0..=*current_block.lock()).collect();
        DataStore {
            current_block,
            available_blocks,
            message_requirements: HashMap::new(),
            dependent_messages: HashMap::new(),
            pending_messages: HashMap::new(),
            messages_for_member,
        }
    }

    fn message_hash(message: &NetworkData) -> u64 {
        let mut hasher = DefaultHasher::new();
        hasher.write(&message.encode());
        hasher.finish()
    }

    fn add_pending_message(&mut self, message: NetworkData, requirements: Vec<BlockNum>) {
        let message_hash = Self::message_hash(&message);
        for block_num in requirements.iter() {
            self.dependent_messages
                .entry(*block_num)
                .or_insert_with(Vec::new)
                .push(message_hash);
        }
        self.message_requirements
            .insert(message_hash, requirements.iter().cloned().collect());
        self.pending_messages.insert(message_hash, message);
    }

    pub(crate) fn add_message(&mut self, message: NetworkData) {
        let requirements: Vec<_> = message
            .included_data()
            .into_iter()
            .filter(|b| !self.available_blocks.contains(b))
            .collect();
        if requirements.is_empty() {
            self.messages_for_member
                .unbounded_send(message)
                .expect("member accept messages");
        } else {
            self.add_pending_message(message, requirements);
        }
    }

    fn push_messages(&mut self, num: BlockNum) {
        for message_hash in self
            .dependent_messages
            .entry(num)
            .or_insert_with(Vec::new)
            .iter()
        {
            self.message_requirements
                .get_mut(message_hash)
                .expect("there are some requirements")
                .remove(&num);
            if self.message_requirements[message_hash].is_empty() {
                let message = self
                    .pending_messages
                    .remove(message_hash)
                    .expect("there is a pending message");
                self.messages_for_member
                    .unbounded_send(message)
                    .expect("member accept messages");
                self.message_requirements.remove(message_hash);
            }
        }
        self.dependent_messages.remove(&num);
    }

    pub(crate) fn add_block(&mut self, num: BlockNum) {
        debug!(target: "data-store", "Added block {:?}.", num);
        self.available_blocks.insert(num);
        self.push_messages(num);
        while self
            .available_blocks
            .contains(&(*self.current_block.lock() + 1))
        {
            *self.current_block.lock() += 1;
        }
    }
}

#[derive(Clone)]
pub(crate) struct DataIO {
    current_block: Arc<Mutex<BlockNum>>,
    finalized_for_world: UnboundedSender<OrderedBatch<Data>>,
}

impl aleph_bft::DataIO<Data> for DataIO {
    type Error = ();
    fn get_data(&self) -> Data {
        *self.current_block.lock()
    }

    fn send_ordered_batch(&mut self, data: OrderedBatch<Data>) -> Result<(), Self::Error> {
        self.finalized_for_world
            .unbounded_send(data)
            .map_err(|_| ())
    }
}

impl DataIO {
    pub(crate) fn new() -> (
        Self,
        UnboundedReceiver<OrderedBatch<Data>>,
        Arc<Mutex<BlockNum>>,
    ) {
        let (finalized_for_world, finalized_from_consensus) = mpsc::unbounded();
        let current_block = Arc::new(Mutex::new(0));
        (
            DataIO {
                current_block: current_block.clone(),
                finalized_for_world,
            },
            finalized_from_consensus,
            current_block,
        )
    }
}