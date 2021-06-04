## 4 Differences between Rush and Aleph.

There are several differences between the Aleph as described in the [paper](https://arxiv.org/abs/1908.05156) and the Aleph implemented in Rush. Many of them are already described in previous sections but for completeness we briefly list the differences here.
1. The main version of Aleph uses Reliable Broadcast to disseminate units. The Rush implementation is closer to QuickAleph (in the Appendix of the paper) that uses Reliable Broadcast only for Alerts.
2. The specifics of alerts are different in the Rush implementation -- in particular they do not require to freeze the protocol at any moment and are generally simpler.
3. Rush uses its own variant of Reliable Broadcast -- see the section [Reliable Broadcast](reliable_broadcast.md##reliable-broadcast).
4. Differences in the use of randomness -- see [Randomness in Aleph](what_is_aleph.md#24-randomness-in-aleph).
5. The parent choice rule for units in the Aleph paper is different: it allows parents from older rounds as well, not only the previous round. The reason why the paper allows older rounds is to guarantee a theoretical censorship resistance property in the asynchronous setting. In practice this is irrelevant and getting rid of that makes the protocol simpler and more efficient.
6. The main version of Aleph uses a full list of parent hashes instead of control hashes -- the latter is described in the Appendix as an optimization.
7. The paper's Appendix proposes the use of random gossip as a method of disseminating units -- Rush uses repeated broadcast + a request/response mechanism instead, which according to our experience performs much better in practice.