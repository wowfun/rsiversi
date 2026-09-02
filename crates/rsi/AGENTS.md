Read the product [contract](README.md) before changing standard composition or
CLI behavior.

- The library owns standard composition, Application and Host Profile
  catalogs, the Session interface and adapters, and Host lifecycle semantics;
  the binary owns only CLI, terminal, signal, and Tokio-runtime concerns.
- Keep default tests keyless, isolated from real user state, and observable
  through the built binary or public library interface.
- The only remote-control surface owned here is the same-user local
  Unix-domain Session Host described by the active Agent Note. Do not extend it
  to Web, TCP, remote identity, Media export, or package management without a
  new owning contract decision.
