# rsi-ai-models-api

The [Models contract](../protocol/README.md) owns the shared domain types and semantics.

The ordinary endpoint plugin owns `models/list/1` over the generic API registrar.
It consumes only `LanguageModelsContract`; enumeration does not invoke a provider,
read media, resolve credentials or create a Session. The operation has a closed
4 KiB request and a 1 MiB response bound, enough for all 256 bounded ModelRefs
including JSON escaping. It uses read ownership in the Data lane.

The ordinary client plugin requires the exact negotiated operation and publishes
the same read-only Models contract. It checks count, ordering and exclusive cursor
progress against the requested page. API failure classifications remain intact.
Neither plugin imports native routers, provider implementations or rendering.

Tests exercise the real Language router and its provider registration gates through
registered API calls, including bounded multi-page discovery and response rejection.
