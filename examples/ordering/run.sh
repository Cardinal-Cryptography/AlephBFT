#!/bin/bash

set -e

cargo build --release

clear

for i in {0..4}; do
    cargo run --release -- --id "$i" --ports 43000,43001,43002,43003,43004 --n-items 50 2> "node$i.log" &
done
