Read the family [contract](README.md) before changing filesystem behavior.

- Keep native filesystem mechanics independent of Session, Workspace trust,
  API authentication and model Tool admission.
- Root handles own the selected directory; later relative reads must not reopen
  that authority through a potentially replaced absolute path.
- Test confinement and replacement using canonical temporary roots and explicit
  symlink fixtures. Report only platforms actually exercised.
