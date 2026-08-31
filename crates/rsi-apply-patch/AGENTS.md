Read the family [contract](README.md) before changing apply-patch behavior.

- Keep the patch grammar, bounded helper wire, filesystem mutation, and exact
  partial-effect reporting together in this family; do not route them through
  a shell implementation.
- Resolve mutable filesystem paths descriptor-relatively without following
  symlinks, complete preflight before mutation, and never replay an invocation
  whose effects may have begun.
- Keep the patch engine private. Cover model-visible schema and helper dispatch
  at public Tool Runtime and process boundaries, and fail closed on targets
  without equivalent native filesystem semantics.
