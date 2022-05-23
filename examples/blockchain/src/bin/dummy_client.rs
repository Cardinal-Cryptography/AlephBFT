use aleph_bft_examples_blockchain::network::{Address, ClientNetwork};
use clap::Parser;
use futures::channel::mpsc::unbounded;
use std::{collections::HashMap, str::FromStr};

/// Example blockchain - dummy client.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// IP address of the client
    #[clap(default_value = "127.0.0.1:0", long)]
    ip_addr: String,

    /// Number of nodes
    #[clap(default_value = "5", long)]
    n_nodes: u32,

    /// Bootnodes indices
    #[clap(long, value_delimiter = ',')]
    bootnodes_id: Vec<u32>,

    /// Bootnodes addresses
    #[clap(long, value_delimiter = ',')]
    bootnodes_ip_addr: Vec<String>,

    /// Time interval between messages
    #[clap(default_value = "200", long)]
    timeout: u64,
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
        for n in 0..args.n_nodes {
            tx_commands
                .unbounded_send(format!("print 0 0 {}", n))
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(args.timeout)).await;
        }
    }
}
