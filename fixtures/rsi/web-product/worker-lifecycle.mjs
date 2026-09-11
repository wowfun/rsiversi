import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

// Real module Worker running the shipped bridge; a gated WASM export fixture
// isolates commit/disconnect ordering without claiming Rust or server coverage.
export async function verifyWorkerLifecycle(browser, root) {
  const worker = await readFile(join(root, "plugins/rsi/web/worker.js"), "utf8");
  const wasm = `
    let rejectCommit;
    const blocked = [];
    export default async function init() {}
    export async function connect() {}
    export async function next_view() { return ["1", "{}", "{}"]; }
    export async function commit_renderer() {
      postMessage({ kind: "fixture_commit" });
      await new Promise((_, reject) => { rejectCommit = reject; });
    }
    export async function command(input) {
      if (input === "block") { postMessage({kind:"fixture_blocked"}); await new Promise(resolve => blocked.push(resolve)); }
      else rejectCommit(new Error("fixture commit failed"));
    }
    export async function disconnect() { blocked.splice(0).forEach(resolve => resolve()); rejectCommit?.(new Error("retired")); return resource_snapshot(); }
    export function resource_snapshot() { return JSON.stringify({pending_timers:0,active_alarms:0,active_requests:0}); }
    export async function ui_source() {}
    export async function restore_session() {}
    export async function prepare_submission() {}
    export async function dispatch_submission() {}
    export async function import_image() {}
    export async function read_image() {}
  `;
  const server = createServer((request, response) => {
    response.setHeader("Content-Type", request.url === "/" ? "text/html" : "text/javascript");
    response.end(request.url === "/worker.js" ? worker : request.url === "/rsi_web.js" ? wasm : "<!doctype html><body>");
  });
  await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
  const page = await browser.newPage();
  try {
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    const results = await page.evaluate(async () => {
      const results = [];
      for (const mode of ["awaiting_ack", "committing", "connected_failure", "saturated"]) {
        const worker = new Worker("/worker.js", { type: "module" });
        const events = [], pending = new Map();
        let id = 0, receiveFrame, receiveCommit, receiveFailure, receiveSaturation;
        let blocked = 0;
        const saturated = new Promise(resolve => { receiveSaturation = resolve; });
        const frame = new Promise(resolve => { receiveFrame = resolve; });
        const commit = new Promise(resolve => { receiveCommit = resolve; });
        const failure = new Promise(resolve => { receiveFailure = resolve; });
        const call = (method, payload) => new Promise((resolve, reject) => {
          const key = ++id; pending.set(key, { resolve, reject });
          worker.postMessage({ kind: "call", id: key, method, payload });
        });
        worker.onmessage = ({ data }) => {
          events.push(data.kind);
          if (data.kind === "view") receiveFrame();
          if (data.kind === "fixture_blocked" && ++blocked === 8) receiveSaturation();
          if (data.kind === "fixture_commit") receiveCommit();
          if (data.kind === "failed") receiveFailure(data.error);
          if (data.kind === "reply") {
            const waiter = pending.get(data.id); pending.delete(data.id);
            if (data.error) waiter?.reject(new Error(data.error)); else waiter?.resolve(data.result);
          }
        };
        let timer;
        try {
          const result = await Promise.race([
            new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`Worker ${mode} did not settle`)), 5000); }),
            (async () => {
              await call("connect", {}); await frame;
              if (mode !== "awaiting_ack" && mode !== "saturated") {
                worker.postMessage({ kind: "ack", frame_id: "1", renderer: { revision: "a".repeat(64), accept: true } });
                await commit;
              }
              if (mode === "connected_failure") {
                await call("command", "fail");
                return { mode, error: await failure };
              }
              if (mode === "saturated") {
                const work = Array.from({length:8}, () => call("command", "block"));
                await saturated;
                let refused = false;
                try { await call("command", "block"); } catch { refused = true; }
                if (!refused) throw new Error("ordinary overflow admitted");
                const resources = await call("disconnect", true);
                await Promise.all(work);
                return { mode, resources, ordinary_completed: work.length, failures: events.filter(event => event === "failed").length };
              }
              const resources = await call("disconnect", true);
              return { mode, failures: events.filter(event => event === "failed").length, resources };
            })(),
          ]);
          results.push(result);
        } finally { clearTimeout(timer); worker.terminate(); }
      }
      return results;
    });
    for (const result of results.filter(result => result.mode !== "connected_failure")) {
      assert.equal(result.failures, 0, result.mode);
      assert.deepEqual(result.resources, { pending_timers: 0, active_alarms: 0, active_requests: 0 });
    }
    assert.equal(results[3].ordinary_completed, 8);
    assert.match(results[2].error, /fixture commit failed/);
    return results;
  } finally {
    await page.close();
    await new Promise(resolve => server.close(resolve));
  }
}
