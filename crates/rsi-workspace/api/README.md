# rsi-workspace-api

The [Workspace contract](../protocol/README.md) owns the shared domain types and semantics.

The ordinary endpoint plugin registers version 1 Workspace get, list, registration,
status and deletion operations through the generic API registrar. Closed DTOs,
bounds and read/mutation policy belong here. The endpoint requires a Workspace
registry and grants no additional filesystem authority; registration requires an
absolute path on that registry's host. Deletion removes only the registration.

The ordinary client plugin requires one negotiated API client and publishes the
Workspace registry contract. It requires all five exact operation descriptors
before publication. Record identities, paths and page cursors are validated at the
wire boundary; host paths remain opaque to the client OS. Failed semantic
validation after a registration mutation reports an unknown outcome. API failures
retain their original classification; domain errors use closed bounded codes.
Native storage or commit-task failure remains infrastructure failure. After
mutation dispatch it cannot prove that the durable registration was unchanged.

Requests use at most 128 KiB, single records at most 128 KiB, and pages at most
32 MiB for up to 256 paths including JSON escapes. All operations use the Data
lane. Registration and deletion transfer mutation ownership to the dispatcher.
Plugin retirement fences its routes and drains admitted work before dependencies
withdraw. Neither plugin depends on Session or native filesystem implementations.

Run package tests against an isolated real Workspace/Storage composition and HTTP
connection. Browser production builds use only these shared protocols and plugins;
native implementations remain outside that dependency closure.
