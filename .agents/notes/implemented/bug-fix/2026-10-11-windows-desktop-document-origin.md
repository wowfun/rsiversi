---
name: Windows Desktop document origin
---

## Problem

WebView2 exposes the `rsi` custom protocol as `http://rsi.localhost`, but the
Desktop adapter selected and admitted only `rsi://localhost`. Navigation and
Origin validation could reject the actual Windows document, and preview CSPs
named the wrong ancestor.

## Decision

The [Desktop contract](../../../../apps/desktop/README.md) selects one platform
document origin for navigation, Origin admission and response CSP. Its native
protocol callback still requires the normalized `rsi://localhost` authority.
Wry 0.55.1's `custom_protocol_workaround.rs` and WebView2 `prepare_request`
restore that callback URI while retaining the document's HTTP Origin header.
All private requests, including frame polling, use POST and require exactly one
well-formed header naming that exact Origin. Duplicate headers are rejected,
including duplicate equal values. The document transport selects POST even when its payload is absent;
ordinary initial immutable asset GETs may omit Origin. A malformed header is
never treated as an absent one.
The [shared Web document](../../../../apps/web/docs/runtime.md) selects native mode
for the two exact platform locations. Its transport, automatic connection,
clipboard, close recovery and export use the same predicate. A protocol-only
check would select the Worker on Windows even with correct native admission.

## Alternatives considered

Admitting both origins on Windows would widen the native route boundary without
a document that needs it. Checking only the hostname would also admit alternate
schemes, ports or user information. Replacing the normalized callback authority
with the HTTP authority would reject Wry's actual callback request.

## Consequences

This application is desktop-only: Windows uses its actual HTTP origin and its
supported Unix desktop targets keep the custom-scheme origin. Wry also applies
the workaround on Android; that target is outside this adapter's contract and
must establish its own origin selection before admission. Only the root and the
two fixed preview paths may navigate. Tests cover
normalized native requests, opaque and foreign origins, alternate authorities,
and preview CSP. These checks establish adapter admission, not complete Windows
Service startup or a live WebView2 application acceptance result.
Desktop bootstrap uses the product-owned standard Bash selector and the native
companion executable suffix. Those launcher paths do not establish Service
acceptance. Native macOS Origin headers and private frame polling remain
unverified; no additional Origin is admitted on that assumption.
An opt-in hidden WebView2 probe exercises the same request validator against
actual callback URIs and headers under the production Application CSP. It loads
the shared mode predicate as an external module and checks the production close
event injection under that CSP. Registered dialog/clipboard
plugins remain denied to document IPC. It destroys the hidden window, waits at
most five seconds for Windows sharing locks to release, and removes its temporary
browser profile. It does not start a Service; this is transport evidence rather than product startup
acceptance.
