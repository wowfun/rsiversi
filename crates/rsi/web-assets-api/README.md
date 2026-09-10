# rsi-web-assets-api

Authenticated renderer generations over the ordinary API registry. `web-assets.observe`
version 1 owns one application observation, keyed by the trusted caller and a fresh
32-character lowercase hexadecimal application nonce. At most 16 observations
exist. A stream offers one complete, already leased generation before any manifest
or module fetch. It retains the displayed and offered generations until the exact
`web-assets.commit` accepts or rejects that offer. A successful DOM commit accepts;
a failed mount rejects and keeps the displayed generation. Changes coalesce while
an offer is outstanding. No model field selects executable URLs.

Dropping observation, device revocation, registration retirement and owner closure
cancel the producer and release both complete leases. Each item reserves 256 KiB
before copying its admitted catalog. Commit requests are closed, at most 1 KiB,
and consume exactly one outstanding offer; they are never automatically replayed.
Unknown commit outcomes require closing and explicitly reconnecting the observer.

The native server consumes an explicit WebAssetControl; the portable typed client
validates exact negotiated operation policies and catalog metadata. Serve owns the
registrations in its Application, alongside its listener, using existing device
authentication. Static bootstrap files need no renderer lease. Renderer generation
paths and aggregate retained-byte accounting remain owned by WebAssets.
