extern crate aleph_bft_examples_blockchain;

use aleph_bft::{run_session, NodeIndex};
use aleph_bft_examples_blockchain::{
    chain::{gen_chain_config, run_blockchain, DataProvider, DataStore, FinalizationHandler},
    crypto::KeyBox,
    network::{Network, Spawner},
};
use aleph_bft_mock::{Loader, Saver};
use chrono::Local;
use clap::Parser;
use futures::{channel::oneshot, StreamExt};
use log::{debug, info};
use parking_lot::Mutex;
use std::{io::Write, sync::Arc, time};

const TXS_PER_BLOCK: usize = 50000;
const TX_SIZE: usize = 300;
const BLOCK_TIME_MS: u64 = 1000;
const INITIAL_DELAY_MS: u64 = 5000;

/// Blockchain example.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Our index
    #[clap(long)]
    my_id: usize,

    /// Size of the committee
    #[clap(long)]
    n_members: usize,

    /// Number of data to be finalized
    #[clap(long)]
    n_finalized: usize,
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

    let args = Args::parse();
    let my_node_ix = NodeIndex(args.my_id);
    let start_time = time::Instant::now();
    info!(target: "Blockchain-main", "Getting network up.");
    let (
        network,
        mut manager,
        block_from_data_io_tx,
        block_from_network_rx,
        message_for_network,
        message_from_network,
    ) = Network::new(my_node_ix)
        .await
        .expect("Libp2p network set-up should succeed.");
    let (data_provider, current_block) = DataProvider::new();
    let (finalization_handler, mut finalized_rx) = FinalizationHandler::new();
    let data_store = DataStore::new(current_block.clone(), message_for_network);

    let (close_network, exit) = oneshot::channel();
    tokio::spawn(async move { manager.run(exit).await });

    let data_size: usize = TXS_PER_BLOCK * TX_SIZE;
    let chain_config = gen_chain_config(
        my_node_ix,
        args.n_members,
        data_size,
        BLOCK_TIME_MS,
        INITIAL_DELAY_MS,
    );
    let (close_chain, exit) = oneshot::channel();
    tokio::spawn(async move {
        run_blockchain(
            chain_config,
            data_store,
            current_block,
            block_from_network_rx,
            block_from_data_io_tx,
            message_from_network,
            exit,
        )
        .await
    });

    let (close_member, exit) = oneshot::channel();
    tokio::spawn(async move {
        let keychain = KeyBox {
            count: args.n_members,
            index: args.my_id.into(),
        };
        let config = aleph_bft::default_config(args.n_members.into(), args.my_id.into(), 0);
        let units = Arc::new(Mutex::new(vec![]));
        let unit_loader = Loader::new((*units.lock()).clone());
        let unit_saver = Saver::new(units);
        let local_io =
            aleph_bft::LocalIO::new(data_provider, finalization_handler, unit_saver, unit_loader);
        run_session(config, local_io, network, keychain, Spawner {}, exit).await
    });

    let mut max_block_finalized = 0;
    while let Some(block_num) = finalized_rx.next().await {
        if max_block_finalized < block_num {
            max_block_finalized = block_num;
        }
        debug!(target: "Blockchain-main",
            "Got new batch. Highest finalized = {:?}",
            max_block_finalized
        );
        if max_block_finalized >= args.n_finalized as u64 {
            break;
        }
    }
    let stop_time = time::Instant::now();
    let tot_millis = (stop_time - start_time).as_millis() - INITIAL_DELAY_MS as u128;
    let tps = (args.n_finalized as f64) * (TXS_PER_BLOCK as f64) / (0.001 * (tot_millis as f64));
    info!(target: "Blockchain-main", "Achieved {:?} tps.", tps);
    close_member.send(()).expect("should send");
    close_chain.send(()).expect("should send");
    close_network.send(()).expect("should send");
}
