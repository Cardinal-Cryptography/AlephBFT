#!/bin/bash

if [ "$#" -ne 1 ]; then
    echo "Usage: run.sh n_nodes"
    exit 1
fi

set -e

cargo build --release

clear

n_nodes="$1"
ports="43000"
port=43000

for i in $(seq 0 $(expr $n_nodes - 2)); do
    port=$((port+1))
    ports+=",$port"
done

for i in $(seq 0 $(expr $n_nodes - 1)); do
    cargo run --release -- --id "$i" --ports "$ports" --n-ordered 50 --n-created 50 --n-starting 0 2>> "node${i}.log" &
done

echo "Running ordering example... (Ctrl+C to exit)"
trap 'kill $(jobs -p)' SIGINT SIGTERM
wait $(jobs -p)
