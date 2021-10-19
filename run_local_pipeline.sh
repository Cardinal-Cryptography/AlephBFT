#!/bin/bash

set -e

cargo +nightly clippy --all-targets --all-features -- -D warnings -A clippy::type_complexity
cargo +nightly fmt --all
cargo test --lib -- --skip medium

pushd fuzz

cargo +nightly clippy --all-targets --all-features -- -D warnings -A clippy::type_complexity
cargo +nightly fmt --all
cargo test --lib -- --skip medium

popd
pushd mock

cargo +nightly clippy --all-targets --all-features -- -D warnings -A clippy::type_complexity
cargo +nightly fmt --all
cargo test --lib -- --skip medium

popd
