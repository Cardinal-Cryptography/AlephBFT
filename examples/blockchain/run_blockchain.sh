#!/bin/bash

set -e

n_members="$1"

for i in $(seq 0 $(expr $n_members - 1)); do
    cargo run --release --example blockchain $i $n_members 43 2> node$i.log &
done
