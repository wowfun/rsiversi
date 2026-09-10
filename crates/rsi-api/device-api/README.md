# rsi-api-device-api

The ordinary DeviceApiFactory publishes local-only register, list and revoke
operations over DeviceAdministration. It consumes the existing registry and
authentication owner; it neither opens Storage nor creates another device cache.
DeviceClient is a stateless typed proxy over an already negotiated local API.
Its methods remain asynchronous even though the provider's bounded list is an
in-process snapshot.

Register validates the existing 128-byte label contract and returns one token
with the device identity. Only its explicit wire DTO serializes the secret;
Debug remains redacted and ordinary list never contains tokens. Requests are
bounded to 1 KiB and responses to 32 KiB, covering the complete 64-device roster
even when every label byte requires JSON escaping. The client validates returned labels,
unique identities, the 64-device bound and token shape before exposing values.

Registration and revocation are owned mutations and are never automatically
replayed. If a registration reply is lost, registration may have succeeded but
its one-time token cannot be recovered. The operator can list and revoke the
unwanted device, then explicitly register another. Losing a revoke reply can be
resolved by listing or explicitly repeating that idempotent revocation. A remote
device cannot discover or invoke these operations, including to issue or revoke
another device's credential.
