import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import http from "node:http";
import net from "node:net";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

export function boundedRun(binary, args, options = {}) {
  const result = spawnSync(binary, args, { timeout: 300_000, maxBuffer: 2 * 1024 * 1024, ...options });
  assert.equal(result.status, 0, result.error?.message ?? result.stderr?.toString());
  return result;
}
export async function deadline(promise, label, ms = 30_000) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${label} timeout`)), ms); })]); }
  finally { clearTimeout(timer); }
}
function text(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) return content.map(part => part.text ?? "").join("\n");
  return "";
}
function sse(delta, finish = "stop") {
  return `data: ${JSON.stringify({ choices: [{ delta: { role: "assistant", ...delta }, finish_reason: null }] })}\n\ndata: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: finish }], usage: { prompt_tokens: 20, completion_tokens: 30 } })}\n\ndata: [DONE]\n\n`;
}
async function startProvider(onRequest) {
  const requests = [];
  const sockets = new Set();
  const server = http.createServer(async (request, response) => {
    try {
      if (request.method !== "POST" || request.url !== "/v1/chat/completions") { response.writeHead(404).end(); return; }
      const chunks = []; let length = 0;
      for await (const chunk of request) { length += chunk.length; if (length > 4 * 1024 * 1024) { response.writeHead(413).end(); return; } chunks.push(chunk); }
      const body = JSON.parse(Buffer.concat(chunks).toString());
      if (requests.length === 512) { response.writeHead(429).end(); return; }
      const messages = body.messages ?? [];
      const lastUser = messages.findLastIndex(message => message.role === "user");
      const prompt = text(messages[lastUser]?.content).slice(0, 2048);
      const completedTool = messages.slice(lastUser + 1).some(message => message.role === "tool");
      const images = (Array.isArray(messages[lastUser]?.content) ? messages[lastUser].content : []).filter(part => part.type === "image_url").map(part => {
        assert.match(part.image_url.url, /^data:image\/png;base64,/);
        const bytes = Buffer.from(part.image_url.url.split(",")[1], "base64");
        assert.equal(bytes.subarray(1, 4).toString(), "PNG");
        return { sha256: createHash("sha256").update(bytes).digest("hex"), width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
      });
      requests.push({ prompt, completedTool, model: body.model, images });
      onRequest?.(body);
      response.writeHead(200, { "content-type": "text/event-stream" });
      if (prompt.includes("hold this turn") && !completedTool) {
        response.write(`data: ${JSON.stringify({ choices: [{ delta: { role: "assistant", content: "Waiting for cancellation." }, finish_reason: null }] })}\n\n`);
        const timer = setTimeout(() => response.end(sse({ content: "Wait deadline reached." })), 60_000);
        response.on("close", () => clearTimeout(timer)); return;
      }
      if (prompt.includes("long streamed reply")) {
        for (let index=0;index<180;index++) response.write(`data: ${JSON.stringify({choices:[{delta:{role:"assistant",content:`segment ${index}\n`},finish_reason:null}]})}\n\n`);
        response.end(sse({content:"Stream complete."})); return;
      }
      if (prompt.includes("Markdown example")) {
        response.end(sse({ content: "## Review notes\n\nThe **Unicode 界** result has `literal <code>` and [documentation](https://example.com/docs).\n\n- Preserve source\n- Keep output bounded\n\n```sh\nprintf 'hello'\n```\n\n<script>window.markdownExecuted = true</script>\n\n![Remote alt text](https://example.com/never-fetch.png)\n\n[Unsafe link](javascript:alert%281%29)" })); return;
      }
      let name; let argumentsValue;
      if (!completedTool && prompt.includes("inspect a child task")) {
        name = "spawn_agent"; argumentsValue = { task_name: "inspect-child", message: "Child inspector evidence", fork_turns: "none" };
      } else if (!completedTool && prompt.includes("ask a question")) {
        name = "ask_user"; argumentsValue = { questions: [{ id: "color", prompt: "Which accent should the workspace use?", options: ["Teal", "Blue"] }, { id: "reason", prompt: "What matters for this change?", options: [] }] };
      } else if (!completedTool && prompt.includes("run the failing command")) {
        name = "bash"; argumentsValue = { command: "printf 'fixture stdout\\n'; printf '%16384s' '' | tr ' ' x; printf 'OUTPUT-NEXT\\000\\377\\n'; printf 'fixture stderr\\000\\377\\n' >&2; exit 7\n# <script>window.untrustedExecuted = true</script> " + "界".repeat(23_000) + " SOURCE-END" };
      }
      if (name) {
        response.end(sse({ tool_calls: [{ index: 0, id: `web-tool-${requests.length}`, type: "function", function: { name, arguments: JSON.stringify(argumentsValue) } }] }, "tool_calls"));
      } else {
        response.end(sse({ content: completedTool ? "The tool result has been reviewed. The conversation can continue." : `Reviewed: ${prompt}\n\nThe workspace keeps this conversation separate from the other pane.\n\n<script>window.untrustedExecuted = true</script>` }));
      }
    } catch { if (!response.headersSent) response.writeHead(400); response.end(); }
  });
  server.maxConnections = 32;
  server.requestTimeout = 10_000;
  server.on("connection", socket => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  return { origin: `http://127.0.0.1:${server.address().port}`, requests,
    async close() { const closed = new Promise(resolve => server.close(resolve)); for (const socket of sockets) socket.destroy(); await closed; } };
}

export async function startService({ binary, assets, report, configure, onRequest }) {
  const temporary = await mkdtemp(join(tmpdir(), "rsi-web-"));
  const provider = await startProvider(onRequest);
  let child; let stopped;
  let stderr = "";
  const workspace = join(temporary, "workspace");
  const inherited = Object.fromEntries(["PATH", "LANG", "RUSTUP_HOME", "CARGO_HOME"].filter(key => process.env[key]).map(key => [key, process.env[key]]));
  const env = { ...inherited, HOME: join(temporary, "home"), XDG_CONFIG_HOME: join(temporary, "config"), XDG_STATE_HOME: join(temporary, "state"), XDG_CACHE_HOME: join(temporary, "cache"), XDG_RUNTIME_DIR: join(temporary, "runtime"), DBUS_SESSION_BUS_ADDRESS: `unix:path=${temporary}/absent-session-bus`, RSI_OPENAI_COMPATIBLE_API_KEY: "isolated-fixture-secret" };
  const run = args => boundedRun(binary, args, { cwd: workspace, env, encoding: "utf8", timeout: 30_000 });
  async function close() {
    let failed;
    if (child && child.exitCode === null) {
      child.kill("SIGTERM");
      try {
        const [code, signal] = await deadline(stopped, "service stop", 20_000);
        if (code !== 0 || signal !== null) failed = new Error(`Service cleanup failed: exit ${code}, signal ${signal}`);
      }
      catch { child.kill("SIGKILL"); await stopped; failed = new Error("Service exceeded clean shutdown deadline"); }
    }
    await writeFile(join(report, "service.stderr.log"), stderr);
    await provider.close();
    await rm(temporary, { recursive: true, force: true });
    if (failed) throw failed;
  }
  try {
    await mkdir(workspace); await mkdir(env.HOME);
    const config = join(env.XDG_CONFIG_HOME, "rsi");
    const host = join(config, "host-profiles/fixture"); const application = join(config, "application-profiles/web");
    await mkdir(host, { recursive: true }); await mkdir(application, { recursive: true });
    await writeFile(join(config, "settings.json"), JSON.stringify({ "rsi.agent": { default_model: { deployment: "fixture", model: "fixture-model" } } }));
    await writeFile(join(host, "host.profile.toml"), `format = 1\n[[steps]]\nkind = "plugin"\nid = "provider"\nplugin = "rsi.ai.provider.openai-compatible"\n[steps.config]\ndeployment = "fixture"\nendpoint = "${provider.origin}"\npath = "/v1/chat/completions"\nallow_image_input = true\ncredential = { owner = "rsi.ai.provider.openai-compatible", slot = "default" }\n[steps.config.language_models.fixture-model]\ncontext_window_tokens = 128000\ndefault_output_reserve_tokens = 4096\nmax_output_reserve_tokens = 16384\n`);
    await writeFile(join(application, "application.profile.toml"), `format = 1\n[[steps]]\nkind = "plugin"\nid = "service"\nplugin = "rsi.application.service"\nconfig = { host_profile = "fixture" }\n[[steps]]\nkind = "plugin"\nid = "assets"\nplugin = "rsi.web.assets"\nconfig = { directory = ${JSON.stringify(assets)} }\n[[steps]]\nkind = "plugin"\nid = "http"\nplugin = "rsi.application.serve-web"\n`);
    await configure?.({ config, workspace, run });
    const certificate = join(temporary, "certificate.pem"); const key = join(temporary, "key.pem");
    boundedRun("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost", "-keyout", key, "-out", certificate]);
    const reservation = net.createServer(); reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
    const address = `127.0.0.1:${reservation.address().port}`; const origin = `https://${address}`;
    await new Promise(resolve => reservation.close(resolve));
    child = spawn(binary, ["--profile", "web", "--bind", address, "--origin", origin, "--tls-certificate", certificate, "--tls-key", key], { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
    stopped = once(child, "exit");
    child.stderr.on("data", chunk => { stderr += chunk.toString(); if (stderr.length > 1024 * 1024) { stderr = stderr.slice(0, 1024 * 1024); child.kill("SIGTERM"); } });
    const lines = createInterface({ input: child.stdout });
    const [ready] = await deadline(Promise.race([once(lines, "line"), stopped.then(() => { throw new Error(`Service exited before readiness: ${stderr}`); })]), "service readiness");
    assert.equal(JSON.parse(ready).event, "serving");
    return { origin, workspace, run, provider, close,
      register(label) { return JSON.parse(run(["--profile", "devices", "register", label]).stdout); },
    };
  } catch (error) { await close(); throw error; }
}
