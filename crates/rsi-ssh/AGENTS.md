Read the family [contract](README.md) before changing SSH transport or target inputs.

- Treat target configuration and every helper frame as bounded external inputs.
  Never use Service SSH aliases, agent sockets or ambient environment as authority.
- Keep target grants and host-key trust decisions in the product owner. Validated
  addresses and public keys are data, not permission to connect.
- Native verification uses an isolated SSH server and explicit test identities.
  Do not use developer keys or read private key contents into test output.
