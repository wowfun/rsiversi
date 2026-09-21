"""Independent legacy MCP fixture: credentials checked, never returned."""
import json
import os
import sys
import signal

assert os.environ["ACP_FIXTURE_SECRET"] == "private-mcp-secret-fixture"
assert os.environ["ACP_FIXTURE_EMPTY"] == ""
with open(sys.argv[1], "w") as marker:
    marker.write(str(os.getpid()))
stall = len(sys.argv) > 2 and sys.argv[2] == "stall"
if stall:
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
for line in sys.stdin.buffer:
    if stall:
        continue
    request = json.loads(line)
    method = request.get("method")
    if "id" not in request:
        continue
    if method == "server/discover":
        response = {"error": {"code": -32601, "message": "legacy fixture"}}
    elif method == "initialize":
        response = {"result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "acp-fixture", "version": "1"}}}
    elif method == "tools/list":
        response = {"result": {"tools": [{"name": "echo", "inputSchema": {"type": "object", "properties": {"message": {"type": "string"}}, "required": ["message"], "additionalProperties": False}, "annotations": {"readOnlyHint": True}}]}}
    elif method == "tools/call":
        if os.environ.get("ACP_FIXTURE_CALLS"):
            with open(os.environ["ACP_FIXTURE_CALLS"], "a", encoding="utf-8") as calls:
                calls.write(json.dumps(request["params"]) + "\n")
        response = {"result": {"content": [{"type": "text", "text": request["params"]["arguments"]["message"]}]}}
    else:
        raise AssertionError("unexpected MCP method")
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], **response}), flush=True)
