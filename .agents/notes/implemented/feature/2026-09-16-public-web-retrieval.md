---
name: Public web retrieval with durable sources
---

## Problem

AI protocol `HostedTool::WebSearch` is a provider capability and does not
supply the ordinary Tools or portable durable sources needed for retrieval. Model-chosen URLs require a stricter network boundary
than operator-configured AI and MCP endpoints.

## Decision

The [retrieval contract](../../../../crates/rsi-retrieval/README.md) lives in its own family and connects through ordinary pre-seal Domain and Tool
contributions. It uses Settings, Credentials and the Configuration grant boundary.
Retrieval starts disabled, displays attributed recorded sources, and preserves
search results separately from generated answers.

The source precedents are DSH `packages/web/web-fetch-http/src/network.ts`
(complete answer validation and pinned lookup), `policy.ts` (URL/origin bounds),
and `packages/web/web-search-exa/src/provider.ts` (highlight-backed sources with
no generated answer). DSH's proxy route delegates DNS to the proxy; RSI's
public-only contract instead disables proxies. Local composition seeds and
Domain restoration already preserve MCP definitions without network discovery.

## Alternatives considered

- Provider-hosted search couples availability and response semantics to the
  model backend; it cannot establish this ordinary Tool contract.
- Reusing MCP transport would conflate administrator-selected endpoints with
  arbitrary model-selected URLs and permit private-network access.
- Rendering HTML or refetching history adds executable content or changes the
  evidence after the recorded turn.

## Consequences

Deterministic tests cover complete public DNS validation, pinning, redirects,
compressed and extracted size limits, and cancellation with retained admission.
A local HTTP fixture checks credential POST bytes and redirect refusal; source
normalization retains only actual provider highlights. TUI and GUI use the
recorded source DTO without network work on history reads. Production public
fetch and real-model product runs remain opt-in and distinct from controlled
HTTP fixtures. Authenticated Exa verification requires its separate credential.

### Trade-offs

Retrieval uses Hickory's asynchronous DNS client to keep resolver latency inside
operation cancellation and shutdown. This adds a resolver dependency and follows
system nameservers and hosts data rather than arbitrary NSS plugins. Returning a
permit while an OS `getaddrinfo` worker remained alive would allow unbounded
background work; retaining it could pin all eight operations and block teardown.
The async boundary avoids both outcomes. DNS64 discovery is cached under DNS TTLs
and fails closed: a well-known-prefix-only fallback would miss a resolver's
network-specific translations to private IPv4. Fetch restricts destination ports
to 80 and 443; exposing arbitrary public TCP ports is outside the web Tool's purpose.
Conservative address policy and same-origin redirects can reject legitimate
sites; failures remain explicit and do not weaken the public-network boundary.
