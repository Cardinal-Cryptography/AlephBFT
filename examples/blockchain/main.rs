use futures::{channel::oneshot, StreamExt};
use log::info;

use aleph_bft::NodeIndex;
use chain::{gen_chain_config, run_blockchain, DataIO};
use chrono::Local;
use crypto::KeyBox;
use network::{Network, Spawner};
use std::{io::Write, time};

mod chain;
mod crypto;
mod network;

const DATA_SIZE: usize = 1000000;
const BLOCK_TIME_MS: u64 = 500;
const INITIAL_DELAY_MS: u64 = 4000;

const USAGE_MSG: &str = "Missing arg. Usage
    cargo run --example blockchain my_id n_members n_finalized

    my_id -- our index
    n_members -- size of the committee
    n_finalized -- number of data to be finalized";

fn parse_arg(n: usize) -> usize {
    if let Some(int) = std::env::args().nth(n) {
        match int.parse::<usize>() {
            Ok(int) => int,
            Err(err) => {
                panic!("Failed to parse arg {:?}", err);
            }
        }
    } else {
        panic!("{}", USAGE_MSG);
    }
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
        .filter(None, log::LevelFilter::Debug)
        .init();

    let my_id = parse_arg(1);
    let n_members = parse_arg(2);
    let n_finalized = parse_arg(3);
    let my_node_ix = NodeIndex(my_id);
    let start_time = time::Instant::now();
    info!(target: "Blockchain-main", "Getting network up.");
    let (network, mut manager, block_from_data_io_tx, block_from_network_rx) =
        Network::new(my_node_ix).await.unwrap();
    let (data_io, mut batch_rx) = DataIO::new();

    let (close_network, exit) = oneshot::channel();
    tokio::spawn(async move { manager.run(exit).await });

    let chain_config = gen_chain_config(
        my_node_ix,
        n_members,
        DATA_SIZE,
        BLOCK_TIME_MS,
        INITIAL_DELAY_MS,
    );
    let (close_chain, exit) = oneshot::channel();
    let data_io_clone = data_io.clone();
    tokio::spawn(async move {
        run_blockchain(
            chain_config,
            data_io_clone,
            block_from_network_rx,
            block_from_data_io_tx,
            exit,
        )
        .await
    });

    let (close_member, exit) = oneshot::channel();
    tokio::spawn(async move {
        let keybox = KeyBox {
            count: n_members,
            index: my_id.into(),
        };
        let config = aleph_bft::default_config(n_members.into(), my_id.into(), 0);
        let member = aleph_bft::Member::new(data_io, &keybox, config, Spawner {});

        member.run_session(network, exit).await
    });

    let mut max_block_finalized = 0;
    while let Some(batch) = batch_rx.next().await {
        for block_num in batch {
            if max_block_finalized < block_num {
                max_block_finalized = block_num;
            }
        }
        info!(target: "Blockchain-main",
            "Got new batch. Highest finalized = {:?}",
            max_block_finalized
        );
        if max_block_finalized >= n_finalized as u64 {
            break;
        }
    }
    let stop_time = time::Instant::now();
    let tot_millis = (stop_time - start_time).as_millis() - INITIAL_DELAY_MS as u128;
    let transaction_size = 300;
    let tps = (n_finalized as f64) * (DATA_SIZE as f64)
        / (transaction_size as f64)
        / (0.001 * (tot_millis as f64));
    info!(target: "Blockchain-main", "Achieved {:?} tps.", tps);
    close_member.send(()).expect("should send");
    close_chain.send(()).expect("should send");
    close_network.send(()).expect("should send");
}
