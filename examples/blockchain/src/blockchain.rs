use codec::{Decode, Encode};
use futures::{channel::mpsc::UnboundedReceiver, StreamExt};
use log::info;

#[derive(PartialEq, Eq, Clone, Debug, Decode, Encode, std::hash::Hash)]
pub enum Transaction {
    Print(u32, u32),
    Burn(u32, u32),
    Transfer(u32, u32, u32),
}

#[derive(Clone, Debug)]
pub enum ProcessedTransaction {
    Accepted(Transaction),
    Rejected(Transaction),
}

#[derive(Clone, Debug)]
pub struct State(Vec<u32>);

impl State {
    fn new(n_clients: u32) -> Self {
        Self(vec![0; n_clients as usize])
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    transactions: Vec<ProcessedTransaction>,
    state: State,
}

#[derive(Debug)]
pub struct Blockchain {
    state: State,
    transactions_buffer: Vec<Transaction>,
    blocks: Vec<Block>,
    n_blocks: u32,
    n_tr_per_block: u32,
    rx_transactions: UnboundedReceiver<Transaction>,
}

impl Blockchain {
    pub fn new(
        n_clients: u32,
        n_blocks: u32,
        n_tr_per_block: u32,
        rx_transactions: UnboundedReceiver<Transaction>,
    ) -> Self {
        assert!(n_clients > 0 && n_blocks > 0 && n_tr_per_block > 0);
        Self {
            state: State::new(n_clients),
            transactions_buffer: vec![],
            blocks: vec![],
            n_blocks,
            n_tr_per_block,
            rx_transactions,
        }
    }

    fn add_transaction(&mut self, transaction: Transaction) -> bool {
        self.transactions_buffer.push(transaction);
        if self.transactions_buffer.len() == self.n_tr_per_block as usize {
            self.create_block();
        }
        self.blocks.len() == self.n_blocks as usize
    }

    fn create_block(&mut self) {
        let mut transactions: Vec<ProcessedTransaction> = vec![];
        for tr in self.transactions_buffer.clone().iter() {
            transactions.push(self.process_transaction(tr));
        }
        let block = Block {
            transactions,
            state: self.state.clone(),
        };
        info!("Creating block {}", self.blocks.len());
        info!("Block contents:");
        info!("  Transactions: {:?}", block.transactions);
        info!("  Current state: {:?}", block.state);
        self.blocks.push(block);
        self.transactions_buffer = vec![];
    }

    fn process_transaction(&mut self, transaction: &Transaction) -> ProcessedTransaction {
        match transaction {
            Transaction::Print(id, n) => {
                self.state.0[*id as usize] += n;
                ProcessedTransaction::Accepted(transaction.clone())
            }
            Transaction::Burn(id, n) => {
                if self.state.0[*id as usize] >= *n {
                    self.state.0[*id as usize] -= n;
                    ProcessedTransaction::Accepted(transaction.clone())
                } else {
                    ProcessedTransaction::Rejected(transaction.clone())
                }
            }
            Transaction::Transfer(id_from, id_to, n) => {
                if self.state.0[*id_from as usize] >= *n {
                    self.state.0[*id_from as usize] -= n;
                    self.state.0[*id_to as usize] += n;
                    ProcessedTransaction::Accepted(transaction.clone())
                } else {
                    ProcessedTransaction::Rejected(transaction.clone())
                }
            }
        }
    }

    pub async fn run(&mut self) {
        while let Some(transaction) = self.rx_transactions.next().await {
            if self.add_transaction(transaction) {
                break;
            }
        }
    }
}
