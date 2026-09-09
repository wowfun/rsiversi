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

An explicitly enabled AI port (`ai: true`, optionally `tools: false`) implements
Describe, frozen Prepare and consuming Start through the public AI Portable
protocol. It verifies a deterministic binary test credential, emits Language
text and a binary Image body, and never contacts a model provider. The Image
body tests normalized framing and descriptor assembly; it is not a raster codec
fixture. `rsi-ai-portable` tests load this same dynamic library through ordinary
Language and Image routers and verify normal withdrawal and resource release.

The optional `revision-b` feature changes the actual compiled Tool description.
The product native-manager test builds both variants into its own target directory
and retains immutable copies. An unchanged Agent Profile can then demonstrate new
artifact admission, new-generation behavior and the old pin's unchanged behavior.

`retained-finalizer.c` is a separate minimal ABI fixture whose identity loads
successfully and whose FINALIZE deliberately returns failure. The product manager
test compiles it with the native C compiler and runs its retention assertions in
a child process. It checks retained mapping/staging/accounting and the still-held
cache lock after ordinary owners drop; only child-process exit permits cleanup.
It does not provide an Agent Tool or simulate a library-close failure.
Compile-time marker paths can pause IDENTITY, and a compile-time status can make
FINALIZE succeed. The manager race test uses those explicit local controls to
change selection or close admission while native staging is still in flight;
it releases the marker and joins the staging thread even on assertion failure.
An explicit `RSI_NATIVE_ADDON_REPORT` directory on the product `native_addons`
test preserves the two compiled variants, failed-finalizer artifact and redacted
retention receipt for release evidence. Default tests leave no such report.
