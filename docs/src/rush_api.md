## 3 API of Rush

### 3.1 Required Trait Implementations.

#### 3.1.1 DataIO.

The DataIO trait is an abstraction for a component that provides data items, checks availability of data items and allows to input ordered data items. `DataIO` is parametrized with a `Data` generic type representing the type of items we would like to order. Below we give examples of what these might be.

```rust
pub trait DataIO<Data> {
    type Error: Debug;
    fn get_data(&self) -> Data;
    fn send_ordered_batch(&mut self, batch: Vec<Data>) -> Result<(), Self::Error>;
    fn check_availability(&self, data: &Data) -> bool;
}
```

Rush internally calls `get_data()` whenever a new unit is created and data needs to be placed inside. The `send_ordered_batch` method is called whenever a new round has been decided and thus a new batch of units (or more precisely the data they carry) is available. Finally `check_availability` is used to validate and check availability of data. The meaning of the latter might be unclear if we think of `Data` as being the actual data that is being ordered, but in applications one often wants to use **hashes of data** (for instance block hashes, see the example below) in which case it is crucial for security that there is access to the actual data, cryptographically represented by a hash. It is assumed that the implementation of DataIO makes best effort of fetch the data in case it is unavailable.

**Note:** above we presented a slightly simplified version of the `DataIO` trait whose `check_availability` method outputs `bool`. The version in the actual implementation returns an `Option`: `None` means that the data is available, `Some(fut)` means that the data is not available and `fut` is a `Future` that will complete at the very moment the data is fetched. Conceptually this still works as explained above and the examples in the later part, but it is a bit more efficient to do it this way in the implementation.

#### 3.1.2 Network.
The Network trait defines the functionality we expect the network layer to satisfy and is quite straightforward:

```rust
pub trait Network<H: Hasher, D: Data, S: Encode + Decode>: Send {
    type Error: Debug;
    fn send(&self, data: NetworkData<H, D, S>, node: NodeIndex) -> Result<(), Self::Error>;
    fn broadcast(&self, data: NetworkData<H, D, S>) -> Result<(), Self::Error>;
    async fn next_event(&mut self) -> Option<NetworkData<H, D, S>>;
}
```

Here `NetworkData` is a type representing possible network messages for the Aleph protocol. For the purpose of implementing the Network trait what matters the most is that they implement the `Encode` and `Decode` traits, i.e., allow for serialization/deserialization thus can be treated as byte arrays if that is more convenient. The `NodeIndex` type represents node indices, i.e., a number between `0` and `N-1`.

The `send` and `broadcast` methods have straightforward semantics: sending a message to a single or to all the nodes. `next_event` is an asynchronous method for receiving messages from other nodes.

**Note on Rate Control**: it is assumed that Network **implements a rate control mechanism** guaranteeing that no node is allowed to spam messages without limits. We do not specify details yet, but in future releases we plan to publish recommended upper bounds for the amounts of bandwidth and number of messages allowed per node per a unit of time. These bounds must be carefully crafted based upon the number of nodes `N` and the configured delays between subsequent Dag rounds, so that at the same time spammers are cut off but honest nodes are able function correctly within these bounds.

**Note on Network Reliability**: it is not assumed that each message that Rush orders to send reaches its intended recipient, there are some built-in reliability mechanisms within Rush that will automatically detect certain failures and resend messages as needed. Clearly, the less reliable the network is, the worse the performarmence of Rush will be (generally slower to produce output). Also, not surprisingly if the percentage of dropped messages is too high Rush might stop making progress, but from what we observe in tests, this happens only when the reliability is extremely bad, i.e., drops below 50% (which means there is some significant issue with the network).



#### 3.1.3 KeyBox.

The `KeyBox` trait is an abstraction for digitally signing arbitrary data and verifying signatures created by other nodes.
```rust
pub trait KeyBox: Index + Clone + Send {
    type Signature: Signature;
    fn sign(&self, msg: &[u8]) -> Self::Signature;
    fn verify(&self, msg: &[u8], sgn: &Self::Signature, index: NodeIndex) -> bool;
}
```

A typical implementation of KeyBox would be a collection of `N` public keys, an index `i` and a single private key corresponding to the public key number `i`. The meaning of `sign` is then to produce a signature using the given private key, and `verify(msg, s, j)` is to verify whether the signature `s` under the message `msg` is correct with respect to the public key of the `j`th node.

### 3.2 Examples

While the implementations of `KeyBox` and `Network` are pretty much universal, the implementation of `DataIO` depends on the specific application. We consider two examples here.

#### 3.2.1 Blockchain Finality Gadget.

Consider a blockchain that does not have an intrinsic finality mechanism, so for instance it might be a PoW chain with probabilistic finality or a chain based on PoS that uses some probabilistic or round-robin block proposal mechanism. Each of the `N` nodes in the network has its own view of the chain and at certain moments in time there might be conflicts on what the given nodes consider as the "tip" of the blockchain (because of possible forks or network delays). We would like to design a finality mechanism based on Rush that will allow the nodes to have agreement on what the "tip" is. Towards this end we just need to implement a suitable `DataIO` object.

For concreteness, suppose each node holds the genesis block `B[0]` and its hash `hash(B[0])` and treats it as the highest finalized block so far.

We start by defining the `Data` type: this will be simply `Hash` representing the block hash in our blockchain. Subsequently, we implement `get_data` (note that we use pseudocode instead of Rust, for clarity)

```
def get_data():
	let B be the tip of the longest chain extending from the most recently finalized block
	return hash(B)
```
This is simply the hash of the block the node thinks is the current "tip".

```
def send_ordered_batch(batch):
	let finalized = the highest finalized block so far
	for block_hash in batch:
		let B be the block such that hash(B) == block_hash
		if there is no such block in local storage then fetch it
		// The fact that block_hash appeared in the batch implies that at least half of the honest nodes hold B, so fetching from random nodes should work
		if finalized is an ancestor of B:
			finalize all the blocks on the path from finalized to B
			let finalized = B
```
Since (because of Rush's guarantees) all the nodes locally observe the same ordered batches, the above implies that all the nodes will finalize the same blocks, with the only difference being possibly a slight difference in timing.

Finally, the availability check is also straightforward. Note that we assume that there is an underlying block import mechanism that is independent from rush and assumes that the blocks disseminate with no significant delays.

```
def check_availability(data):
	let B be the block such that hash(B) == data
	if there is no such B in our local storage:
		// start efforts to fetch B
		return false
	if all the blocks on the path from genesis to B are in local storage:
		return true
	else:
		// start efforts to fetch the missing blocks
		return false
```

We note that availability of `B` alone is not quite enough for this case, as, for instance, if the parent of `B` is missing from out storage, we cannot really think of `B` as available, because we cannot finalize `B`.



#### 3.2.2 State Machine Replication (Standalone Blockchain).

Suppose the set of `N` nodes would like to implement State Machine Replication, so roughly speaking, a blockchain. Each of the nodes keeps a local transaction pool: transactions it received from users, and the goal is to keep producing blocks with transactions, or in other words produce a linear ordering of these transactions. As previously, we demonstrate how one should go about implementing the `DataIO` object for this application.

First of all, `Data` in this case is `Vec<Transaction>`, i.e., a list of transactions.

```
def get_data():
	tx_list = []
	while tx_pool.not_empty() and tx_list.len() < 100:
		tx = tx_pool.pop()
		tx_list.append(tx)
	return tx_list
```
We simply fetch at most 100 transactions from the local pool and return such a list of transactions.

```
def send_ordered_batch(batch):
	let k be the number of the previous block
	let tx_list = concatenation of all lists in batch
	remove duplicated from tx_list
	remove from tx_list all transactions that appeared in blocks B[0], B[1], ..., B[k]
	form block B[k+1] using tx_list as its content
	add B[k+1] to the blockchain
```
Whenever a new batch is receive we simple create a new block out of the batch's content.

When it comes to the availability check, in this case `Data` is not a cryptographic fingerprint of data, but the data itself, so it is available automatically.

```
def check_availability(data):
	return true
```

### 3.3 Guarantees of Rush.
Let `round_delay` be the average delay between two consecutive rounds in the Dag that can be configured in Rush (default value: 0.5 sec). Under the assumption that there are at most `floor(N/3)` dishonest nodes in the committee and the network behaves reasonably well (we do not specify the details here, but roughly speaking, a weak form of partial synchrony is required) Rush guarantees that:

1. Each honest node will make progress in producing to the `out` stream at a pace of roughly `1` ordered batch per `round_delay` seconds (by default, two batches per second).
2. For honest nodes that are not "falling behind" significantly (because of network delays or other issues) it is guaranteed that the data items they input in the protocol (from their local `DataIO` object) will be part of the output stream with a delay of roughly `~round_delay*4` from the time of inputting it. It is hard to define what "falling behind" exactly means, but think of a situation where a node's round `r` unit is always arriving much later then the expected time for round `r` to start. When a node is falling behind from time to time, then there is no issue and its data will be still included in the output stream, however if this problem is chronic, then this node's data might not find its way into the output stream at all. If something like that happens, it most likely means that the `round_delay` is configured too aggresively and one should consider extending the delay.

We note that the issue of an honest node's data not being included in the stream is not too dangerous for most of the applications. For instance, for the two example scenarios:

1. For the **finality gadget** example, most honest nodes see the same blocks and there is a high level of redundancy, so it does not quite matter that some of the nodes are possibly censored.
2. For the **state machine replication** example one must assume that there is some redundancy in the way transactions are distributed among nodes. For instance if there is a gurantee (that one can easily achieve by randomly gossiping each transaction to a smal random subset of nodes) that each transaction appears in the pools of at least `5` nodes, then the issue of censorship essentially goes away.

Still, the most important thing to take away from this section is that if censorship happens, then the configuration of Rush is suboptimal and one should increase the `round_delay`.

### 3.3.1 What happens when not enough nodes are honest.
If there are less than `floor(2/3N)+1` nodes that are online and are honestly following the protocol rules then certain failures might happen. There are two possibilities:

1. **Stall** -- the output streams of nodes stop producing data items. This is also what will happen when the nodes are generally honest, but there is either a significant network partition or lots of nodes crash. If this is not caused by malicious behavior but network issues, the protocol will recover by itself and eventually resume its normal execution.
2. **Inconsistent Output** -- this is the most extreme failure that can happen and can only be a result of malicious behavior of a significant fraction of all the nodes. It means that the honest nodes' output streams stop being consistent. In practice for this to happen the adversary must control *lots* of nodes, i.e., around `(2/3)N`. The type of failure that would usually happen if  the adversary controls barely above `floor(1/3N)+1` is stall.


### 3.4 Rush Sessions.
Currently the API of Rush allows to run a single Session that is expected to last a fixed number of rounds and thus to finalize a fixed number of output batches. By default a Rush Session is `5000` rounds long but out of these `5000` there are `3000` rounds that the protocol proceeds at a regular speed (i.e., `500ms` per round) and after that starts to slow down (each round is `1.005` times slower than the previous one) so that round `5000` is virtually impossible to reach.

There are essentially two ways to use Rush:

1. **Single Session** -- just run a single session to make consensus regarding some specific one-time question. In this case one can run the default configuration and just terminate the protocol once the answer is in the output stream.
2. **Multiple Sessions** -- a mode of execution when Rush is run several times sequentially. An important motivating example is the use of Rush as a finality gadget for a blockchain. Think of session `k` as being responsible for  finalizing blocks at heights `[100k, 100(k+1)-1]`. There should be then an external mechanism to run a new Rush session when the last block of a session gets finalized (and stop inactive sessions as well). This example gives a clear answer for why we opted for the slowdown after round `3000` as explained above: this is to make sure that no matter the variance in block-time of the block production mechanism, and no matter whether there are stalls, network issues, crashes, etc it is guaranteed that the prespecified segment of blocks is guaranteed to be finalized in a given session. Readers who are experienced with consensus engines are surely aware of how problematic it would be if at the end of a session, say, only `70` out of the intended `100` blocks would be finalized. That's why it's better to slow down consensus but make sure it achieves the intended goal.

**Why are there even sessions in Rush?** To answer this question one would need to make a deep dive into the internal workings of Aleph, but a high level summary is: we want to make Rush blazing fast, hence we need to keep everything in RAM (no disk), hence we need to have a round limit, hence we need sessions. For every "hence" in the previous sentence there are extensive arguments to back it, but they are perhaps beyond the scope of this document. We are aware of the inconvenience that it brings -- being forced to implement a session manager, but:

1. We feel that depending on the application there might be different ways to deal with sessions and its better if we leave the task of session managing to the user.
2. In one of the future releases we plan to add an optional default session manager, but will still encourage the user to implement a custom one for a particular use-case.