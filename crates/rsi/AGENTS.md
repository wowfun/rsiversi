Read the product [contract](README.md) before changing standard composition or
CLI behavior.

- The core library owns standard composition, Application and Host Profile
  catalogs, product domain adapters and Host lifecycle semantics. Terminal
  application plugins own their argument grammar, input, rendering and signals;
  the binary owns launcher/management parsing, process control and Tokio setup.
- Keep default tests keyless, isolated from real user state, and observable
  through the built binary or public library interface.
- The [application/client foundation decision](../../.agents/notes/implemented/architecture/2026-09-06-application-client-foundation.md)
  authorizes explicit multi-device API, authentication, and application ownership
  changes. Implement each surface with its owning contract and tests; the generic
  Host and Meta never acquire product or remote identity policy.
