use aleph_bft_examples_blockchain::{
    blockchain::Blockchain,
    consensus::run as run_consensus,
    io::consensus_network_io,
    network::{Address, NodeNetwork},
};
use chrono::Local;
use clap::Parser;
use futures::channel::{mpsc::unbounded, oneshot};
use std::{collections::HashMap, io::Write, str::FromStr};

/// Blockchain example.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Our index
    #[clap(long)]
    id: u32,

    #[clap(default_value = "127.0.0.1:0", long)]
    ip_addr: String,

    #[clap(long, value_delimiter = ',')]
    bootnodes_id: Vec<u32>,

    #[clap(long, value_delimiter = ',')]
    bootnodes_ip_addr: Vec<String>,

    #[clap(default_value = "1000", long)]
    n_blocks: u32,

    #[clap(default_value = "5", long)]
    n_tr_per_block: u32,

    #[clap(default_value = "5", long)]
    n_nodes: u32,

    #[clap(default_value = "5", long)]
    n_clients: u32,
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .format(|buf, record| {
            writeln!(
                buf,
                "{} {}: {}",
                record.level(),
                Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.args()
            )
        })
        .filter(None, log::LevelFilter::Info)
        .init();

    let args = Args::parse();
    let bootnodes: HashMap<u32, Address> = args
        .bootnodes_id
        .into_iter()
        .zip(args.bootnodes_ip_addr)
        .map(|(id, addr)| (id, Address::from_str(&addr).unwrap()))
        .collect();

    loop {
        let (tx_crash, rx_crash) = oneshot::channel();
        let (tx_transactions, rx_transactions) = unbounded();
        let (network_io, consensus_io) = consensus_network_io();
        let mut blockchain = Blockchain::new(
            args.n_clients,
            args.n_blocks,
            args.n_tr_per_block,
            rx_transactions,
        );
        let mut network = NodeNetwork::new(
            args.id as usize,
            args.ip_addr.clone(),
            args.n_nodes as usize,
            bootnodes.clone(),
            consensus_io,
            tx_crash,
        )
        .await;

        tokio::spawn(async move {
            run_consensus(
                args.id as usize,
                args.n_nodes as usize,
                network_io,
                tx_transactions,
            )
            .await
        });
        tokio::spawn(async move { network.run().await });
        let mut blockchain_handle = tokio::spawn(async move { blockchain.run().await });

        tokio::select! {
            _ = &mut blockchain_handle => break,
            _ = rx_crash => continue,
        }
    }
}
