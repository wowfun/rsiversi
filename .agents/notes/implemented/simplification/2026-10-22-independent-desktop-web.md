---
name: Independent Desktop and Web packages
---

## Problem

Desktop's exact Web digest dependency blocks Web-only rebuilds. It pairs artifacts
without actually proving their native bridge contracts agree.

## Decision

Desktop checks the actual document generation's bridge descriptor before opening
a window, including supervised Vite overlays. Packaged Web receipts bind that
descriptor to bootstrap identity. A supervised frontend instead reports its baked
descriptor through the private launcher channel; the selected frontend executable
is trusted to describe the document it serves. Desktop no longer pins a Web digest; remote install adds Web
only when absent. Package hashes, signatures, dependency closure and leases remain.

## Alternatives considered

Requiring a new Desktop artifact for every Web update prevents independent work.
Checking only after opening the window gives an unusable application. Declaring the
bridge as a Core-provided protocol misrepresents its owner.

## Consequences

Bootstrap and bridge changes require reattachment; renderer-only refresh can
publish in place. The pre-release manifest switch requires a new binary installed
to an empty prefix, then all old selections removed and unleased objects collected.
Trust high-water records and user data survive; old-format parsers are not retained.
