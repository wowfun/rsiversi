# rsi-ai

This package is the standalone façade. An immutable `Registry` resolves an
exact `ModelRef` into one of five typed model handles. Each handle exposes a
provider-I/O-free `prepare`, a consuming `start`, a validated stream or live
session, and a convenience one-shot method where appropriate. Provider
registration and transport implementation stay behind the façade. Language
providers may additionally expose explicit deferred submission: the caller
polls or cancels once per method call and commits each normalized event batch
with its monotonic checkpoint before resuming.
