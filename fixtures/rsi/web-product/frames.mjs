import assert from "node:assert/strict";
import { join } from "node:path";

export async function verifyFrameDom(page, report, browser) {
  const result = await page.evaluate(() => {
    $("workbench").hidden = false;
    const block = (key, text) => ({ key, text, role: "assistant", title: "Assistant", sources: 0, clipped: false });
    const pane = index => ({ generation: "1", session: `frame-session-${index}`, path: "/workspace", draft: "",
      model: { deployment: "test", model: "model" }, transcript: { blocks: [block("a", "First block"), block("b", "Second block")], status: "Ready" }, pending: [], notice: "" });
    const snapshot = { panes: [pane(0), pane(1)], notice: "", catalog: { workspaces: [], sessions: [], models: [] } };
    presentFrame({ kind: "snapshot", frame_id: "1", view: snapshot });
    const first = panes[0].blocks.get("a").node, second = panes[0].blocks.get("b").node;
    const right = panes[1].blocks.get("a").node;
    panes[1].input.focus();
    const patched = presentFrame({ kind: "patch", frame_id: "2", base_frame_id: "1", sections: {}, panes: [{ index: 0, fields: {}, transcript: { fields: {}, upsert: [block("b", "Updated second")], remove: [] } }] });
    const identities = first === panes[0].blocks.get("a").node && second === panes[0].blocks.get("b").node && right === panes[1].blocks.get("a").node;
    const focus = document.activeElement === panes[1].input;
    const stale = presentFrame({ kind: "patch", frame_id: "4", base_frame_id: "3", sections: { notice: "must not appear" }, panes: [] });
    const retained = frameId === "2" && view.notice === "" && panes[0].blocks.get("b").text.textContent === "Updated second";
    const reordered = presentFrame({ kind: "patch", frame_id: "3", base_frame_id: "2", sections: {}, panes: [{ index: 0, fields: {}, transcript: { fields: {}, upsert: [block("c", "New block")], remove: ["a"], order: ["c", "b"] } }] });
    const order = [...panes[0].transcript.querySelectorAll(".message-text")].map(node => node.textContent);
    const replacement = pane(0); replacement.generation = "2";
    presentFrame({ kind: "snapshot", frame_id: "5", view: { ...snapshot, panes: [replacement, pane(1)] } });
    const replaced = second !== panes[0].blocks.get("b").node;
    return { patched, identities, focus, stale, retained, reordered, order, replaced };
  });
  assert.deepEqual(result, { patched: true, identities: true, focus: true, stale: false, retained: true, reordered: true, order: ["New block", "Updated second"], replaced: true });
  await page.screenshot({ path: join(report, `${browser}-incremental-frame-dom.png`) });
}

// Actual Worker/Rust application and authenticated HTTP; the document deliberately
// withholds acknowledgement. This does not infer cleanup from DOM disappearance.
export async function verifyAcknowledgementDeadline(page, service) {
  const receipt = service.register("ack deadline fixture");
  const result = await page.evaluate(async receipt => {
    const worker = new Worker("/worker.js", { type: "module" });
    let sequence = 0, frameCount = 0;
    const pending = new Map();
    let receiveFrame, fail;
    const first = new Promise(resolve => { receiveFrame = resolve; });
    const failed = new Promise(resolve => { fail = resolve; });
    const call = (method, payload) => new Promise((resolve, reject) => {
      const id = ++sequence; pending.set(id, { resolve, reject }); worker.postMessage({ kind: "call", id, method, payload });
    });
    worker.onmessage = ({ data }) => {
      if (data.kind === "view") { frameCount++; receiveFrame(JSON.parse(data.view)); }
      if (data.kind === "failed") fail(data.error);
      if (data.kind === "reply") { const waiter = pending.get(data.id); pending.delete(data.id); if (data.error) waiter?.reject(new Error(data.error)); else waiter?.resolve(data.result); }
    };
    let timer;
    try {
      const deadline = new Promise((_, reject) => { timer = setTimeout(() => reject(new Error("ACK expiry failed to drain Worker")), 40_000); });
      return await Promise.race([deadline, (async () => {
        await call("connect", { receipt: JSON.stringify(receipt), devHttp: false });
        const frame = await first;
        const workspace = frame.view.catalog.workspaces[0].id;
        for (const pane of [0, 1]) await call("command", JSON.stringify({ action: "create", pane, workspace, trust: false }));
        // A stale acknowledgement must not release the pending frame or renew its deadline.
        worker.postMessage({ kind: "ack", frame_id: "18446744073709551615" });
        const error = await failed;
        const resources = await call("resources");
        return { frameCount, error, resources };
      })()]);
    } finally { clearTimeout(timer); worker.terminate(); }
  }, receipt);
  assert.equal(result.frameCount, 1);
  assert.match(result.error, /acknowledgement timed out/);
  assert.deepEqual(result.resources, { pending_timers: 0, active_alarms: 0, active_requests: 0 });
  return result;
}
