# Product implementation evidence

**Status:** empty by design; the accepted operator-control-plane target is not yet implemented.

This human view remains empty until a requirement reaches `implemented`. The machine authority is
`PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv`; one row records a full repository-format Git object id, the exact
`acp-<category>-<number>` acceptance id/Make target and the path/hash of one target-specific descriptor.

Each schema-v1 descriptor is tracked at `tests/product-acceptance/descriptors/<target>.tsv`, is byte-identical
in the declared implementation commit and current `HEAD`, and contains exactly one `requirement`, `target`
and hashed `entrypoint`, at least one hashed production `implementation` below `crates/`, optional hashed
`support` files, and the exact relative `artifact` set/hashes. Duplicate records are invalid. The declared
entrypoint—not an overridable Make recipe—is executed directly from a fresh detached checkout of exact
`HEAD`, with a new private empty
`TURN_PRODUCT_ACCEPTANCE_ROOT`, external build root and unpredictable `TURN_PRODUCT_ACCEPTANCE_TOKEN`; it must
record that token under `.oracle-invocations/<target>`. The caller's ignored files cannot influence the run.
The gate rejects stale artifacts, no-op or forged Make targets, extra files, symlinks/FIFOs/sockets/devices,
traversal, a commit predating the oracle, changed sources, untracked authority and any dirty checkout before
or after execution.

A PR, screenshot, test count or prose claim is not evidence. The gate deliberately fails while any
requirement has another status.

| Requirement | Implementation commit | Oracle target | Descriptor and SHA-256 |
| --- | --- | --- | --- |
