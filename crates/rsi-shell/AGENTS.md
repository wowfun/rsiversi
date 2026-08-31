Read the family [contract](README.md) before changing shell behavior.

- Keep shell argv, environment policy, output classification, and producer
  adaptation in the concrete shell package; generic Jobs and Process own their
  existing lifecycle and native execution contracts.
- Register model-visible definitions only through the unpublished Tool
  registrar, and cover schema plus execution behavior at the sealed Tool
  Runtime seam.
- Never discover an executable or capture ambient environment during
  activation. State native support and settlement guarantees exactly.
