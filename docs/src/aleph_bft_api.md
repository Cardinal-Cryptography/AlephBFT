## 3 API of AlephBFT

### 3.1 Required Trait Implementations.

#### 3.1.1 DataProvider & FinalizationHandler.

The DataProvider trait is an abstraction for a component that provides data items. `DataProvider` is parametrized with a `Data` generic type representing the type of items we would like to order. Below we give examples of what these might be.

```rust
pub trait DataProvider {
    type Output: Data;

    async fn get_data(&mut self) -> Option<Self::Output>;
}
```

AlephBFT internally calls `get_data()` whenever a new unit is created and data needs to be placed inside. If no data is currently available, the method should return `None` immediately to prevent halting unit creation.

The FinalizationHandler trait is an abstraction for a component that should handle finalized items. Same as `DataProvider` is parametrized with a `Data` generic type.

```rust
pub trait FinalizationHandler<Data> {
    fn data_finalized(&mut self, data: Data);
}
```

Calls to function `data_finalized` represent the order of the units that AlephBFT produced and that hold some data.


#### 3.1.2 Network.

The Network trait defines the functionality we expect the network layer to satisfy and is quite straightforward:

```rust
pub trait Network<H: Hasher, D: Data, S: Encode + Decode>: Send {
    fn send(&self, data: NetworkData<H, D, S>, recipient: Recipient);
    async fn next_event(&mut self) -> Option<NetworkData<H, D, S>>;
}
```

Here `NetworkData` is a type representing possible network messages for the AlephBFT protocol. For the purpose of implementing the Network trait what matters the most is that they implement the `Encode` and `Decode` traits, i.e., allow for serialization/deserialization thus can be treated as byte arrays if that is more convenient. The `Recipient` represents who should receive the message, either everyone or a node with a specific index:

```rust
pub enum Recipient {
    Everyone,
    Node(NodeIndex),
}
```

Additionally `NetworkData` implements a `included_data` method which returns all the `Data` that might end up ordered as a result of this message being passed to AlephBFT. The implementation of `Network` should ensure that the user system is ready to have that `Data` be ordered. In the case of `Data` only representing actual data being ordered (e.g. hashes of blocks of transactions), this means ensuring data availability before passing the messages on.

The `send` method has straightforward semantics: sending a message to a single or to all the nodes. `next_event` is an asynchronous method for receiving messages from other nodes.

**Note on Rate Control**: The Network implementation must include rate control mechanisms to prevent spam attacks. The recommended limits are:

- Maximum bandwidth per node: 5 MB/s for N≤100 nodes
- Maximum messages per second per node: 1000 for N≤100 nodes
- These limits should scale approximately linearly with N for N>100

The exact values should be adjusted based on:
- Network capacity and topology
- Number of nodes (N) 
- Configured round_delay
- Expected transaction volume

**Note on Network Reliability**: it is not assumed that each message that AlephBFT orders to send reaches its intended recipient, there are some built-in reliability mechanisms within AlephBFT that will automatically detect certain failures and resend messages as needed. Clearly, the less reliable the network is, the worse the performarmence of AlephBFT will be (generally slower to produce output). Also, not surprisingly if the percentage of dropped messages is too high AlephBFT might stop making progress, but from what we observe in tests, this happens only when the reliability is extremely bad, i.e., drops below 50% (which means there is some significant issue with the network).

#### 3.1.3 Keychain.

The `Keychain` trait is an abstraction for digitally signing arbitrary data and verifying signatures created by other nodes.

```rust
pub trait Keychain: Index + Clone + Send + Sync + 'static {
    type Signature: Signature;
    fn sign(&self, msg: &[u8]) -> Self::Signature;
    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool;
}
```

A typical implementation of Keychain would be a collection of `N` public keys, an index `i` and a single private key corresponding to the public key number `i`. The meaning of `sign` is then to produce a signature using the given private key, and `verify(msg, s, j)` is to verify whether the signature `s` under the message `msg` is correct with respect to the public key of the `j`th node.

#### 3.1.4 Read & Write – recovering mid session crashes

The `std::io::Write` and `std::io::Read` traits are used for creating backups of Units created in a session. This is a part of crash recovery. Units created are needed for member to recover after crash during a session for Aleph to be BFT. This means that user needs to provide two traits `std::io::Write` and `std::io::Read` that are used for storing and reading Unit that are created by member. At first (without any crash) `std::io::Read` should return nothing. After crash it should contain all data that was stored before in this session.

These traits are optional. If you do not want to recover crashes mid session or your session handling ensures AlephBFT will not run in the same session twice you can pass NOOP implementation here.

[`std::io::Write`](https://doc.rust-lang.org/std/io/trait.Write.html#) should provide a way of writing data generated during session which should be backed up. **`flush` method should block until the written data is backed up.**

[`std::io::Read`](https://doc.rust-lang.org/std/io/trait.Read.html#) should provide a way of retreiving backups of all data generated during session by this member in case of crash. **`std::io::Read` should have a copy of all data so that writing to `std::io::Write` has no effect on reading.**

### 3.2 Examples

While the implementations of `Keychain`, `std::io::Write`, `std::io::Read` and `Network` are pretty much universal, the implementation of `DataProvider` and `FinalizationHandler` depends on the specific application. We consider two examples here.

#### 3.2.1 Blockchain Finality Gadget.

Consider a blockchain that does not have an intrinsic finality mechanism, so for instance it might be a PoW chain with probabilistic finality or a chain based on PoS that uses some probabilistic or round-robin block proposal mechanism. Each of the `N` nodes in the network has its own view of the chain and at certain moments in time there might be conflicts on what the given nodes consider as the "tip" of the blockchain (because of possible forks or network delays). We would like to design a finality mechanism based on AlephBFT that will allow the nodes to have agreement on what the "tip" is. Towards this end we just need to implement a suitable `DataProvider` object and filtering of network messages for AlephBFT.

For concreteness, suppose each node holds the genesis block `B[0]` and its hash `hash(B[0])` and treats it as the highest finalized block so far.

We start by defining the `Data` type: this will be simply `Hash` representing the block hash in our blockchain. Subsequently, we implement `get_data` (note that we use pseudocode instead of Rust, for clarity)

```
def get_data():
	let B be the tip of the longest chain extending from the most recently finalized block
	return Some(hash(B))
```

This is simply the hash of the block the node thinks is the current "tip".

Using the `included_data` method of `NetworkData` we can filter incoming network messages in our implementation of `Network`. The code handling incoming network messages could be

```
def handle_incoming_message(M):
 let hashes = M.included_data()
 if we have all the blocks referred to by hashes:
	 if we have all the ancestors of these blocks:
			add M to ready_messages
			return
	add M with it's required hashes to waiting_messages
	request all the blocks we don't have from random nodes
```

We note that availability of a block alone is not quite enough for this case as, for instance, if the parent of a block is missing from out storage, we cannot really think of the block as available because we cannot finalize it.

When we get a new block we can check all the messages in `wating_messages` and move the ones that now have all the hashes satisfied into `ready_messages`.

```
def handle_incoming_block(B):
	block_hash = hash(B)
	for M in waiting_messages:
		if M depends on block_hash:
			remove the dependency
			if we don't have an ancestor B' of B:
				add a dependency on hash(B') and request it from random nodes
			if M has no more dependencies:
				move M to ready_messages
```

The `next` method of `Network` simply returns messages from `ready_messages`.

Now we can implement handling block finalization by implementing trait `FinalizationHandler`.

```
def data_finalized(block_hash):
	let finalized = the highest finalized block so far
 // We have this block in local storage by the above filtering.
    let B be the block such that hash(B) == block_hash
    if finalized is an ancestor of B:
        finalize all the blocks on the path from finalized to B
```

Since (because of AlephBFT's guarantees) all the nodes locally observe hashes in the same order, the above implies that all the nodes will finalize the same blocks, with the only difference being possibly a slight difference in timing.

#### 3.2.2 State Machine Replication (Standalone Blockchain).

Suppose the set of `N` nodes would like to implement State Machine Replication, so roughly speaking, a blockchain. Each of the nodes keeps a local transaction pool: transactions it received from users, and the goal is to keep producing blocks with transactions, or in other words produce a linear ordering of these transactions. As previously, we demonstrate how one should go about implementing the `DataProvider` and `FinalizationHandler` objects for this application.

First of all, `Data` in this case is `Vec<Transaction>`, i.e., a list of transactions.

```
def get_data():
	tx_list = []
	while tx_pool.not_empty() and tx_list.len() < 100:
		tx = tx_pool.pop()
		tx_list.append(tx)
	return Some(tx_list)
```

We simply fetch at most 100 transactions from the local pool and return such a list of transactions.

```
def data_finalized(tx_list):
	let k be the number of the previous block
	remove duplicated from tx_list
	remove from tx_list all transactions that appeared in blocks B[0], B[1], ..., B[k]
	form block B[k+1] using tx_list as its content
	add B[k+1] to the blockchain
```

Whenever a new batch is received we simply create a new block out of the batch's content.

When it comes to availability, in this case `Data` is not a cryptographic fingerprint of data, but the data itself, so no filtering of network messages is necessary. However, they can be inspected to precompute some operations as an optimization.

### 3.3 Guarantees of AlephBFT.

Let `round_delay` be the average delay between two consecutive rounds in the Dag that can be configured in AlephBFT (default value: 0.5 sec). Under the assumption that there are at most `floor(N/3)` dishonest nodes in the committee and the network behaves reasonably well (we do not specify the details here, but roughly speaking, a weak form of partial synchrony is required) AlephBFT guarantees that:

1. Each honest node will make progress in producing to the `out` stream at a pace of roughly `1` ordered batch per `round_delay` seconds (by default, two batches per second).
2. For honest nodes that are not "falling behind" significantly (because of network delays or other issues) it is guaranteed that the data items they input in the protocol (from their local `DataProvider` object) will have `FinalizationHandler::data_finalized` called on them with a delay of roughly `~round_delay*4` from the time of inputting it. It is hard to define what "falling behind" exactly means, but think of a situation where a node's round `r` unit is always arriving much later then the expected time for round `r` to start. When a node is falling behind from time to time, then there is no issue and its data will be still included in the output stream, however if this problem is chronic, then this node's data might not find its way into the output stream at all. If something like that happens, it most likely means that the `round_delay` is configured too aggresively and one should consider extending the delay.

We note that the issue of an honest node's data not being included in the stream is not too dangerous for most of the applications. For instance, for the two example scenarios:

1. For the **finality gadget** example, most honest nodes see the same blocks and there is a high level of redundancy, so it does not quite matter that some of the nodes are possibly censored.
2. For the **state machine replication** example one must assume that there is some redundancy in the way transactions are distributed among nodes. For instance if there is a gurantee (that one can easily achieve by randomly gossiping each transaction to a smal random subset of nodes) that each transaction appears in the pools of at least `5` nodes, then the issue of censorship essentially goes away.

Still, the most important thing to take away from this section is that if censorship happens, then the configuration of AlephBFT is suboptimal and one should increase the `round_delay`.

### 3.3.1 What happens when not enough nodes are honest.

If there are less than `floor(2/3N)+1` nodes that are online and are honestly following the protocol rules then certain failures might happen. There are two possibilities:

1. **Stall** -- the output streams of nodes stop producing data items. This is also what will happen when the nodes are generally honest, but there is either a significant network partition or lots of nodes crash. If this is not caused by malicious behavior but network issues, the protocol will recover by itself and eventually resume its normal execution.
2. **Inconsistent Output** -- this is the most extreme failure that can happen and can only be a result of malicious behavior of a significant fraction of all the nodes. It means that the honest nodes' output streams stop being consistent. In practice for this to happen the adversary must control _lots_ of nodes, i.e., around `(2/3)N`. The type of failure that would usually happen if the adversary controls barely above `floor(1/3N)+1` is stall.

### 3.4 AlephBFT Sessions.

Currently AlephBFT supports two session modes:

1. **Fixed-length Sessions** - The traditional mode where each session runs for a configured number of rounds (default 5000) with automatic slowdown after round 3000.

2. **Dynamic Sessions** (new) - Sessions can now run indefinitely without slowdown, with external coordination determining when to start new sessions. This mode is recommended for production deployments as it provides better throughput consistency.

For both modes, the following configuration options are available:

```rust
pub struct SessionConfig {
    pub round_limit: Option<u32>, // None for dynamic sessions
    pub round_delay: Duration,    // Default 500ms
    pub slowdown_start: Option<u32>, // Round to start slowdown, None for dynamic
    pub slowdown_factor: f64,    // Multiple for each round after slowdown
}
```

### 3.5 Monitoring and Metrics

AlephBFT exposes several metrics to help monitor the health and performance of the consensus:

- Round progression rate
- Network message counts and sizes
- Unit creation/reception latencies 
- Finalization delays
- Memory usage statistics

These metrics can be accessed via:

```rust
pub trait MetricsProvider {
    fn get_metrics(&self) -> ConsensusMetrics;
    fn register_observer(&mut self, observer: Box<dyn MetricsObserver>);
}
```

Implementations should consider exposing these metrics via standard monitoring solutions like Prometheus.
