#!/bin/bash

cargo run --bin node -- --id 0 --ip-addr 127.0.0.1:43000 2> node0.log &
for i in {1..4}; do
    cargo run --bin node -- --id "$i" --ip-addr 127.0.0.1:0 --bootnodes-id 0 --bootnodes-ip-addr 127.0.0.1:43000 2> "node$i.log" &
done
