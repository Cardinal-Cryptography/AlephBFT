#!/bin/bash

set -e

cargo build --release

clear

n_members="$1"

cargo run --release -- --my-id 0 --n-members $n_members --n-finalized 1000 --ip-addr 127.0.0.1:43000 --bootnodes-id 0 --bootnodes-ip-addr 127.0.0.1:43000 2>> node0.log
