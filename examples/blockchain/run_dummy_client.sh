#!/bin/bash

cargo run --bin dummy_client -- --ip-addr 127.0.0.1:0 --n-nodes 5 --bootnodes-id 0 --bootnodes-ip-addr 127.0.0.1:43000 --timeout 200
