#!/bin/bash

usage() {
    echo "Usage: ./run.sh [-n N_NODES] [-m N_MALFUNCTIONING_NODES] [-c N_CRASHES] [-o N_ORDERED_PER_CRASH] [-d RESTART_DELAY]"
    exit 1
}

N_NODES=5
N_MALFUNCTIONING_NODES=5
N_CRASHES=3
N_ORDERED_PER_CRASH=25
RESTART_DELAY=1

while getopts :n:m:c:o:d: flag; do
    case "${flag}" in
        n) N_NODES=${OPTARG};;
        m) N_MALFUNCTIONING_NODES=${OPTARG};;
        c) N_CRASHES=${OPTARG};;
        o) N_ORDERED_PER_CRASH=${OPTARG};;
        d) RESTART_DELAY=${OPTARG};;
        *) usage;;
    esac
done

n_ordered=$(( (N_CRASHES+1)*N_ORDERED_PER_CRASH ))
port=10000
ports="$port"
for i in $(seq 0 $(expr $N_NODES + $N_MALFUNCTIONING_NODES - 2)); do
    port=$((port+1))
    ports+=",$port"
done

set -e

cargo build --release
binary="../../target/release/aleph-bft-examples-ordering"

clear

run_crash_node () {
    id="$1"
    n_starting=0
    for (( i = 1; i <= N_CRASHES; i++ )); do
        echo "Starting node $id at $n_starting items ($i/$((N_CRASHES+1)))..."
        "$binary" --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$N_ORDERED_PER_CRASH" --n-starting "$n_starting" --crash 2>> "node${id}.log"
        echo "Node $id crashed.  Respawning in $RESTART_DELAY seconds..."
        sleep "$RESTART_DELAY"
        n_starting=$(( n_starting + N_ORDERED_PER_CRASH ))
    done
    echo "Starting node $id at $n_starting items ($((N_CRASHES+1))/$((N_CRASHES+1)))..."
    "$binary" --id "$id" --ports "$ports" --n-ordered "$n_ordered" --n-created "$N_ORDERED_PER_CRASH" --n-starting "$n_starting" 2>> "node${id}.log"
}

for i in $(seq 0 $(expr $N_NODES + $N_MALFUNCTIONING_NODES - 1)); do
    rm "aleph-bft-examples-ordering-backup/${i}.units" 2> /dev/null
    rm "node${i}.log" 2> /dev/null
done

for i in $(seq 0 $(expr $N_NODES - 1)); do
    echo "Starting node $i..."
    "$binary" --id "$i" --ports "$ports" --n-ordered "$n_ordered" --n-created "$n_ordered" --n-starting 0 2>> "node$i.log" &
done

for i in $(seq $(expr $N_NODES) $(expr $N_NODES + $N_MALFUNCTIONING_NODES - 1)); do
    run_crash_node "$i" &
done

trap 'kill $(jobs -p); wait' SIGINT SIGTERM
wait
