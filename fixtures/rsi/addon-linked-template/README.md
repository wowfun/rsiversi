# rsi-addon-linked-template

This independent workspace supplies the linked variant of `cargo xtask addon new`.
It owns an addon library, a composition executable and public-boundary lifecycle
tests. RSI dependencies use the invoking checkout under the
[generator's local SDK contract](../../../crates/tools/rsi-xtask/README.md).

Run `cargo test --locked` and `cargo run --locked`. Tests exercise explicit role
placement, Local invocation, Profile replacement and cleanup without user paths
or credentials. They exercise the current local SDK, including uncommitted
changes. The native addon fixture separately exercises the current Portable wire.

`python3 fixtures/rsi/addon-linked-template/verify.py --report NEW_DIRECTORY`
runs the current generator into a temporary external directory, builds/tests and
runs that generated project with `--locked`, and verifies its lockfile is unchanged.
It retains only logs and hashes in the report; the generated project is removed.
Generated-project artifacts use `CARGO_TARGET_DIR/linked-template` when configured,
otherwise `target/linked-template` under the repository. Relative target paths are
resolved from the verification command's working directory.
The standard Linux CI lane runs this check against the local checkout.
