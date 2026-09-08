These fixtures verify the public [API contract](../../crates/rsi-api/README.md).
Keep transport fakes, bounded inputs and explicit synchronization isolated from
user state. Browser evidence must execute real Workers and report actual engines;
WASM compilation alone does not establish lifecycle behavior. Use public contracts
without importing implementation-private state.
