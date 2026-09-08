# API TLS fixture

This self-signed localhost certificate and public test private key exist solely
for isolated HTTP adapter tests. Tests explicitly trust this certificate; ordinary
clients must reject it. The key provides no production authentication authority.
The long validity period avoids replacing a deterministic test asset routinely.
No runtime configuration selects these files by default.
