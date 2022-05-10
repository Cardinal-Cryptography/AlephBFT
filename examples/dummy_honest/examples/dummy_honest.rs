use aleph_bft::run_session;
use aleph_bft_examples_network::Network;
use aleph_bft_mock::{DataProvider, FinalizationHandler, Keychain, Loader, Saver, Spawner};
use chrono::Local;
use clap::Parser;
use futures::{channel::oneshot, StreamExt};
use log::{debug, error, info};
use parking_lot::Mutex;
use std::{io::Write, sync::Arc};

/// Dummy honest node example.
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Our index
    #[clap(long)]
    my_id: usize,

    /// Ports
    #[clap(long, value_delimiter = ',')]
    ports: Vec<usize>,

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

    info!(target: "dummy-honest", "Getting network up.");
    let network = Network::new(args.my_id, &args.ports).await.unwrap();
    let n_members = args.ports.len();
    let data_provider = DataProvider::new();
    let (finalization_handler, mut finalized_rx) = FinalizationHandler::new();
    let backup_loader = Loader::new(vec![]);
    let backup_saver = Saver::new(Arc::new(Mutex::new(vec![])));
    let local_io = aleph_bft::LocalIO::new(
        data_provider,
        finalization_handler,
        backup_saver,
        backup_loader,
    );

    let (close_member, exit) = oneshot::channel();
    let member_handle = tokio::spawn(async move {
        let keychain = Keychain::new(n_members.into(), args.my_id.into());
        let config = aleph_bft::default_config(n_members.into(), args.my_id.into(), 0);
        run_session(config, local_io, network, keychain, Spawner {}, exit).await
    });

    for i in 0..args.n_finalized {
        match finalized_rx.next().await {
            Some(_) => debug!(target: "dummy-honest", "Got new batch. Finalized: {:?}", i+1),
            None => {
                error!(target: "dummy-honest", "Finalization stream finished too soon. Got {:?} batches, wanted {:?} batches", i+1, args.n_finalized);
                panic!("Finalization stream finished too soon.");
            }
        }
    }
    close_member.send(()).expect("should send");
    member_handle.await.unwrap();
}
