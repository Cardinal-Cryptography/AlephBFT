use crate::{
    testing::{init_log, spawn_honest_member, Data},
    NodeCount, NodeIndex, SpawnHandle, TaskHandle,
};
use aleph_bft_mock::{Router, Spawner};
use futures::channel::{mpsc, oneshot};
use parking_lot::Mutex;
use std::{collections::HashMap, sync::Arc};

struct NodeData {
    _batch_rx: mpsc::UnboundedReceiver<Data>,
    _exit_tx: oneshot::Sender<()>,
    _handle: TaskHandle,
    _saved_units: Arc<Mutex<Vec<u8>>>,
}

async fn crashed_nodes_recover(
    n_members: NodeCount,
    _n_kill: NodeCount,
    _n_batches: usize,
    _network_reliability: f64,
) {
    init_log();
    let spawner = Spawner::new();
    let (net_hub, networks) = Router::new(n_members, 1.0);
    spawner.spawn("network-hub", net_hub);

    let mut node_data = HashMap::new();
    let mut batches = HashMap::<NodeIndex, Vec<Data>>::new();

    for network in networks {
        let ix = network.0.index();
        if n_members.into_range().contains(&ix) {
            let (batch_rx, saved_units, exit_tx, handle) =
                spawn_honest_member(spawner.clone(), ix, n_members, vec![], network.0);
            batches.insert(ix, vec![]);
            node_data.insert(
                ix,
                NodeData {
                    _batch_rx: batch_rx,
                    _exit_tx: exit_tx,
                    _handle: handle,
                    _saved_units: saved_units,
                },
            );
        }
    }

    // for (i, data) in node_data.iter_mut() {
    //     let node_batches = batches.get_mut(i).unwrap();
    //     for _ in 0..n_batches {
    //         let batch = data.batch_rx.next().await.unwrap();
    //         node_batches.push(batch);
    //     }
    // }

    // let expected_batches = batches.get(&NodeIndex(0)).unwrap().clone();

    // println!("First batch");

    // for i in 0..n_kill.0 {
    //     let data = node_data.get_mut(&NodeIndex(i)).unwrap();
    //     for tx in data.exit_txs.drain(..) {
    //         let _ = tx.send(());
    //     }
    //     for handle in data.handles.drain(..) {
    //         let _ = handle.await;
    //     }
    // }

    // println!("Killed");

    // for network in networks {
    //     let ix = network.index();
    //     if n_kill.into_range().contains(&ix) {
    //         let units = node_data.remove(&ix).unwrap().units;
    //         let (batch_rx, exit_txs, handles) =
    //             spawn_honest_member(spawner.clone(), ix, n_members, units.clone(), network);
    //         node_data.insert(
    //             ix,
    //             NodeData {
    //                 batch_rx,
    //                 exit_txs,
    //                 handles,
    //                 units,
    //             },
    //         );
    //     }
    // }

    // println!("Spawned");

    // for (ix, data) in node_data.iter_mut() {
    //     if n_kill.into_range().contains(ix) {
    //         let node_batches = batches.get_mut(ix).unwrap();
    //         node_batches.clear();
    //         for _ in 0..n_batches {
    //             let batch = data.batch_rx.next().await.unwrap();
    //             node_batches.push(batch);
    //         }
    //     }
    // }

    // for (ix, batch) in batches.iter() {
    //     assert_eq!((ix, batch), (ix, &expected_batches));
    // }

    // println!("Second Batch");

    // for data in node_data.values_mut() {
    //     for tx in data.exit_txs.drain(..) {
    //         let _ = tx.send(());
    //     }
    //     for handle in data.handles.drain(..) {
    //         let _ = handle.await;
    //     }
    // }
}

#[tokio::test]
async fn node_crash_recovery() {
    crashed_nodes_recover(4.into(), 1.into(), 5, 1.0).await;
}
