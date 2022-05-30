#!/bin/bash

set -e

cargo build --release

clear

for i in {0..4}; do
    cargo run --release -- --id "$i" --ports 43000,43001,43002,43003,43004 --n-items 50 2> "node$i.log" &
done

echo "Running ordering example... (Ctrl+C to exit)"
trap 'kill $(jobs -p)' SIGINT SIGTERM
wait $(jobs -p)
