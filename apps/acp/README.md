# rsi-acp-application

The Unix ApplicationFactory owns ACP stdio, signals and application task cleanup.
It consumes the native Agent backend and local ServingService contracts.
`rsi --profile acp` selects this plugin; there is no separate ACP executable.

Stdio uses nonblocking duplicated descriptors so cancelling an idle pipe read
does not leave a blocking Tokio stdin worker behind. Redirected regular files
use owned file I/O; retirement drains outstanding file work before releasing
descriptors. Stdout carries only NDJSON; bounded categorical diagnostics use
stderr. The Service backend is supplied through a Local contract, and
Application retirement joins protocol cleanup.
