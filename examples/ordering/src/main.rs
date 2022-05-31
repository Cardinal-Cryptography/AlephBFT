use aleph_bft::run_session;
use aleph_bft_mock::{DataProvider, FinalizationHandler, Keychain, Spawner};
use chrono::Local;
use clap::Parser;
use futures::{channel::oneshot, StreamExt};
use log::{debug, error, info};
use std::{fs, fs::File, io, io::Write, path::Path};

mod network;
use network::Network;

/// Example node producing linear order.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Index of the node
    #[clap(long)]
    id: usize,

    /// Ports
    #[clap(long, value_delimiter = ',')]
    ports: Vec<usize>,

    /// Number of items to be ordered
    #[clap(long)]
    n_items: usize,
}

fn create_backup(node_id: usize) -> Result<(File, io::Cursor<Vec<u8>>), io::Error> {
    let stash_path = Path::new("./aleph-bft-examples-ordering-backup");
    fs::create_dir_all(&stash_path)?;
    let file_path = stash_path.join(format!("{}.units", node_id));
    let _ = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
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
    let data_provider = DataProvider::new();
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

    for i in 0..args.n_items {
        match finalized_rx.next().await {
            Some(_) => debug!("Got new batch. Finalized: {:?}", i + 1),
            None => {
                error!(
                    "Finalization stream finished too soon. Got {:?} batches, wanted {:?} batches",
                    i + 1,
                    args.n_items
                );
                panic!("Finalization stream finished too soon.");
            }
        }
    }
    info!("Finalized required number of items.");
    info!("Waiting 10 seconds for other nodes...");
    tokio::time::sleep(core::time::Duration::from_secs(10)).await;
    info!("Shutdown.");
    close_member.send(()).expect("should send");
    member_handle.await.unwrap();
}
