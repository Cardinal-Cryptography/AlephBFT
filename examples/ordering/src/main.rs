mod dataio;
mod network;

use aleph_bft::{run_session, NodeIndex};
use aleph_bft_mock::{Keychain, Spawner};
use chrono::Local;
use clap::Parser;
use dataio::{Data, DataProvider, FinalizationHandler};
use futures::{channel::oneshot, StreamExt};
use log::{debug, error, info};
use network::Network;
use std::{collections::HashMap, fs, fs::File, io, io::Write, path::Path};

/// Example node producing linear order.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Index of the node
    #[clap(long, value_parser)]
    id: usize,

    /// Ports
    #[clap(long, value_parser, value_delimiter = ',')]
    ports: Vec<usize>,

    /// Number of items to be ordered
    #[clap(long, value_parser)]
    n_ordered: u32,

    /// Number of items to be created
    #[clap(long, value_parser)]
    n_created: u32,

    /// Number of the first created item
    #[clap(long, value_parser)]
    n_starting: u32,

    /// Should the node crash after finalizing its items
    #[clap(long, value_parser)]
    crash: bool,
}

fn create_backup(node_id: usize) -> Result<(File, io::Cursor<Vec<u8>>), io::Error> {
    let stash_path = Path::new("./aleph-bft-examples-ordering-backup");
    fs::create_dir_all(&stash_path)?;
    let file_path = stash_path.join(format!("{}.units", node_id));
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path.clone());
    let loader = io::Cursor::new(fs::read(file_path.clone())?);
    let saver = fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(file_path)?;
    Ok((saver, loader))
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

    info!("Getting network up.");
    let network = Network::new(args.id, &args.ports).await.unwrap();
    let n_members = args.ports.len();
    let data_provider = DataProvider::new(args.id.into(), args.n_starting, args.n_created);
    let (finalization_handler, mut finalized_rx) = FinalizationHandler::new();
    let (backup_saver, backup_loader) =
        create_backup(args.id).expect("Error setting up unit saving");
    let local_io = aleph_bft::LocalIO::new(
        data_provider,
        finalization_handler,
        backup_saver,
        backup_loader,
    );

    let (close_member, exit) = oneshot::channel();
    let member_handle = tokio::spawn(async move {
        let keychain = Keychain::new(n_members.into(), args.id.into());
        let config = aleph_bft::default_config(n_members.into(), args.id.into(), 0);
        run_session(config, local_io, network, keychain, Spawner {}, exit).await
    });

    let mut count_finalized: HashMap<NodeIndex, u32> =
        (0..args.ports.len()).map(|c| (c.into(), 0)).collect();
    let show_finalized = |cf: &HashMap<NodeIndex, u32>| -> Vec<u32> {
        let mut v = cf
            .iter()
            .map(|(id, n)| (id.0, n))
            .collect::<Vec<(usize, &u32)>>();
        v.sort();
        v.iter().map(|(_, n)| **n).collect()
    };
    loop {
        match finalized_rx.next().await {
            Some((id, Some(number))) => {
                *count_finalized.get_mut(&id).unwrap() += 1;
                debug!(
                    "Finalized new item: node {:?}, number {:?}; total: {:?}",
                    id.0,
                    number,
                    show_finalized(&count_finalized)
                );
            }
            Some((_, None)) => (),
            None => {
                error!(
                    "Finalization stream finished too soon. Got {:?} items, wanted {:?} items",
                    show_finalized(&count_finalized),
                    args.n_ordered
                );
                panic!("Finalization stream finished too soon.");
            }
        }
        if args.crash
            && count_finalized.get(&args.id.into()).unwrap()
                == &(args.n_created + args.n_starting - 1)
        {
            info!(
                "Forced crash - items finalized so far: {:?}.",
                show_finalized(&count_finalized)
            );
            break;
        }
        if count_finalized.values().all(|c| c >= &(args.n_ordered)) {
            info!("Finalized required number of items.");
            info!("Waiting 10 seconds for other nodes...");
            tokio::time::sleep(core::time::Duration::from_secs(10)).await;
            info!("Shutdown.");
            break;
        }
    }

    close_member.send(()).expect("should send");
    member_handle.await.unwrap();
}
