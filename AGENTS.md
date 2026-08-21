- Read [the monorepo architecture](docs/architecture.md) before changing product boundaries or repository layout, and read the nearest product and subtree `AGENTS.md` before changing files below that boundary.
- Keep product-specific architecture and trust rules in the owning subtree; the root owns only cross-product organization and governance.
- Pre-release with no compatibility promise. Prefer the correct foundation over compatibility shims, and update all affected paths, manifests, schemas, fixtures, tests, and contracts together.
- Validate and bound external or durable inputs at their owning boundary. Trust validated, typed safe-Rust values in-process, and document the contract for every unsafe block and operation.
- Keep DRY and avoid restating anything that is already self-explanatory. Give each current contract (rather than change history) one authoritative README, rustdoc, schema, source, or subject document and link to it; **change contracts before implementations**; [Agent Notes](.agents/notes/README.md) own durable decision rationale.
- Do not maintain indexes or inventories for navigation.

## Tests
- For logic changes, add or update the closest meaningful behavior or invariant test, and run the changed test target until it passes.
- Select validation by changed surface and the owning testing guide; use `cargo xtask verify-docs` for repository documentation or instruction changes.
- Treat snapshot, golden, baseline, ignore-list, and expected-failure diffs as intentional contract changes that require review; never refresh them merely to make a gate pass.
- Leave exhaustive suites and platform matrices to CI unless explicitly requested, diagnosing CI, or the change is irreducibly repository-wide.
- Keep default validation deterministic and isolated from real user state, credentials, and live services; real integrations are opt-in.
- Fix or report plausibly related failures and validation blockers. Do not repeat passing checks for formality; report only platforms actually exercised.
