Read the family [contract](README.md) before changing API dispatch or transport
ownership.

- Domain plugins own versioned DTOs and operation policy. This family must not
  import product implementations or infer Session semantics.
- Admit bytes before allocation. Immutable buffers and slices keep their lease
  until their last owner drops; encoding and transport scratch have independent
  bounds.
- Select mutation ownership and admission class from registered metadata.
  Disconnect cancels a waiter, never an admitted mutation. Reads and streams
  release on cancellation; retirement closes admission and drains owned work.
- Test these contracts through public interfaces with explicit execution,
  isolated transports and deterministic fixtures. Live authentication and
  provider calls remain opt-in.
