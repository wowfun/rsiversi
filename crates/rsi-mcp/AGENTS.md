Read the family contract before changing integration behavior.

- Keep wire framing, connection epochs and server manifests here. Agent Kernel
  consumes only generic composition seeds and ordinary Tool/Domain contracts.
- Validate response size before parsing JSON. Reject incomplete or oversized
  discovery without publishing a partial or truncated catalog.
- Never infer authorization from MCP annotations or replay a started call.
- Keep endpoint credentials in Credentials and process ownership in Process.
- Default tests use isolated local protocol servers. External servers are opt-in.
