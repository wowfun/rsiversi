# rsi-fixture-native-addon

This standalone keyless fixture exports a real ABI v3 dynamic library using the
safe native SDK. Its Tool port implements the public Portable Tool Describe,
Execute and host-confined process-plan exchange. Tool catalog tests consume the
built artifact through NativeCatalog and the ordinary PortableToolsFactory.
It performs no ambient file, credential or provider access and does not spawn a
process; Confine proves use of the invocation's pinned host planner, not process
enforcement on a real operating system.

Run manifest-scoped build, test and strict Clippy with its own lockfile. The
native Tool integration test builds this manifest automatically into
`target/native-addon-fixture-test`. Tests only establish the platform actually
executed; no loader teardown failure or live-provider behavior is implied.
