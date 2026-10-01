---
name: Frozen MCP templates and request-time image projection
---

## Problem

Static-only MCP discovery excludes template-only servers, while text placeholders
discard image content. A one-time model capability check is insufficient because a later
Step or fork can select a text-only model while retaining the same rich history.

## Decision

Manifest codec 3 freezes template metadata and Disabled, Unsupported and Available
discovery states. Template metadata affects the manifest digest;
operator selection affects the configuration digest. Templates are opt-in per
server. Discovery validates the complete catalog and exposes one bounded resource
reader with server, opaque ID and optional template parameters. Expansion uses iri-string
0.7.14 with UriSpec and a bounded writer. MethodNotFound has an explicit variant.

Import validates images into Media regardless of the current model and preserves
raw MCP JSON and durable Media references. Context projects nested ToolResult
images at request construction: a positive image-tool-result capability keeps
images; No or Unknown produces deterministic descriptor text. Canonical
fold, checkpoints and compaction source digests remain independent of that projection.
Normal user-image validation and text-only compaction retain their contracts.

The semantic request exposes consuming message extraction. Image fallback moves
unaffected messages and rebuilds only results containing images before invoking
the complete-request constructor. This preserves closed request validation while
avoiding a second allocation of the entire retained transcript.

The wire-frame bound remains 1 MiB. Media accepts a caller-tightened canonical
encoding limit, with a 32 MiB aggregate image budget per MCP result. Earlier
successful imports may remain after failure; do not delete shared objects.
This follows the [Media lifetime decision](../../implemented/architecture/2026-09-06-application-client-foundation.md).

## Alternatives considered

Checking only the model that issued a Tool call breaks subsequent model switches.
Putting base64 in text context hides large inputs from image budgets. Silently
dropping malformed templates conflicts with the complete frozen-catalog contract.
Transactional Media deletion requires reference ownership that does not exist.
Exposing mutable request messages would let callers invalidate cross-message and
aggregate bounds; consuming extraction requires reconstruction through the owner
instead. Cloning all messages for one image fallback has no semantic benefit.

## Consequences

Vision-to-text-to-vision requests preserve rich history and content ordering;
fork, checkpoint and installed-summary tests exercise the same behavior.
Template-only servers register the reader. Unknown variables, oversized expansion
and disabled templates cause no resource RPC. Raw Program results remain exact.
Image expansion fails before publication above the remaining canonical budget.
An earlier milestone's frozen static reader and summary cold-resume unchanged.

Opting into a server's templates authorizes the resource range those templates
express; it does not grant local filesystem authority. A malformed enabled
template rejects the candidate catalog. Media has no garbage collector, and the
wire limit intentionally excludes many large screenshots. Rich visual summaries
and image offloading are separate work.
