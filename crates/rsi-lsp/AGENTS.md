Read the [language contract](core/README.md) before changing query or lifecycle behavior.

- Keep language-server policy in this family. Kernel and Meta must not acquire
  language, file-position or server-installation semantics.
- Derive workspace authority from the actual caller. A returned URI or model
  argument cannot grant file access or authorize edits.
- Default tests use bounded fake peers. Actual semantic acceptance selects the
  pinned server explicitly and verifies native process retirement.
