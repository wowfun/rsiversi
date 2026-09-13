"""Deterministic provider for Session API mechanism evidence, never model scoring."""
import http.server
import json
import shlex
import threading

SUMMARY_TEXT = "API_PRESSURE_SUMMARY: Earlier inspection is complete. The fixed repair still needs to be applied and verified."
SUMMARY_FRAME = "[Internal context summary; source Facts remain authoritative]\n" + SUMMARY_TEXT
PRESSURE_TEXT = "Earlier implementation findings. " * 3500
BOUNDARY_MARKER = "API_BOUNDARY_OK"


class Provider:
    def __init__(self, pressure=False):
        self.lock = threading.Lock()
        self.step = 0
        self.patch = ""
        self.goal = ""
        self.requests = []
        self.pressure = pressure
        self.boundary_command = ""
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                length = int(self.headers.get("Content-Length", "0"))
                if not 0 < length <= 2 * 1024 * 1024:
                    self.send_error(413)
                    return
                request = json.loads(self.rfile.read(length))
                summary = not request.get("tools")
                with owner.lock:
                    step = owner.step
                    if not summary:
                        owner.step += 1
                    owner.requests.append({"step": step, "stage": owner.goal, "model": request.get("model"),
                                           "summary": summary, "messages": request.get("messages", []),
                                           "tools": [tool.get("function", {}).get("name") for tool in request.get("tools", [])]})
                    if summary:
                        call = None
                    elif step == 0:
                        call = ("bash", {"command": owner.boundary_command})
                    elif step == 2:
                        call = ("apply_patch", {"patch": owner.patch})
                    elif step == 3:
                        call = ("report_goal", {"goal_id": owner.goal, "kind": "complete",
                                                "evidence": "The scripted repair is ready for the independent oracle."})
                    else:
                        call = None
                delta = {"content": "Continue in the next automatic round." if step == 1 else "Scripted stage complete."}
                if summary:
                    delta = {"content": SUMMARY_TEXT}
                elif owner.pressure and step == 1:
                    delta = {"content": PRESSURE_TEXT}
                finish = "stop"
                if call:
                    delta = {"tool_calls": [{"index": 0, "id": f"{owner.goal}-call-{step}", "type": "function",
                              "function": {"name": call[0], "arguments": json.dumps(call[1])}}]}
                    finish = "tool_calls"
                chunks = [{"choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
                          {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                           "usage": {"prompt_tokens": 105000 if owner.pressure and step == 1 and not summary else 100, "completion_tokens": 40}}]
                body = ("".join("data: " + json.dumps(chunk) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def stage(self, task, stage):
        previous = task["initial"] if stage == 0 else task["reference"][stage - 1]
        patch = ["*** Begin Patch"]
        for name, result in task["reference"][stage].items():
            if previous[name] == result:
                continue
            patch.extend([f"*** Update File: {name}", "@@"])
            patch.extend("-" + line for line in previous[name].splitlines())
            patch.extend("+" + line for line in result.splitlines())
        patch.append("*** End Patch")
        with self.lock:
            self.step = 0
            self.patch = "\n".join(patch) + "\n"
            self.goal = f"eval-goal-{stage + 1}"

    def configure(self, root):
        targets = [str(root / "config/rsi/settings.json"), str(root / "state/rsi/agent/sessions.sqlite3")]
        probe = f'''import os
from pathlib import Path
assert not any(name in os.environ for name in ("DEEPSEEK_API_KEY", "RSI_OPENAI_COMPATIBLE_API_KEY"))
for target in {targets!r}:
    try:
        Path(target).write_text("forged by boundary probe")
    except OSError:
        pass
    else:
        raise AssertionError("Tool can overwrite Host state: " + target)
local = Path(".boundary-probe")
local.write_text("workspace remains writable")
local.unlink()
print({BOUNDARY_MARKER!r})
'''
        self.boundary_command = "python3 -c " + shlex.quote(probe)
        profile = root / "config/rsi/host-profiles/live/host.profile.toml"
        profile.write_text(f'''format = 1
[[steps]]
kind = "plugin"
id = "scripted-provider"
plugin = "rsi.ai.provider.openai-compatible"
[steps.config]
deployment = "live-deepseek"
endpoint = "http://127.0.0.1:{self.server.server_port}"
path = "/v1/chat/completions"
allow_image_input = false
credential = {{ owner = "rsi.ai.provider.openai-compatible", slot = "default" }}
[steps.config.language_models.fixture-model]
context_window_tokens = 128000
default_output_reserve_tokens = 4096
max_output_reserve_tokens = 16384
''')

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        if self.thread.is_alive():
            raise RuntimeError("scripted provider failed to close")
