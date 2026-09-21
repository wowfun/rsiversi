# rsi-addon-linked-template

This independent workspace supplies the linked variant of `cargo xtask addon new`.
It owns an addon library, a composition executable and public-boundary lifecycle
tests. All RSI dependencies share one Git source and full revision. Cargo fetches
that immutable SDK; this is not a copied implementation checkout.

Run `cargo test --locked` and `cargo run --locked`. Tests exercise explicit role
placement, Local invocation, Profile replacement and cleanup without user paths
or credentials. They establish source distribution against the pinned SDK, not
validation of uncommitted changes in the parent repository. The native addon
fixture separately exercises the current Portable wire.

`python3 fixtures/rsi/addon-linked-template/verify.py --report NEW_DIRECTORY`
runs the current generator into a temporary external directory, builds/tests and
runs that generated project with `--locked`, and verifies its lockfile is unchanged.
It retains only logs and hashes in the report; the generated project is removed.
The standard Linux CI lane runs this check against the template’s pinned SDK.
