Read the family [contract](README.md) before changing Settings behavior.

- Keep secret material out of Settings; store only credential references and
  explicitly allowed environment-variable names.
- Preserve namespace ownership, last-good validated values, and revision CAS.
- Local-provider tests use temporary files and never ambient user settings.
