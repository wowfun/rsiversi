---
name: Package-owned development documentation
---

## Problem

Terminal development and debugging instructions describe a single plugin but
resided in repository-wide docs. The documentation checker enforced taxonomy
only at repository and product-family roots.

## Decision

Keep those tutorials and references in the terminal package's own `docs`
directory, with a repository-level quick start limited to compiling and choosing
a development surface. Apply the existing documentation taxonomy to `docs`
directly below actual Cargo packages as well as product families. Nested package
docs are not workspace members and need no workspace exclusion.

Product subsystem references own cross-package ordering and lifecycle semantics;
package READMEs and rustdoc retain exact package interfaces. Product architecture
links those references through its explanation of ownership.

## Alternatives considered

Keeping plugin instructions at root blurs ownership. A separate navigation
inventory would duplicate the owning README and is unnecessary.

## Consequences

Package docs receive the same taxonomy and recursive Markdown-link checks.
The existing package README remains the entry point for its development and
debugging instructions.
