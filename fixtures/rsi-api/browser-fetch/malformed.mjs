import assert from "node:assert/strict";
import { createServer } from "node:http";
import { gzipSync } from "node:zlib";
import { executeWorker } from "./worker.mjs";

export async function malformedProbe(browser, glue, wasm, catalog) {
  const calls = new Map();
  let redirects = 0;
  const server = createServer((request, response) => {
    if (request.url === "/forbidden-replay") { redirects++; response.writeHead(500).end(); return; }
    if (request.url === "/") { response.end("<!doctype html><title>Malformed Fetch fixture</title>"); return; }
    if (request.method !== "POST" || !request.url.startsWith("/api/v1/")) { response.writeHead(404).end(); return; }
    let bytes = 0;
    request.on("data", chunk => { bytes += chunk.length; if (bytes > 1024) request.destroy(); });
    request.on("end", () => {
      const name = request.url.split("/")[4];
      calls.set(request.url, (calls.get(request.url) || 0) + 1);
      assert(calls.get(request.url) <= 32, "unbounded transport retransmission");
      if (request.url === "/api/v1/login") { response.destroy(); return; }
      let status = 200;
      const headers = { "x-rsi-wire-version": "1", "x-rsi-endpoint-id": "02".repeat(16),
        "x-rsi-host-epoch": "03".repeat(16), "content-type": "application/json", "connection": "close" };
      let body = Buffer.from("true");
      if (name === "describe") body = Buffer.from(JSON.stringify({ wire_version: 1, endpoint_id: "02".repeat(16), host_epoch: "03".repeat(16) }));
      else if (name === "operations") body = Buffer.from(JSON.stringify(catalog));
      else if (name.startsWith("read-") || name.startsWith("mutate-")) {
        switch (name.substring(name.indexOf("-") + 1)) {
          case "json": body = Buffer.from("{broken"); break;
          case "truncated": headers["content-length"] = "20"; break;
          case "epoch": headers["x-rsi-host-epoch"] = "04".repeat(16); break;
          case "endpoint": headers["x-rsi-endpoint-id"] = "05".repeat(16); break;
          case "type": headers["content-type"] = "text/plain"; break;
          case "encoding": headers["content-encoding"] = "gzip"; body = gzipSync(body); break;
          case "length": headers["content-length"] = "129"; break;
          case "redirect": status = 307; headers.location = "/forbidden-replay"; break;
          case "binary": headers["content-type"] = "application/vnd.rsi.binary"; body = Buffer.alloc(16, 255); break;
          case "header": headers["content-type"] = "x".repeat(1025); break;
          case "head-loss": response.destroy(); return;
          default: throw new Error(`unexpected fault ${name}`);
        }
      } else {
        headers["content-type"] = "text/event-stream";
        const frames = {
          "missing-end": "event: item\ndata: 1\n\n",
          trailing: "event: end\ndata: {}\n\nx",
          opening: ": ready\n\n: ready\n\nevent: end\ndata: {}\n\n",
          "domain-without-end": "event: domain-error\ndata: {}\n\n",
          "invalid-item": "event: item\ndata: {broken\n\nevent: end\ndata: {}\n\n",
        };
        assert(Object.hasOwn(frames, name));
        body = Buffer.from(frames[name]);
      }
      if (!headers["content-length"]) headers["content-length"] = String(body.length);
      response.writeHead(status, headers).end(body);
    });
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.on("console", message => console.error(`malformed: ${message.text()}`));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    const result = await executeWorker(page, glue, wasm, "run_malformed_probe");
    assert.equal(result.malformed_cases, 27);
    assert.equal(result.fetch_calls.length, 30);
    const retransmissions = [];
    for (const [path, count] of result.fetch_calls) {
      assert.equal(count, 1, `RSI replayed Fetch ${path}`);
      assert(calls.has(path), `request never arrived ${path}`);
      const delivered = calls.get(path);
      if (path === "/api/v1/login" || path.includes("head-loss")) {
        retransmissions.push({ path, fetch_calls: count, http_deliveries: delivered });
      } else assert.equal(delivered, 1, `unexpected HTTP replay ${path}`);
    }
    assert.equal(calls.size, 30);
    assert.equal(redirects, 0, "browser followed a forbidden redirect");
    delete result.fetch_calls;
    return { ...result, retransmissions };
  } finally {
    await context.close(); server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
}
