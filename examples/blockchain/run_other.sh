#!/bin/bash

set -e

n_members="$1"

for i in $(seq 1 $(expr $n_members - 1)); do
    cargo run --release -- --my-id $i --n-members $n_members --n-finalized 1000  --bootnodes-id 0 --bootnodes-ip-addr 127.0.0.1:43000 2> node$i.log &
done
