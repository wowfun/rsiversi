import {readFactEvidence} from './fact-evidence.mjs';
import { cleanupAll } from './cleanup.mjs';
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import http from "node:http";
import net from "node:net";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { mkdtemp, mkdir, readFile, writeFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { setTimeout as pause } from "node:timers/promises";

// Playwright's waitForFunction polls synchronously; do not pass async predicates.
export async function waitUntil(probe, label, ms = 30_000) {
  const expires = Date.now() + ms;
  while (Date.now() < expires) { if (await probe()) return; await pause(25); }
  throw new Error(`${label} timeout`);
}

export function boundedRun(binary, args, options = {}) {
  const result = spawnSync(binary, args, { timeout: 300_000, maxBuffer: 2 * 1024 * 1024, ...options });
  assert.equal(result.status, 0, result.error?.message ??
    `Process exited with status ${result.status}: ${(result.stderr?.toString() || result.stdout?.toString() || 'no diagnostic').slice(-4096)}`);
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
  const held = new Map();
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
      requests.push({ prompt, completedTool, model: body.model, reasoning_effort: body.reasoning_effort ?? null, images });
      onRequest?.(body);
      response.writeHead(200, { "content-type": "text/event-stream" });
      if (prompt.includes("SETTLEMENT_OUTPUT_LIMIT") || prompt.includes("UI Goal hold") || (prompt.includes("observe background job") && completedTool)) {
        const truncated = prompt.includes("SETTLEMENT_OUTPUT_LIMIT");
        response.write(`data: ${JSON.stringify({ choices: [{ delta: { role: "assistant", content: truncated ? "流式预览 🦀 · first boundary\n" : "Waiting for fixture release." }, finish_reason: null }] })}\n\n`);
        const id = requests.length;
        const finish = () => response.end(sse({ content: truncated ? "保留的部分输出 · final boundary" : "Fixture released the model stream." }, truncated ? "length" : "stop"));
        held.set(id, { prompt, finish });
        const timer = setTimeout(finish, truncated ? 180_000 : 60_000);
        response.on("close", () => { clearTimeout(timer); held.delete(id); });
        return;
      }
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
      if (messages.some(message => message.role === "system" && text(message.content).includes("MARKDOWN-REVIEWER-V"))) {
        response.end(sse({content:"Custom reviewer completed its isolated task."})); return;
      }
      let name; let argumentsValue;
      // Forked child prompts retain the parent marker; match their exact task first.
      if (!completedTool && prompt.includes("WORKFLOW_CHILD_")) {
        name="report_result"; argumentsValue={n:prompt.includes("LEFT")?19:23};
      } else if (!completedTool && prompt.includes("WORKFLOW_FIXTURE_")) {
        name="run_workflow";
        const task = side => ({message:`WORKFLOW_CHILD_${side}`,output_schema:{type:'object',properties:{n:{type:'integer'}},required:['n'],additionalProperties:false}});
        const script=prompt.includes("CANCEL") ? "await workflow.phase('Await cancellation',{pid:process.pid}); await new Promise(()=>{}); return {};"
          : `await workflow.phase('Collect evidence',{jobs:2}); const rows=await workflow.parallel([()=>workflow.agent(${JSON.stringify(task('LEFT'))}),()=>workflow.agent(${JSON.stringify(task('RIGHT'))})]); return await workflow.pipeline([async rows=>({total:rows.reduce((sum,row)=>sum+row.value.n,0)})],rows);`;
        argumentsValue={script,background:true};
      } else if (!completedTool && prompt.includes("delegate to Markdown reviewer")) {
        name="spawn_agent"; argumentsValue={role:"reviewer",task_name:`reviewer-${requests.length}`,message:"Review the isolated sample and return a concise finding."};
      } else if (!completedTool && prompt.includes("start an external delegation")) {
        name = "external_agent"; argumentsValue = { operation: "start", endpoint: "sdk-agent" };
      } else if (!completedTool && prompt.includes("record a typed addon result")) {
        name = "fixture_echo"; argumentsValue = { message: "中文 · typed result <script>literal</script>" };
      } else if (!completedTool && prompt.includes("record an inline patch")) {
        name = "apply_patch"; argumentsValue = { patch: "*** Begin Patch\n*** Update File: card.txt\n@@\n-before\n+after · 界\n*** End Patch\n" };
      } else if (!completedTool && prompt.includes("observe background job")) {
        name = "bash"; argumentsValue = { command: "printf 'job-ready\\n'; while [ ! -f job-release ]; do sleep 0.05; done; printf 'job-done\\n'", run_in_background: true };
      } else if (!completedTool && prompt.includes("inspect a child task")) {
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
    release(marker) {
      const matches = [...held.values()].filter(value => value.prompt.includes(marker));
      assert(matches.length > 0, `no held provider response for ${marker}`);
      for (const value of matches) value.finish();
    },
    async close() { const closed = new Promise(resolve => server.close(resolve)); for (const socket of sockets) socket.destroy(); await closed; } };
}

export async function startService({ binary, assets, report, configure, onRequest, deepseekKey, watchAssets = false, developmentHttp = false }) {
  const temporary = await mkdtemp(join(tmpdir(), "rsi-web-"));
  const provider = await startProvider(onRequest);
  let child; let stopped; let hostStarted = false;
  let stderr = "";
  const workspace = join(temporary, "workspace");
  const inherited = Object.fromEntries(["PATH", "LANG", "RUSTUP_HOME", "CARGO_HOME"].filter(key => process.env[key]).map(key => [key, process.env[key]]));
  const env = { ...inherited, HOME: join(temporary, "home"), XDG_CONFIG_HOME: join(temporary, "config"), XDG_STATE_HOME: join(temporary, "state"), XDG_CACHE_HOME: join(temporary, "cache"), XDG_RUNTIME_DIR: join(temporary, "runtime"), DBUS_SESSION_BUS_ADDRESS: `unix:path=${temporary}/absent-session-bus`, RSI_OPENAI_COMPATIBLE_API_KEY: "isolated-fixture-secret" };
  const run = (args, {input} = {}) => boundedRun(binary, args, { cwd: workspace, env, encoding: "utf8", timeout: 30_000, input });
  // Only the explicitly invoked live fixture supplies this value. Default tests
  // remain keyless even when the developer's process has a real provider key.
  if (deepseekKey !== undefined) {
    assert.equal(typeof deepseekKey, "string"); assert(deepseekKey.length > 0 && deepseekKey.length <= 64 * 1024);
    env.DEEPSEEK_API_KEY = deepseekKey;
  }
  async function stopProcess() {
    let failure;
    try { if (child && child.exitCode === null) {
      child.kill("SIGTERM");
      let result;
      try { result = await deadline(stopped, "service stop", 20_000); }
      catch { child.kill("SIGKILL"); await stopped; throw new Error("Service exceeded clean shutdown deadline"); }
      const [code, signal] = result;
      const forcedWindows = process.platform === 'win32' && signal === 'SIGTERM';
      await writeFile(join(report, 'service-process-shutdown.json'), JSON.stringify({platform:process.platform,code,signal,mode:forcedWindows?'windows-terminate-process':'graceful'}));
      if (!forcedWindows && (code !== 0 || signal !== null)) throw new Error(`Service cleanup failed: exit ${code}, signal ${signal}`);
    } } catch (error) { failure = error; }
    if (hostStarted) {
      const stopHost = force => boundedRun(binary, ["host", "stop", ...(force ? ["--force"] : [])],
        {cwd:workspace,env,encoding:"utf8",timeout:45_000});
      try {
        if (run(["host", "status"]).stdout.trim() !== "stopped") stopHost(false);
        hostStarted = false;
      }
      catch (error) {
        failure = failure ? new AggregateError([failure,error], "Fixture and Host cleanup failed") : error;
        try { stopHost(true); hostStarted = false; } catch { /* Retain its home authorities below. */ }
      }
    }
    if (failure) throw failure;
  }
  async function close() {
    let failed;
    try { await stopProcess(); } catch (error) { failed = error; }
    await cleanupAll(
      () => { if (failed) throw failed; },
      () => writeFile(join(report, "service.stderr.log"), stderr),
      async () => {
        const log = join(env.XDG_STATE_HOME, 'rsi/session-host/owner.log');
        const info = await stat(log).catch(error => { if (error.code !== 'ENOENT') throw error; });
        if (info && info.size <= 1024 * 1024) await writeFile(join(report, 'host.stderr.log'), await readFile(log));
      },
      () => provider.close(),
      () => hostStarted
        ? writeFile(join(report, "retained-host-home.txt"), `${temporary}\n`)
        : rm(temporary, { recursive: true, force: true }),
    );
  }
  try {
    await mkdir(workspace); await mkdir(env.HOME);
    const config = join(env.XDG_CONFIG_HOME, "rsi");
    await mkdir(config,{recursive:true});
    if(process.platform==='win32') boundedRun('powershell.exe',['-NoProfile','-NonInteractive','-Command',`
      $ErrorActionPreference='Stop'
      $directory=$env:RSI_FIXTURE_DIRECTORY
      $identity=[System.Security.Principal.WindowsIdentity]::GetCurrent().User
      $acl=[System.Security.AccessControl.DirectorySecurity]::new()
      $userSid=$identity.Value
      $acl.SetSecurityDescriptorSddlForm(('O:{0}D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{0})' -f $userSid),[System.Security.AccessControl.AccessControlSections]'Owner,Access')
      [System.IO.Directory]::SetAccessControl($directory,$acl)
    `],{env:{...process.env,RSI_FIXTURE_DIRECTORY:config}});
    const host = join(config, "host-profiles/fixture"); const application = join(config, "application-profiles/browser-fixture");
    await mkdir(host, { recursive: true }); await mkdir(application, { recursive: true });
    await writeFile(join(config, "rsi.toml"), "format = 1\n[service]\nkind = 'catalog'\nid = 'fixture'\n");
    await writeFile(join(config, "settings.json"), JSON.stringify({ "rsi.agent": { default_model: { deployment: "fixture", model: "fixture-model" } } }));
    await writeFile(join(host, "host.profile.toml"), `format = 1\n[[steps]]\nkind = "plugin"\nid = "provider"\nplugin = "rsi.ai.provider.openai-compatible"\n[steps.config]\ndeployment = "fixture"\nendpoint = "${provider.origin}"\npath = "/v1/chat/completions"\nallow_image_input = true\ncredential = { owner = "rsi.ai.provider.openai-compatible", slot = "default" }\n[steps.config.language_models.fixture-model]\ncontext_window_tokens = 128000\ndefault_output_reserve_tokens = 4096\nmax_output_reserve_tokens = 16384\n[steps.config.reasoning_efforts.fixture-model]\nsupported = ["low", "high"]\ndefault = "low"\n`);
    await writeFile(join(application, "application.profile.toml"), `format = 1\n[[steps]]\nkind = "plugin"\nid = "service"\nplugin = "rsi.application.service"\nconfig = { host_profile = "fixture" }\n[[steps]]\nkind = "plugin"\nid = "http"\nplugin = "rsi.application.serve"\n`);
    await configure?.({ config, workspace, run, provider });
    const certificate = join(temporary, "certificate.pem"); const key = join(temporary, "key.pem");
    if (!developmentHttp) boundedRun("openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1,DNS:localhost", "-keyout", key, "-out", certificate]);
    const reservation = net.createServer(); reservation.listen(0, "127.0.0.1"); await once(reservation, "listening");
    const address = `127.0.0.1:${reservation.address().port}`;
    let origin = `${developmentHttp?'http':'https'}://${address}`, launchUrl;
    await new Promise(resolve => reservation.close(resolve));
    if (watchAssets) {
      const directory = dirname(assets), output = join(directory, "web-package");
      const inputs = ['standard.js','lazy.js','ui-renderers.json'].map(name => `${basename(assets)}/${name}`);
      await writeFile(join(config, "rsi.toml"), `${await readFile(join(config, "rsi.toml"), "utf8")}\n[sources.web]\ndirectory = ${JSON.stringify(directory)}\ncommand = ${JSON.stringify([process.execPath, join(import.meta.dirname, "source-package.mjs"), assets, output])}\ninputs = ${JSON.stringify(inputs)}\noutput = "web-package/package.json"\n`);
    }
    const startProcess = async () => {
    hostStarted = true;
    const argumentsValue = watchAssets
      ? ["--watch", "web", "--port", "0", "--no-open"]
      : ["run", "browser-fixture", "--assets", assets, "--bind", address, "--origin", origin, ...(developmentHttp ? ['--dev-http'] : ["--tls-certificate", certificate, "--tls-key", key])];
    child = spawn(binary, argumentsValue, { cwd: workspace, env, stdio: ["ignore", "pipe", "pipe"] });
    stopped = once(child, "exit");
    child.stderr.on("data", chunk => { stderr += chunk.toString(); if (stderr.length > 1024 * 1024) { stderr = stderr.slice(0, 1024 * 1024); child.kill("SIGTERM"); } });
    const lines = createInterface({ input: child.stdout });
    const [ready] = await deadline(Promise.race([once(lines, "line"), stopped.then(() => { throw new Error(`Service exited before readiness: ${stderr}`); })]), "service readiness", 210_000);
    if (watchAssets) {
      assert(ready.startsWith("rsi web: "));
      launchUrl = ready.slice("rsi web: ".length);
      origin = new URL(launchUrl).origin;
    } else assert.equal(JSON.parse(ready).event, "serving");
    };
    await startProcess();
    return { origin, launchUrl, developmentHttp: new URL(origin).protocol === 'http:', workspace, run, provider, close, get pid() { return child.pid; },
      async readSessionFacts(session) {
        assert(typeof session==='string'&&session.length>0&&session.length<=128);
        const {DatabaseSync}=await import('node:sqlite');
        const database=new DatabaseSync(join(env.XDG_STATE_HOME,'rsi/agent/sessions.sqlite3'),{readOnly:true});
        try {
          const {facts,selection}=readFactEvidence(database,session);
          await writeFile(join(report,'fact-evidence-source.json'),JSON.stringify(selection));
          return facts;
        } finally {database.close();}
      },
      probe(program, input) { return boundedRun(program, [], {cwd: workspace, env, encoding: 'utf8', input: JSON.stringify(input)}); },
      async restart(change) { await stopProcess(); await change?.(); await startProcess(); },
      register(label) {
        const receipt = JSON.parse(run(["run", "devices", "register", label]).stdout);
        const grants = JSON.parse(run(["run", "devices", "configuration", "list"]).stdout);
        run(["run", "devices", "configuration", "grant", receipt.id, grants.revision]);
        return receipt;
      },
    };
  } catch (error) {
    try { await close(); }
    catch (cleanup) { throw new AggregateError([error, cleanup], "Service startup and cleanup failed", { cause: error }); }
    throw error;
  }
}
