[![Crate][crate-image]][crate-link]
[![Docs][docs-image]][docs-link]
[![Build Status][build-image]][build-link]
[![Apache 2.0 Licensed][license-image]][license-link]

<p align="center">
  <a href="https://alephzero.org" target="_blank">
  <img src="https://alephzero.org/wp-content/uploads/A0_logotype_bft_dark.jpg" />
  </a>
</p>

### Overview

AlephBFT is an asynchronous and Byzantine fault tolerant consensus protocol aimed
at ordering arbitrary messages (transactions). It has been designed to operate
continuously under conditions where there is no bound on message-delivery delay
and under the assumption that there is a significant probability of malicious
behavior, making it an excellent fit for blockchain-related applications.
For more information, check [the white paper][paper-link].

This repository contains a Rust implementation of AlephBFT that offers a convenient
API enabling seamless application to various problems. The prime application of
the repository is the consensus engine (sometimes called the "finality gadget")
of the [Aleph Zero blockchain][aleph-node-link].

The code is split into several Rust packages, each having its own directory -
see the `Cargo.toml` file, which defines the layout of the whole workspace.
The main package, `aleph-bft`, is located in the `consensus` directory.
Additionally, every other package has a short README describing its role
in the AlephBFT toolset.

### Documentation

Every package is documented on [docs.rs][docs-link]. Comprehensive documentation
is available as a [mdBook][reference-link].

The book can be built locally (assuming you have installed `rustup`):
```
cargo install mdbook
cd docs
mdbook serve --open
```

### Implementation status

Highlights:
- The protocol is asynchronous, so it's driven by consensus events as opposed
  to some clock ticks.
- The performance is still optimal in a partially synchronous environment.
- BFT - secure if less than one third of the committee is malicious.
- Secure against fork bombs.
- Lowered network overhead of sending DAG parent information.
- Thorough testing, including malicious scenarios, and high code coverage.

More details are available [in the book][reference-link-implementation-details].

### Using the crate

- Import AlephBFT in your crate
  ```toml
  [dependencies]
  aleph-bft = "^0.14"
  ```
- The main entry point is the `run_session` function, which returns a Future that runs the
  consensus algorithm.
  To call this function, you need to pass a configuration (defaults are available in the package),
  and implement certain traits, which will provide all the necessary functionalities, such as networking
  and message signing.
  A comprehensive guide is available [in the documentation][reference-link-api].

### Examples

Currently we provide two basic examples of running AlephBFT. The first one: `ordering` implements a committee member that
generates its own stream of data, and cooperates with other nodes to reach a common linear ordering.
Run the following command
```
cd examples/ordering
./run.sh
```
to launch five nodes listening on ports 43000-43004, and investigate the output written to `nodeX.log` files.
For further details, see
```
cargo run -- --help
```

The second example: `blockchain` implements a simple blockchain built upon our consensus protocol.
The blockchain state is represented by a vector of integers, each denoting the account balance
of a specific client.

Clients and nodes are numbered, separately, with consecutive integers. This toy blockchain is not cryptographically
secure, and the numbers also serve as mock signatures.

Run
```
cd examples/blockchain
./run_nodes.sh
```
to start five nodes in background, and investigate the output written to `nodeX.log` files.
Remember to stop the nodes before running this script again, e.g.
```
killall -p node
```

Nodes use a simple discovery protocol, so as long as you specify at least one bootnode with fixed IP address,
the other nodes will be able to broadcast their location.
When providing the port number, you may set it to zero, and then a free port will be given by the operating system,
but note that bootnodes must have properly specified ports.

A new block is generated upon reaching a certain number of finalized transactions - list of those transactions, their status,
and state of the blockchain will be logged upon block creation.
Since transactions are required to proceed, you may run
```
./run_dummy_client.sh
```
and the script will send a no-op transaction every 200 ms, thus creating a new block every second.

We also provide an interactive client
```
./run_client.sh
```
which can be run concurrently with the dummy client, and allows you to manually send one of
four types of messages: `print`, `burn`, `transfer`, and `crash`.
For example:
- `print 0 100 3` - add 100 to the account of client 0, send the request to node 3,
- `burn 2 50 broadcast` - subtract 50 from the accout of client 2, but send the request to every node, so it will be processed multiple times,
- `3 transfer 1 2 50 4` - transfer 50 from client 1 to client 2, send 3 such requests to node 4,
- `crash 1` - crash and restart node 1,
- `crash broadcast` - crash and restart all nodes.

A transaction can be rejected (but still included in a block) if there are insufficient funds on the client's account.
This fact will be denoted in the logs.
Also, since crashing and restarting the nodes happens inside the node binary, you'll be able to test complex scenarios of scheduled crashes
and have a single log file per node.

For further details, see
```
cargo run [node|client|dummy_client] -- --help
```

### Dependencies

The repository is mainly self-contained. It is implemented using Rust's async features and depends only on the
`futures` crate from the standard library. Moreover, it has some usual dependencies like
`log` and `rand` and one bigger for encoding, namely `parity-scale-codec`. In future work, we plan to get
rid of this dependency.

### Toolchain

This release was built and tested against the `nightly-2021-10-24` Rust toolchain.
If you want to use another version, edit the `rust-toolchain` file, or use an [override](https://rust-lang.github.io/rustup/overrides.html) with higher priority.

### Tests

There are many unit tests and several integration tests that may be run by standard command
`cargo test --lib` or `cargo test --lib --skip medium` if you want to run just small tests.
Alternatively, you may run the `run_local_pipeline.sh` script.

### Fuzzing

We provide fuzzing tests that try to crash the whole application by creating arbitrary data for the network layer
and feeding it into the `member` implementation. To run those tests you need to install `afl` and `cargo-fuzz`.
`cargo-fuzz` requires you to use a nightly Rust toolchain. `afl` differs from `cargo-fuzz` in that it requires
so called corpus data to operate, i.e. some non-empty data set that do not crash the application.
Both tools are using LLVM's instrumentation capabilities in order to guide the fuzzing process basing on code-coverage statistics.

```sh
cargo install cargo-fuzz
cargo install afl
```

#### cargo-fuzz/libfuzzer

```sh
cargo fuzz run --features="libfuzz" fuzz_target
```

#### afl

You will need to generate some `seed` data first in order to run it.

```sh
# create some random input containing network data from a locally executed test
mkdir afl_in
cargo build --bin gen_fuzz
./target/debug/gen_fuzz >./afl_in/seed
```

You might need to reconfigure your operating system in order to proceed -
in such a case follow the instructions printed by the afl tool in your terminal.

```sh
cargo afl build --features="afl-fuzz" --bin fuzz_target_afl
cargo afl fuzz -i afl_in -o afl_out target/debug/fuzz_target_afl
```

The `gen_fuzz` binary is also able to verify data for the afl tool.

```sh
cargo build --bin gen_fuzz
./target/debug/gen_fuzz | ./target/debug/gen_fuzz --check-fuzz
```

### Code Coverage

You may generate the code coverage summary using the `gen_cov_data.sh` script and then a detailed
raport for every file with `cov_report.sh`. Make sure to first install all the required
tools with `install_cov_tools.sh`.

### Resources

- Papers: [current version][paper-link], [old version][old-paper-link]
- docs: [crate documentation][docs-link], [reference][reference-link]

### Future work

- Asynchronous liveness is an important theoretical property and there is a lot of technical
  sophistication that comes in the design of AlephBFT in order to achieve it, however on the practical
  side there is still little evidence that performing such attacks against liveness in real-world
  scenarios is possible. Still, no matter how unlikely such attacks might be, we take them very
  seriously and plan to add randomness to AlephBFT in one of the future releases. We decided to go
  for a version without randomness first, as it gives an incredibly simple and at the same time
  secure and robust BFT consensus protocol. Adding randomness introduces some complexity into the
  protocol, so it makes sense to add it on top of a well-tested, working product. The API of the
  protocol will not change, and we will make the use of randomness configurable.
- We see a big value in keeping a critical piece of code such as a consensus protocol as
  self-contained as possible, so we would like to get rid of the only major dependency -
  `parity-scale-codec`

### License

AlephBFT is licensed under the terms of the Apache License 2.0.

### Funding

The implementation in this repository is funded by [Aleph Zero Foundation][webpage-link].

[//]: ### "badges"
[dataio-link]: https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html#311-dataio
[network-link]: https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html#312-network
[keybox-link]: https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html#313-keybox
[crate-image]: https://img.shields.io/crates/v/aleph-bft.svg
[crate-link]: https://crates.io/crates/aleph-bft
[docs-image]: https://docs.rs/aleph-bft/badge.svg
[docs-link]: https://docs.rs/aleph-bft
[build-image]: https://github.com/Cardinal-Cryptography/AlephBFT/workflows/CI/badge.svg
[build-link]: https://github.com/Cardinal-Cryptography/AlephBFT/actions?query=workflow%3ACI
[license-image]: https://img.shields.io/badge/license-Apache2.0-blue.svg
[license-link]: https://github.com/Cardinal-Cryptography/AlephBFT/blob/main/LICENSE
[rustc-image]: https://img.shields.io/badge/rustc-stable-blue.svg
[//]: ### "general links"
[reference-link]: https://Cardinal-Cryptography.github.io/AlephBFT/index.html
[reference-link-implementation-details]: https://cardinal-cryptography.github.io/AlephBFT/differences.html
[reference-link-api]: https://cardinal-cryptography.github.io/AlephBFT/aleph_bft_api.html
[paper-link]: https://arxiv.org/abs/1908.05156
[old-paper-link]: https://arxiv.org/abs/1810.05256
[aleph-node-link]: https://github.com/Cardinal-Cryptography/aleph-node
[webpage-link]: https://alephzero.org
