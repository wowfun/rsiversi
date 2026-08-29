# rsi-storage

This package defines the bounded JSON KV backend contract and provides the
ordinary `rsi.storage` hub plugin. A backend registration belongs to the
registering plugin generation and disappears when its lease is dropped.

The hub performs exact-name routing only. It does not open files, choose a
default backend, retry failed writes, or expose mutable registry internals.
