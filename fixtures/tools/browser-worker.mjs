import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

export function buildWorkerProbe({ fixture, stem }) {
  const generated = join(fixture, "target/browser-pkg");
  function run(command, args) {
    const result = spawnSync(command, args, { cwd: fixture, stdio: "inherit", timeout: 300_000 });
    if (result.error) throw result.error;
    assert.equal(result.status, 0, `${command} failed`);
  }

  const bindgen = process.env.RSI_WASM_BINDGEN || "wasm-bindgen";
  const version = spawnSync(bindgen, ["--version"], { encoding: "utf8", timeout: 10_000 });
  assert.equal(version.status, 0, version.error?.message || version.stderr);
  assert.equal(version.stdout.trim(), "wasm-bindgen 0.2.127", "CLI must match the locked Rust schema");
  const tree = spawnSync("cargo", ["tree", "--locked", "--target", "wasm32-unknown-unknown",
    "--edges", "normal", "--prefix", "none", "--format", "{p}"], {
    cwd: fixture, encoding: "utf8", timeout: 60_000,
  });
  assert.equal(tree.status, 0, tree.stderr);
  const dependencies = new Set(tree.stdout.split("\n").map((line) => line.split(" ")[0]));
  for (const forbidden of ["rsi-agent-kernel", "rsi-agent-store-sqlite", "rsi-storage-sqlite",
    "rsi-workspace", "rsi-service-host", "rsi-api-uds-client", "rsi-meta-native", "rsi-meta-native-loader",
    "crossterm", "ratatui", "mio", "socket2"]) {
    assert(!dependencies.has(forbidden), `browser closure includes ${forbidden}`);
  }
  run("cargo", ["build", "--lib", "--locked", "--target", "wasm32-unknown-unknown"]);
  run(bindgen, ["--target", "web", "--out-dir", generated,
    `target/wasm32-unknown-unknown/debug/${stem}.wasm`]);
  return generated;
}

export async function runWorkerProbe({ fixture, stem, engines, cases, trap = false }) {
  const generated = buildWorkerProbe({ fixture, stem });

  const routes = new Map([
    ["/", [join(fixture, "probe.html"), "text/html"]],
    ["/worker.mjs", [join(fixture, "worker.mjs"), "text/javascript"]],
    [`/pkg/${stem}.js`, [join(generated, `${stem}.js`), "text/javascript"]],
    [`/pkg/${stem}_bg.wasm`, [join(generated, `${stem}_bg.wasm`), "application/wasm"]],
  ]);
  const server = createServer(async (request, response) => {
    const route = routes.get(request.url);
    if (!route) { response.writeHead(404).end(); return; }
    try {
      const bytes = await readFile(route[0]);
      response.writeHead(200, { "content-type": route[1], "cache-control": "no-store" }).end(bytes);
    } catch {
      response.writeHead(500).end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  try {
    for (const [name, engine] of engines) {
      const browser = await engine.launch({ headless: true });
      try {
        const page = await browser.newPage();
        page.on("console", (message) => console.error(`${name}: ${message.text()}`));
        await page.goto(`http://127.0.0.1:${server.address().port}/`);
        const results = await page.evaluate((trap) => Promise.all([
          window.runProbe("lifecycle"), window.runProbe("lifecycle"),
          ...(trap ? [window.runProbe("trap")] : []),
        ]), trap);
        for (const result of results.slice(0, 2)) {
          assert.equal(result.status, "passed");
          assert.equal(result.pending_timers, 0);
          assert.equal(result.active_alarms, 0);
          assert.equal(result.cases.length, cases);
        }
        if (trap) assert.deepEqual(results[2], { state: "failed", cleanup_acknowledged: false });
        console.log(JSON.stringify({ browser: name, version: browser.version(), workers: 2,
          ...results[0], ...(trap ? { trap: results[2] } : {}) }));
      } finally { await browser.close(); }
    }
  } finally { await new Promise((resolve) => server.close(resolve)); }

}
