# Reviewed Profile product evidence

`profile-leaf-probe` is a fixture-only Local API client. It reads the explicitly
selected isolated Service Host metadata, verifies the process, and negotiates
that exact deployment over the public Unix client. It derives the deployment
gate through the Service Host owner's `local_compatibility_key`; same-UID transport authentication
remains enforced by the owner. It never opens the grant database or source editor.
Its stdin accepts one bounded catalog or grant operation and stdout returns the
typed result. Browser tests use it to issue explicit Device leaf grants; remote
configuration authority alone does not grant source changes.
The probe's Unix transport is unavailable on other platforms; its executable
rejects before reading stdin or consulting Service metadata there.

Build with `cargo build -p rsi-cli --example profile-leaf-probe`. Run `browser.mjs`
with the [shared paired-publication supervisor](../web-product/README.md).
Set `RSI_PROFILE_PROBE` to reuse an explicit paired fixture-build probe; otherwise
it uses the ordinary debug example. Supply
`RSI_PAIRED_BUNDLE` pointing to a complete product publication and an explicit
report directory. It exercises Chromium and Firefox, reviewed publication, original
receipt recovery, stale source, grant revocation, and a narrow viewport.
The Linux desktop fixture consumes the same source UI through its native bridge.
All default evidence uses isolated user state and a deterministic provider.
