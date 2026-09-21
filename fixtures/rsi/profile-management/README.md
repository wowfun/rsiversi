# Reviewed Profile product evidence

`profile-leaf-probe` is a fixture-only Local API client. It reads the explicitly
selected isolated Service Host metadata, verifies the process, and negotiates
that exact deployment over the public Unix client. It uses the advertised build
identity to derive the compatibility gate; same-UID transport authentication
remains enforced by the owner. It never opens the grant database or source editor.
Its stdin accepts one bounded catalog or grant operation and stdout returns the
typed result. Browser tests use it to issue explicit Device leaf grants; remote
configuration authority alone does not grant source changes.

Build with `cargo build -p rsi --example profile-leaf-probe`. Run `browser.mjs`
with `RSI_WEB_ASSETS` pointing to a fresh product build and an explicit report
directory. It exercises Chromium and Firefox, reviewed publication, original
receipt recovery, stale source, grant revocation, and a narrow viewport.
The Linux desktop fixture consumes the same source UI through its native bridge.
All default evidence uses isolated user state and a deterministic provider.
