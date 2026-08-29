Read the family [contract](README.md) before changing approval behavior.

- Requests and provenance are bounded and non-secret. Agent durability belongs
  to the Agent owner, not this live decision service.
- Answerers run in deterministic order without registry locks held across
  await. Absence or abstention fails closed to deny.
