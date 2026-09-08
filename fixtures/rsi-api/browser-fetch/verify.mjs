import assert from "node:assert/strict";
import { dirname, join } from "node:path";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { chromium, firefox } from "playwright";
import { buildWorkerProbe } from "../../tools/browser-worker.mjs";
import { executeWorker } from "./worker.mjs";
import { malformedProbe } from "./malformed.mjs";

const fixture = dirname(fileURLToPath(import.meta.url));
const stem = "rsi_api_browser_fetch";
const generated = buildWorkerProbe({ fixture, stem });
const built = spawnSync("cargo", ["build", "--bin", "rsi-api-browser-fetch", "--locked"], {
  cwd: fixture, stdio: "inherit", timeout: 300_000,
});
assert.equal(built.status, 0, built.error?.message);
const catalogResult = spawnSync(join(fixture, "target/debug/rsi-api-browser-fetch"), ["--malformed-catalog"], { encoding: "utf8", timeout: 10_000 });
assert.equal(catalogResult.status, 0, catalogResult.stderr);
assert(catalogResult.stdout.length <= 32 * 1024);
const malformedCatalog = JSON.parse(catalogResult.stdout);
const glue = await readFile(join(generated, `${stem}.js`), "utf8");
const wasm = (await readFile(join(generated, `${stem}_bg.wasm`))).toString("base64");
assert(glue.length <= 2 * 1024 * 1024 && wasm.length <= 64 * 1024 * 1024);
const secure = process.env.RSI_FETCH_TLS === "1";
const server = spawn(join(fixture, "target/debug/rsi-api-browser-fetch"), secure ? ["--tls"] : [], {
  cwd: fixture, stdio: ["pipe", "pipe", "inherit"],
});
const exited = once(server, "exit");
const lines = createInterface({ input: server.stdout });
const [ready] = await Promise.race([once(lines, "line"), exited.then(() => { throw new Error("server exited before ready"); }),
  new Promise((_, reject) => { const timer = setTimeout(() => reject(new Error("server readiness timeout")), 15_000); timer.unref(); })]);
const { origin } = JSON.parse(ready);
try {
  for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
    const browser = await engine.launch({ headless: true });
    try {
      if (!secure) {
        const malformed = await malformedProbe(browser, glue, wasm, malformedCatalog);
        console.log(JSON.stringify({ browser: name, version: browser.version(), malformed }));
        const lifecycle = await browser.newContext();
        try {
          const page = await lifecycle.newPage();
          page.on("console", message => console.error(`bootstrap: ${message.text()}`));
          await page.goto(origin);
          const result = await executeWorker(page, glue, wasm, "run_bootstrap_cancel_probe");
          assert.equal(result.cancelled_bootstrap, "passed");
          console.log(JSON.stringify({ browser: name, controlled_platform_promise: result }));
        } finally { await lifecycle.close(); }
      }
      for (let worker = 0; worker < 2; worker++) {
        const context = await browser.newContext({ ignoreHTTPSErrors: secure });
        try {
          const page = await context.newPage();
          page.setDefaultTimeout(90_000);
          page.on("console", (message) => console.error(`${name}: ${message.text()}`));
          await page.goto(`${origin}/`);
          const auth = await page.evaluate(async () => {
            const login = await fetch("/api/v1/login", { method: "POST", headers: {
              authorization: `Bearer ${"a".repeat(64)}`, "x-rsi-csrf": "1", "x-rsi-wire-version": "1",
            } });
            const missingCsrf = await fetch("/api/v1/connection/describe/1", { method: "POST",
              headers: { "content-type": "application/json", "x-rsi-wire-version": "1" }, body: '{"wire_version":1}' });
            return { login: login.status, missingCsrf: missingCsrf.status, cookie: document.cookie };
          });
          assert.deepEqual(auth, { login: 200, missingCsrf: 401, cookie: "" });
          const cookies = await context.cookies();
          assert.equal(cookies.length, 1);
          assert.equal(cookies[0].httpOnly, true);
          assert.equal(cookies[0].sameSite, "Strict");
          assert.equal(cookies[0].path, "/api");
          assert.equal(cookies[0].secure, secure);
          for (const forgedOrigin of [undefined, "https://foreign.invalid"]) {
            const headers = { "x-rsi-wire-version": "1", "x-rsi-csrf": "1", "content-type": "application/json" };
            if (forgedOrigin) headers.origin = forgedOrigin;
            const rejected = await context.request.post(`${origin}/api/v1/connection/describe/1`, {
              headers, data: '{"wire_version":1}',
            });
            assert.equal(rejected.status(), 401, "cookie authority requires exact Origin");
          }
          // No routing interception, including bootstrap: it changes Firefox abort behavior.
          const result = await executeWorker(page, glue, wasm);
          assert.equal(result.status, "passed");
          assert.equal(result.cases.length, 10);
          assert.equal(result.active_requests, 0);
          assert.equal(result.pending_timers, 0);
          assert.equal(result.active_alarms, 0);
          assert.equal((await context.cookies()).length, 0, "logout removed the device cookie");
          console.log(JSON.stringify({ browser: name, version: browser.version(), worker, ...result }));
          if (secure) {
            const pool = await executeWorker(page, glue, wasm, "run_pool_probe");
            console.log(JSON.stringify({ browser: name, version: browser.version(), worker, tls_pool: pool }));
            assert.equal(pool.control_completed, true, "idle subscriptions preserve HTTP/2 control progress");
          }
        } finally { await context.close(); }
      }
      if (secure) {
        const context = await browser.newContext({ ignoreHTTPSErrors: true });
        try {
          const page = await context.newPage();
          page.on("console", message => console.error(`${name}: ${message.text()}`));
          await page.goto(origin);
          const status = await page.evaluate(async () => (await fetch("/api/v1/login", { method: "POST",
            headers: { authorization: `Bearer ${"a".repeat(64)}`, "x-rsi-csrf": "1", "x-rsi-wire-version": "1" } })).status);
          assert.equal(status, 200);
          const results = await Promise.all([
            executeWorker(page, glue, wasm, "run_shared_pool_probe"),
            executeWorker(page, glue, wasm, "run_shared_pool_probe"),
          ]);
          assert(results.every(result => result.shared_subscriptions === 8 && result.active_requests === 0));
          console.log(JSON.stringify({ browser: name, version: browser.version(), concurrent_workers: 2, results }));
        } finally { await context.close(); }
      }
    } finally { await browser.close(); }
  }
} finally {
  server.stdin.end("q");
  const result = await Promise.race([exited, new Promise((resolve) => {
    const timer = setTimeout(() => { server.kill("SIGKILL"); resolve([null, "cleanup timeout"]); }, 10_000); timer.unref();
  })]);
  lines.close();
  assert.deepEqual(result, [0, null], "native server acknowledged clean shutdown");
}
