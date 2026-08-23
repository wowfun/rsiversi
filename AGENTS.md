- Read [the architecture](docs/architecture.md) before changing repository structure or ownership boundaries, and follow the nearest AGENTS.md within each subtree. Keep architecture and trust rules with their owning subtree; the root owns only repository-wide organization and governance.
- Pre-release with no compatibility guarantees. Prefer the correct foundation over compatibility shims, and update all affected paths, code, tests, and docs together.
- Validate and bound external or durable inputs at their owning boundary. Trust validated, typed safe-Rust values in-process, and document the contract for every unsafe block and operation.
- Keep DRY and avoid restating anything that is already self-explanatory. Give each current contract—not its change history—one authoritative home and link to it; **update contracts before implementation**; [Agent Notes](.agents/notes/README.md) own durable decision rationale.
- Do not maintain indexes or inventories for navigation.

## Tests
- For logic changes, add or update the closest meaningful behavior or invariant test, and run the affected test target until it passes.
- Select verification by changed surface and the owning testing guide; use `cargo xtask verify-docs` for repository docs or instruction changes.
- Treat snapshot, golden, baseline, ignore-list, and expected-failure diffs as intentional contract changes that require review; never refresh them merely to make a gate pass.
- Leave exhaustive suites and platform matrices to CI unless explicitly requested, when diagnosing CI, or when the change is irreducibly repository-wide.
- Keep default validation deterministic and isolated from real user state, credentials, and live services; real integrations are opt-in.
- Fix or report plausibly related failures and validation blockers. Do not repeat passing checks for formality; report only platforms actually exercised.
