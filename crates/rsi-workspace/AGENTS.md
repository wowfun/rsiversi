Read the family [contract](README.md) before changing Workspace behavior.

- Workspace identity uses an existing canonical physical directory.
- Deleting a registration must never delete, rename, chmod, or otherwise
  mutate the user's directory or files.
- Test only through the Local service with temporary directories and a real
  domain backend or deterministic domain seam.
