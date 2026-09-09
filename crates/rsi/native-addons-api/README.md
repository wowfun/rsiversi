# rsi-native-addons-api

This product adapter owns one local-only JSON mutation, `native-addons.refresh`
version 1. It retries the selected source through an explicitly supplied
`NativeAddonAdministration`; it owns no source store, loader, scheduler or discovery.
Its request is exactly `{}`, bounded to 1 KiB; its finite response is bounded to
4 KiB on the API Data lane. Source revisions cross JSON as canonical decimal
strings. Failures are categorical and contain no paths or foreign diagnostics.

The registration owner closes new admission and drains accepted operations.
The product adapter cancels its Control waits before draining; the manager still
joins any in-flight native callback during its own retirement. A lost response does not authorize automatic retry; callers inspect
current state before another explicit refresh. Successful staging is distinct
from new Session generation construction and from an existing Session's pin.
