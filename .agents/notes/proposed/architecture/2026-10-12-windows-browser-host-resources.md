---
name: Host resources for a dedicated Windows Browser runtime
---

## Problem

The [Browser worker](../../../../crates/rsi/browser/runtime/worker.mjs) creates
Chromium and MCP pipes inside its restricted process. Native Windows probes with
the selected Low IL write-restricted DSH token refuse these allocations.
Host-created CDP pipes pass a transport baseline, but installed Chromium also
fails broker desktop, Mojo pipe and cache ACL operations. The user selected
retaining DSH and investigating a dedicated runtime. The
[current Browser contract](../../../../crates/rsi/browser/README.md) remains
authoritative; component probes do not establish Windows readiness.

Chromium evidence is pinned to revision
`782af9cb30a53f54487e5d2e44738645a8ec457c`, reported by tested binaries.
Its [broker initialization](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/sandbox/policy/sandbox.cc)
requires an alternate Window Station. Its
[Mojo channel](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/mojo/public/cpp/platform/platform_channel.cc)
uses a null pipe SD whose actual DACL omits the scratch restriction SID despite
TokenDefaultDACL. The identified failure belongs to the broker, not a proven
renderer startup failure.

## Proposal

Keep native policy preparation with Sandbox, native launch/transports/Job
settlement with Process, and artifact/child-role manifests, CDP/MCP routing,
private profiles and purpose readiness with Browser. Generic Host and Meta
acquire no Chromium policy.

Start integration with a dedicated pinned Headless build. Its
[Windows crash reporter path](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/headless/lib/headless_content_main_delegate.cc)
avoids the full Chrome crashpad initialization that fails under DSH. A new build
requires a new artifact digest and actual native acceptance.

Host preparation creates separate new broker/renderer desktops on a noninteractive
station, preserving the existing station's DACL/label and Chromium's
[Restricted SID desktop deny mask](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/sandbox/win/src/window.cc).
Add an explicit SDK adoption seam; validate scope/object identity, Low labels and
new-object ownership before adoption. A handle or name alone grants no authority
to adopt an existing object. Retain owned desktops until the outer Job settles.
The native component passes Low broker opening the Host-created renderer desktop
and launching its child with that distinct desktop in HANDLE_LIST/lpDesktop.

Delegate only role-specific restricted tokens, never the unrestricted Host token.
Chromium's [token policy](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/sandbox/win/src/sandbox_policy_base.cc)
and [suspended launch](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/sandbox/win/src/broker_services.cc)
separate permanent primary and startup impersonation tokens. A component passes
with a Low primary containing deny-only user/groups, Null restriction and zero
privileges, plus an immutable Low initial token fully restricted to
World/logon/current scratch. It preserves Win32k lockdown, reaches Untrusted IL
and drops impersonation. It can write scratch during bootstrap, cannot write an
outside Low file in either phase, and loses scratch write after lockdown.
This stronger initial shape has not passed the actual SDK loader. Renderer policy
cannot substitute for GPU, utility, Network or Lowbox policy; frozen roles must
preserve upstream mitigations and confinement throughout bootstrap.

Create private Mojo pipes with protected scope-specific DACLs and Low labels,
only required pipe rights, overlapped I/O, anonymous client SQOS, first-instance
creation and remote-client refusal. Actual payload and same-user different-scope
refusal pass in native components. Apply this contract to every creation/retry
path. Synthetic fixed probe SIDs do not implement fresh production capabilities.

Prepare AppContainer/LPAC access only on newly owned private profiles or staged
runtime bytes. Chromium's [Network data admission](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/content/browser/network_sandbox.cc)
recursively changes ACLs, which scratch Modify cannot do. Freeze the role, target
and capability; reject arbitrary paths/ACL requests from the broker. Verify
staged bytes before admission. Installation-source ACLs remain outside authority;
actual LPAC loading and cross-scope data isolation still require probes.

Host-only Lowbox construction preserves the source restriction SIDs and zero
lockdown-main privileges in a native probe. This is not LPAC process acceptance:
the pinned [startup helper](https://chromium.googlesource.com/chromium/src/+/782af9cb30a53f54487e5d2e44738645a8ec457c/sandbox/win/src/startup_information_helper.cc)
uses security-capability and all-application-packages opt-out creation attributes,
with distinct Lowbox and derived/profile branches. Preserve that role distinction
and prove actual access behavior rather than inferring LPAC from token creation.

Launch Chromium and pinned MCP Node through Process-owned handles, replacing both
worker spawn sites. Bound/authenticate the control transport before resource
delivery. Separate purpose/assurance readiness so Windows partial assurance
cannot satisfy a strong checker. This proposal adds no Windows OS egress guarantee.

## Alternatives considered

Medium IL still reproduces Mojo/cache refusals. An interactive-station desktop
passes a component but drops the alternate-station property. No-sandbox flags,
unrestricted replay and broad broker WRITE_DAC abandon the selected boundary.
Dedicated-user setup is a different provisioning contract already excluded here.
TokenDefaultDACL alone does not change the observed default pipe SD.

## Acceptance criteria

Build and pin actual SDK/Headless bytes. A Win11 ordinary-user run must verify the
DSH broker, every child role's tokens/mitigations, bootstrap confinement,
Lowbox/LPAC state, desktop ownership and inherited handle set. Unknown roles or
incomplete preparation fail before child admission.

Pass actual CDP DOM evaluation and visually inspected screenshots, private MCP,
runtime-byte pinning, same-user cross-Session refusal, bounded saturated I/O,
cancellation and close. Abrupt Host death must empty the whole Job and retire
owned resources. Publish readiness only after these receipts; Win32 components
or a Medium Chromium baseline are insufficient.

## Risks

The SDK initializes shared IPC/interceptions before CRT startup; the passing
standalone child does not reproduce this machinery. AppContainer policy and
recursive capability grants need further authority/isolation checks. A dedicated
runtime adds pinned patch/producer maintenance. Current local preflight lacks
depot_tools, a full checkout and SDK x64 Debugging Tools; no complete Chromium
build exists. Independent Windows work continues while this feature and the
whole-support claim remain gated.
