Read the product [contract](README.md) before changing standard composition or
CLI behavior.

- The library owns Base/Headless composition; the binary owns only process and
  CLI concerns.
- Keep default tests keyless, isolated from real user state, and observable
  through the built binary or public library interface.
- Do not add Web, remote control, Media export, or package-management surfaces
  without a new owning contract decision.
