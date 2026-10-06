# Controlled HTTPS preview

The explicit Linux fixture generates a one-day self-signed certificate with
OpenSSL in a private temporary directory and removes the files before listening.
The certificate authorizes no real service. An optional argument records observed
request paths for the native egress assertions.
