mod chain;
mod data;
mod network;

use aleph_bft::run_session;
use aleph_bft_mock::{FinalizationHandler, Keychain, Spawner};
use chain::{run_blockchain, Block, BlockNum, ChainConfig};
use chrono::Local;
use clap::Parser;
use data::{Data, DataProvider, DataStore};
use futures::{channel::oneshot, StreamExt};
use log::{debug, error, info};
use network::{Address, NetworkData, NetworkManager};
use std::{
    collections::HashMap, fs, fs::File, io, io::Write, path::Path, str::FromStr, time,
    time::Duration,
};

const TXS_PER_BLOCK: usize = 50000;
const TX_SIZE: usize = 300;
const BLOCK_TIME: Duration = Duration::from_millis(1000);
const INITIAL_DELAY: Duration = Duration::from_millis(5000);

/// Blockchain example.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Our index
    #[clap(long)]
    my_id: usize,

    /// IP address of the node
    #[clap(default_value = "127.0.0.1:0", long)]
    ip_addr: String,

    /// Bootnodes indices
    #[clap(long, value_delimiter = ',')]
    bootnodes_id: Vec<u32>,

    /// Bootnodes addresses
    #[clap(long, value_delimiter = ',')]
    bootnodes_ip_addr: Vec<String>,

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
    let start_time = time::Instant::now();
    info!(target: "Blockchain-main", "Getting network up.");
    let bootnodes: HashMap<u32, Address> = args
        .bootnodes_id
        .into_iter()
        .zip(args.bootnodes_ip_addr)
        .map(|(id, addr)| (id, Address::from_str(&addr).unwrap()))
        .collect();
    let (
        mut manager,
        network,
        block_from_data_io_tx,
        block_from_network_rx,
        message_for_network,
        message_from_network,
    ) = NetworkManager::new(args.my_id, args.ip_addr, args.n_members, bootnodes)
        .await
        .expect("Libp2p network set-up should succeed.");
    let (data_provider, current_block) = DataProvider::new();
    let (finalization_handler, mut finalized_rx) = FinalizationHandler::new();
    let data_store = DataStore::new(current_block.clone(), message_for_network);

    let (close_network, exit) = oneshot::channel();
    let network_handle = tokio::spawn(async move { manager.run(exit).await });

    let data_size: usize = TXS_PER_BLOCK * TX_SIZE;
    let chain_config = ChainConfig::new(
        args.my_id.into(),
        args.n_members,
        data_size,
        BLOCK_TIME,
        INITIAL_DELAY,
    );
    let (close_chain, exit) = oneshot::channel();
    let chain_handle = tokio::spawn(async move {
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

    fn rotate_saved_unit_files(node_id: usize) -> Result<(File, File), io::Error> {
        let stash_path = Path::new("./backup");
        let extension = ".units";
        let session_path = stash_path.join(format!("{}", node_id));
        fs::create_dir_all(&session_path)?;
        let index = fs::read_dir(&session_path)
            .unwrap()
            .filter_map(|r| r.ok())
            .filter_map(|x| x.file_name().into_string().ok())
            .filter_map(|s| u64::from_str(s.strip_suffix(extension)?).ok())
            .max();
        let load_path = match index {
            Some(index) => session_path.join(format!("{}{}", index, extension)),
            None => "/dev/null".into(),
        };
        let load_file = File::open(load_path)?;
        let save_file = File::create(session_path.join(format!(
            "{}{}",
            index.map_or(0, |i| i + 1),
            extension
        )))?;
        Ok((save_file, load_file))
    }

    let (saver, loader) =
        rotate_saved_unit_files(args.my_id).expect("Error setting up unit saving");
    let (close_member, exit) = oneshot::channel();
    let member_handle = tokio::spawn(async move {
        let keychain = Keychain::new(args.n_members.into(), args.my_id.into());
        let config = aleph_bft::default_config(args.n_members.into(), args.my_id.into(), 0);
        let local_io = aleph_bft::LocalIO::new(data_provider, finalization_handler, saver, loader);
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
        if max_block_finalized >= args.n_finalized as u32 {
            break;
        }
    }
    if max_block_finalized < args.n_finalized as u32 {
        error!(target: "Blockchain-main", "Finalization stream finished too soon. Highest finalized = {:?}, expected {:?}", max_block_finalized, args.n_finalized);
        panic!("Finalization stream finished too soon.");
    }

    let stop_time = time::Instant::now();
    let tot_millis = (stop_time - start_time - INITIAL_DELAY).as_millis();
    let tps = (args.n_finalized as f64) * (TXS_PER_BLOCK as f64) / (0.001 * (tot_millis as f64));
    info!(target: "Blockchain-main", "Achieved {:?} tps.", tps);
    close_member.send(()).expect("should send");
    member_handle.await.unwrap();
    close_chain.send(()).expect("should send");
    chain_handle.await.unwrap();
    close_network.send(()).expect("should send");
    network_handle.await.unwrap();
}