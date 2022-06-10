#!/bin/bash

if [ "$#" -ne 3 ]; then
    echo "Usage: run.sh n_working_nodes n_nodes_to_crash restart_delay"
    exit 1
fi

set -e

cargo build --release

clear

n_working_nodes="$1"
n_nodes_to_crash="$2"
restart_delay="$3"
ports="43000"
port=43000

n_ordered=100
n_ordered_before_crash=35
n_ordered_after_crash=65

for i in $(seq 0 $(expr $n_working_nodes + $n_nodes_to_crash - 2)); do
    port=$((port+1))
    ports+=",$port"
done

run_crash_node () {
    id="$1"
    ports="$2"
    echo "Starting node $id (to be crashed after providing ${n_ordered_before_crash}/${n_ordered} items)..."
    cargo run --release -- --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered_before_crash" --n-starting 0 --crash 2>> "node${id}.log"
    echo "Node $id crashed with exit code $?.  Respawning in $restart_delay seconds..."
    sleep "$restart_delay"
    echo "Starting node $id again..."
    cargo run --release -- --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered_after_crash" --n-starting "$n_ordered_before_crash" 2>> "node${id}.log"
}

for i in $(seq 0 $(expr $n_working_nodes - 1)); do
    echo "Starting node $i..."
    cargo run --release -- --id "$i" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered" --n-starting 0 2>> "node$i.log" &
done

for i in $(seq $(expr $n_working_nodes) $(expr $n_working_nodes + $n_nodes_to_crash - 1)); do
    ( run_crash_node "$i" "$ports" ) &
done

trap 'kill $(jobs -p); wait' SIGINT SIGTERM
wait
