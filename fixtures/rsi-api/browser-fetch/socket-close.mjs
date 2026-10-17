import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createServer } from "node:http";
import { executeWorker } from "./worker.mjs";

// A real peer that deliberately withholds the closing handshake, then disconnects.
export async function socketCloseProbe(browser, glue, wasm, catalog) {
  const peers = new Set();
  const timers = new Set();
  let closes = 0;
  const server = createServer((request, response) => {
    if (request.url === "/") { response.end("<!doctype html><title>Socket close fixture</title>"); return; }
    const name = request.url.split("/")[4];
    const body = name === "describe" ? { wire_version: 1, endpoint_id: "02".repeat(16), host_epoch: "03".repeat(16) }
      : name === "operations" ? catalog : { kind: "device", device_id: "06".repeat(16) };
    response.writeHead(200, { "x-rsi-wire-version": "1", "x-rsi-endpoint-id": "02".repeat(16),
      "x-rsi-host-epoch": "03".repeat(16), "content-type": "application/json" }).end(JSON.stringify(body));
  });
  server.on("upgrade", (request, socket, head) => {
    assert.equal(request.url, "/api/v1/fault/missing-end/1");
    assert.equal(head.length, 0);
    peers.add(socket);
    socket.on("close", () => peers.delete(socket));
    socket.on("error", () => socket.destroy());
    const accept = createHash("sha1").update(request.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest("base64");
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Protocol: ${request.headers["sec-websocket-protocol"]}\r\n\r\n`);
    const send = text => {
      const body = Buffer.from(text);
      assert(body.length < 65536);
      const header = body.length < 126 ? Buffer.from([0x82, body.length]) : Buffer.from([0x82, 126, body.length >> 8, body.length & 255]);
      socket.write(Buffer.concat([header, body]));
    };
    let pending = Buffer.alloc(0);
    let initialized = false;
    socket.on("data", bytes => {
      pending = Buffer.concat([pending, bytes]);
      assert(pending.length <= 1024, "bounded fault peer input");
      while (pending.length >= 2) {
        const length = pending[1] & 127;
        assert(length < 126 && (pending[1] & 128), "fixture expects short masked client frames");
        if (pending.length < 6 + length) return;
        const opcode = pending[0] & 15;
        const body = Buffer.from(pending.subarray(6, 6 + length));
        for (let i = 0; i < length; i++) body[i] ^= pending[2 + i % 4];
        pending = pending.subarray(6 + length);
        if (opcode === 8) {
          closes++;
          const timer = setTimeout(() => { timers.delete(timer); socket.destroy(); }, 6500);
          timers.add(timer);
        } else if (!initialized) {
          assert.equal(opcode, 2);
          assert.equal(body.toString(), "{}");
          initialized = true;
          send(JSON.stringify({ status: 200, endpoint: "02".repeat(16), epoch: "03".repeat(16), content_type: "text/event-stream", content_length: null }));
        } else {
          assert.deepEqual(body, Buffer.from([1]));
          send(": ready\n\n");
        }
      }
    });
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const context = await browser.newContext();
  try {
    const page = await context.newPage();
    page.on("console", message => console.error(`socket-close: ${message.text()}`));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    const result = await executeWorker(page, glue, wasm, "run_socket_close_probe");
    assert.equal(closes, 1);
    assert.equal(result.cleanup, "bounded-failure");
    assert.equal(result.retained_requests, 1);
    assert.equal(result.active_requests, 0);
    return result;
  } finally {
    await context.close();
    for (const timer of timers) clearTimeout(timer);
    for (const socket of peers) socket.destroy();
    server.closeAllConnections();
    await new Promise(resolve => server.close(resolve));
  }
}
