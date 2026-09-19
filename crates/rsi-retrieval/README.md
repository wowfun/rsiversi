# Web retrieval

Unreadable or malformed Settings and failed workers have distinct closed errors;
neither is reported as an intentional disable, caller cancellation or bad content.

Retrieval owns the ordinary `web_fetch` and Exa-backed `web_search` Tools. They
are disabled by default in `rsi.retrieval` Settings. Fresh Agent compositions
capture those flags in the `rsi.retrieval.config` Domain before sealing; old
Sessions restore their definitions offline. Current disabling still prevents
execution. ToolPolicy remains the authority for Tool calls.

## Network and work bounds

Model-supplied fetch URLs must be HTTP/S on ports 80 or 443, at most 2 KiB,
without user information. Attributed search links are not network requests and
retain the separate syntactic HTTP/S link contract.
Every DNS answer must be public unicast. IPv4 special-use ranges, IPv6 local,
documentation and transition ranges, and discovered DNS64 translations to
non-public IPv4 are refused. A private per-hop client pins the checked addresses
while preserving HTTP Host and TLS identity. Proxies, cookies, automatic
redirects and automatic decompression are disabled. At most five redirects are
followed, within the original scheme/host/port, with validation on every hop.

Each operation has a 30-second deadline including resolution and decoding, a
4 MiB wire limit, 8 MiB decoded limit and 256 KiB extracted UTF-8 text limit.
Identity decoding borrows the bounded response body; compressed decoding owns its
bounded decompression buffer. The final extracted text owns only its retained prefix.
Wire/decompression overflow fails explicitly; extracted text carries a truncation
flag. HTML is tokenized into text without execution or a DOM. Script, style,
template and noscript content is omitted. Content encodings are identity, gzip
and Brotli; unsupported media types or charsets fail explicitly. UTF-8 and
UTF-16 BOMs and declared supported text charsets are decoded before extraction.
Eight admitted operations share bounded owner-held work. DNS uses asynchronous
Hickory resolution through the system-configured nameservers and hosts file,
querying both address families. Lookups use absolute names, a two-second attempt
timeout, at most two attempts per server, and a 64-entry TTL cache. DNS64 discovery
uses that same cache and remains mandatory for IPv6 candidates: discovery failure
is closed, with no well-known-prefix-only fallback. Arbitrary NSS plugins are not
used. Cancelling an operation drops its DNS query futures; OS `getaddrinfo` work
cannot retain its admission or delay shutdown. Blocking decoding may finish unused,
retaining admission until actual completion. Shutdown waits for that work. No
started operation is replayed.

## Search and history

Exa uses only `POST https://api.exa.ai/search` and a Credentials-resolved
`rsi.retrieval/exa` reference. Default result count is five, maximum ten. The
first nonblank highlight supplies each snippet; entries without a safe URL or a highlight are omitted
and counted. There is no additional model call or provider-hosted search.
Response parsing and normalized title, URL, date and text fields are bounded.

ToolResults retain versioned structured sources and useful attributed model
text. Human source views derive from the exact recorded Tool intent/result;
history and rendering never refetch a URL. Opening a URL is an explicit user
action. Enabling Tools and storing/removing the Exa credential are independent
configuration operations; neither submits a search.

## Verification

Default tests use injected DNS/transport and isolated HTTP servers. Production
has no private-address override. Test the public-address policy independently
from the actual pinned HTTP connection, then test their composition. External
fetch, Exa and model integrations are opt-in and are separate evidence classes.
