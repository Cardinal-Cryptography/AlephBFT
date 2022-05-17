use aleph_bft_examples_blockchain::network::{Address, ClientNetwork};
use clap::Parser;
use futures::channel::mpsc::unbounded;
use std::{collections::HashMap, str::FromStr};

/// Blockchain example.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(default_value = "127.0.0.1:0", long)]
    ip_addr: String,

    #[clap(default_value = "5", long)]
    n_nodes: u32,

    #[clap(long, value_delimiter = ',')]
    bootnodes_id: Vec<u32>,

    #[clap(long, value_delimiter = ',')]
    bootnodes_ip_addr: Vec<String>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let bootnodes: HashMap<u32, Address> = args
        .bootnodes_id
        .into_iter()
        .zip(args.bootnodes_ip_addr)
        .map(|(id, addr)| (id, Address::from_str(&addr).unwrap()))
        .collect();
    let (tx_commands, rx_commands) = unbounded();
    let mut network =
        ClientNetwork::new(args.ip_addr, args.n_nodes as usize, bootnodes, rx_commands).await;
    tokio::spawn(async move { network.run().await });
    loop {
        tx_commands
            .unbounded_send("2 PRINT 0 1 0".to_string())
            .unwrap();
        tx_commands
            .unbounded_send("PRINT 0 1 1".to_string())
            .unwrap();
        tx_commands
            .unbounded_send("2 PRINT 0 1 2".to_string())
            .unwrap();
        tx_commands
            .unbounded_send("TRANSFER 0 2 1 3".to_string())
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
}
