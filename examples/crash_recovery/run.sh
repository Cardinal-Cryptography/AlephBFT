#!/bin/bash

usage="Example usage: run.sh --n-working-nodes 5 --n-nodes-to-crash 5 --n-crashes 3 --n-ordered-per-crash 25 --restart-delay 5"

while [[ "$#" -gt 0 ]]; do
    case $1 in
        --n-working-nodes) n_working_nodes="$2";;
        --n-nodes-to-crash) n_nodes_to_crash="$2";;
        --restart-delay) restart_delay="$2";;
        --n-crashes) n_crashes="$2";;
        --n-ordered-per-crash) n_ordered_per_crash="$2";;
        *) echo "$usage"; exit 1 ;;
    esac
    shift
    shift
done

if [[ -z $n_working_nodes || -z $n_nodes_to_crash || -z $restart_delay || -z $n_crashes ]]; then
    echo "$usage"
    exit 1
fi

n_ordered=$(( (n_crashes+1)*n_ordered_per_crash ))
port=4343
ports="$port"
for i in $(seq 0 $(expr $n_working_nodes + $n_nodes_to_crash - 2)); do
    port=$((port+1))
    ports+=",$port"
done

set -e

cargo build --release

clear

run_crash_node () {
    id="$1"
    n_starting=0
    for (( i = 1; i <= n_crashes; i++ )); do
        echo "Starting node $id at $n_starting items ($i/$((n_crashes+1)))..."
        cargo run --release -- --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered_per_crash" --n-starting "$n_starting" --crash 2>> "node${id}.log"
        echo "Node $id crashed.  Respawning in $restart_delay seconds..."
        sleep "$restart_delay"
        n_starting=$(( n_starting + n_ordered_per_crash ))
    done
    echo "Starting node $id at $n_starting items ($((n_crashes+1))/$((n_crashes+1)))..."
    cargo run --release -- --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered_per_crash" --n-starting "$n_starting" 2>> "node${id}.log"
}

for i in $(seq 0 $(expr $n_working_nodes - 1)); do
    echo "Starting node $i..."
    cargo run --release -- --id "$i" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered" --n-starting 0 2>> "node$i.log" &
done

for i in $(seq $(expr $n_working_nodes) $(expr $n_working_nodes + $n_nodes_to_crash - 1)); do
    run_crash_node "$i" &
done

trap 'kill $(jobs -p); wait' SIGINT SIGTERM
wait
