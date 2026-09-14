---
name: Terminal startup and provider discovery
---

## Problem

The TUI created a Session before entering terminal mode. An unconfigured Host
could boot, but its required frozen default model prevented users from reaching
configuration. The generic Backend error also lost its setup diagnostic across
the Session API.

## Decision

The [Session selection decision](../architecture/2026-09-14-session-model-selection.md)
adds model/effort changes to an attached Session. Startup-default persistence is
still explicit and independent. Manual metadata accepts bounded adapter-validated
effort declarations without claiming discovery or issuing a test completion.

Keep terminal and application commands alive without a Session. The explicit
`rsi tui` alias selects the existing Profile; `/login` and `/model` use the
existing setup and managed-provider owners. Freeze an exact model only when
creating a Session. Use a typed setup-required domain error across adapters.

Discover models through a separately authorized provider operation, before a
complete deployment exists. Listing candidates never changes routes. Keep
credential persistence, provider convergence and default selection independent.
Use source-attributed exact official-model capacities only to fill missing
metadata; unknown models require explicit user values.

Treat catalog availability as a recoverable application concern while retaining
Settings and Store error classifications. Bound the setup scan independently of
the per-page wire contract. Reject incomplete upstream lists rather than silently
inventing completeness. Share OpenAI version-prefix joining between discovery
and inference so a usable login cannot persist an unusable inference address.
Retain configuration leases through discovery: admission alone would allow grant
revocation to finish while an authorized credential operation was still running.

Preserve typed Session creation errors until the terminal has selected its
startup state: a catalog check cannot freeze later configuration. Only missing
setup returns fresh startup to home; resume and other failures keep their meaning.
The shared error stays application-neutral. Reserve `/login` and `/model` in the
TUI grammar before Session dispatch; these names intentionally shadow same-name
Session commands without entering the Commands registry or conversation history.
Keep this grammar in the terminal product, outside the process-local Commands
family. Reserve secret input capacity before the first byte so zeroization covers
the allocation throughout editing, including after a prior key has been moved
to credential storage.
Project a bounded menu window from the complete application selection state:
independent route and provider count bounds alone do not bound their combined
display bytes at maximal identifier lengths.

Validate discovered candidates at their producing parser and their consuming API
boundary. Reuse the owning HTTP/API byte budgets rather than serializing typed
snapshots for duplicate size checks; JSON expansion still requires an encoded
reply limit. Preserve the provider owner's convergence diagnostic in model-save
failures instead of substituting a generic refresh instruction for every outcome.

Application command input, completion and the reversible setup wizard follow the
[fixed terminal input decision](../simplification/2026-09-13-terminal-command-input.md).

## Alternatives considered

A fixed DeepSeek deployment would simplify key entry but silently choose a
provider. Making Session headers accept missing models would spread incomplete
configuration into the durable Agent contract. A separate setup executable would
leave the interactive application unable to repair its own configuration.

## Consequences

An isolated Linux PTY starts without settings, completes keyless mock setup and
submits a first message without restart, both embedded and over a daemon.
Application commands are excluded from conversation history; secret values are
excluded from scenes.
Cancellation, permission rejection, stale configuration and uncertain writes
leave a usable application with accurate operation receipts.

Provider listings omit capacity and protocol support. Metadata snapshots can
age, so their provenance is explicit and user configuration remains authoritative.
Platform keyring availability remains platform-owned; no plaintext credential
fallback or implicit real-provider verification is introduced.

The subsequent [file credential decision](../architecture/2026-09-14-file-credentials.md)
supersedes the keyring persistence choice; application setup ownership remains.
