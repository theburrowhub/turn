# Product implementation evidence

**Status:** empty by design; the accepted operator-control-plane target is not yet implemented.

This human view remains empty until a requirement reaches `implemented`. The machine authority is
`PRODUCT_IMPLEMENTATION_EVIDENCE_V1.tsv`; one row records a full repository-format Git object id, the exact
`acp-<category>-<number>` acceptance/dispatch id and the path/hash of one target-specific descriptor.

Each schema-v1 descriptor is tracked at `tests/product-acceptance/descriptors/<target>.tsv`, is byte-identical
in the declared implementation commit and current `HEAD`, and contains exactly one `requirement`, `target`
and hashed `entrypoint`, at least one hashed production `implementation` below `crates/`, optional hashed
`support` files, and the exact relative `artifact` set/hashes. Duplicate records are invalid. The declared
entrypoint—not an overridable Make recipe—is executed directly from a fresh detached checkout of exact
`HEAD`, with a new private empty `TURN_PRODUCT_ACCEPTANCE_ROOT`, external build root and random 256-bit
`TURN_PRODUCT_ACCEPTANCE_TOKEN`; it must record that token under `.oracle-invocations/<target>`. Ignored files
in the caller's checkout are absent and cannot influence relative-path reads; OS state, absolute paths and
explicitly inherited environment remain declared test dependencies rather than a claimed sandbox boundary.
The gate rejects stale artifacts, no-op entrypoints, extra files, symlinks/FIFOs/sockets/devices,
traversal, a commit predating the oracle, changed sources, untracked authority and any dirty checkout before
or after execution.

This mechanical gate proves identity, Git provenance, declared source binding, clean-checkout execution,
freshness and exact artifact shape/hash. It does **not** prove that an entrypoint or its production file
semantically satisfies the English ACP oracle: a deliberately trivial implementation can manufacture the
right bytes. Product completion therefore additionally requires §16.2's non-author audit to inspect each
ACP → descriptor → entrypoint/implementation path and the required live, destructive, measured and packaged
evidence. Neither green mechanics nor a self-authored fixture is semantic acceptance by itself.

A PR, screenshot, test count or prose claim is not evidence. The gate deliberately fails while any
requirement has another status.

| Requirement | Implementation commit | Oracle target | Descriptor and SHA-256 |
| --- | --- | --- | --- |
