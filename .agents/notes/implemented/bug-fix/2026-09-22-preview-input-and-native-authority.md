---
name: Bound preview decoding and authenticate the native preview parent
comment: Browser behavior and real native input determine the preview trust boundary
---

## Problem

A bounded encoded file can still request an enormous raster allocation. An image
load callback runs after decoding, so its dimension check cannot admit decoding
work. Likewise, checking CSP strings does not establish that an engine enforces
every directive for a custom protocol.

The native WebKitGTK engine probe demonstrates that a foreign `rsi://` parent can
embed a bootstrap despite its response frame-ancestors directive. The same test
with a self source or X-Frame-Options does not restore the intended restriction.
Tauri's remote URL matcher also expands a bare root path to a wildcard; its
custom-scheme path canonicalization makes superficially narrower alternatives
unreliable. The document's window-close permission exists only for fixture input,
while product closing already uses native window events and Application actions.

## Decision

The [preview renderer](../../../../plugins/rsi/web/README.md) checks encoded image
headers before assigning supplied images to a browser decoder, including raster
signatures disguised by non-image MIME labels. Markup data image/font references
join the same admitted resource package; response CSP excludes raw data images.
Its file and resource budgets
remain separate from this decoded-pixel bound. Recognized images share a per-preview
pixel reservation, so adding more individually valid resources cannot multiply
admitted dimensions without a limit. Rejected images retain the normal
paged text/hex surface. The [Files contract](../../../../crates/rsi/session-files-ui/README.md)
owns supported representations and limits.

The bootstrap admits its one-use MessagePort only from its immediate parent with
an exact message Origin matching its own document URL. It closes that port before
executing supplied HTML. Response CSP, native navigation, and this admission gate
have separate jobs: an inert bootstrap may be embedded on affected engines, but
an unauthorized parent cannot deliver executable content.

The [desktop document](../../../../crates/rsi/desktop/README.md) has no Tauri IPC
capabilities. Native close fixtures send WM_DELETE_WINDOW on their isolated Xvfb
display and observe the real drain path. They also verify that JavaScript close
is denied, that failed draft saves keep the window alive, and that duplicate
startup close requests retain one cleanup deadline.

## Alternatives considered

Post-decode canvas resizing or a load-event check cannot prevent the initial
allocation. General animated/vector resource decoding needs a separate resource
budget; this preview path conservatively declines animation and SVG embedded
raster/foreign DOM instead of trusting a browser's decoded memory policy.

A CSP-only fix is insufficient under the measured custom-scheme behavior.
Changing source-list spelling or adding a legacy framing header did not fix the
native probe. The maintained engine fixture exercises both permitted origins and
foreign origins, including an otherwise permissive transport control.

Adding a custom close command or widening a URL pattern to work around Tauri's
matcher would retain unnecessary document authority. Removing the fixture-only
capability leaves native product closing intact and tests its actual input seam.

## Consequences

Supplied static supported images are rejected before decoding when their headers
exceed the pixel bound. This does not bound all raster allocations a script-enabled
document can cause: scripts can create new Blob images and HTTPS mode admits
uninspected remote responses. SVG and script-enabled HTML remain executable
rendering systems, not general CPU or memory sandboxes. HTTPS opt-in allows the document and remote
scripts to transmit supplied contents; the control states that consequence.

The [desktop fixture](../../../../fixtures/rsi/desktop-product/README.md) separates
actual product behavior from the custom-scheme engine probe. The
[browser fixture](../../../../fixtures/rsi/web-product/README.md) uses a real
foreign loopback server and CSP diagnostics so private-network or certificate
failures cannot masquerade as frame-ancestors enforcement. Native Windows and
macOS behavior requires their own runners.
