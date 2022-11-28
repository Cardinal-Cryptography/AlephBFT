#!/bin/bash

set -e

cargo clippy --all-targets --all-features -- -D warnings -D clippy::type_complexity
cargo fmt --all
cargo test --lib -- --skip medium
