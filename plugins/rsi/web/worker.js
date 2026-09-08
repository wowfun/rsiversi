import init, { connect, command, next_view, disconnect, resource_snapshot } from "/rsi_web.js";

const initialized = init();
let connected = false;
let acknowledgement;
let pumping;
let calls = 0;
async function views() {
  while (connected) {
    let view;
    try { view = await next_view(); }
    catch (error) { if (connected) throw error; return; }
    if (!connected) return;
    await new Promise(resolve => { acknowledgement = resolve; postMessage({ kind: "view", view }); });
    acknowledgement = undefined;
  }
}
self.onmessage = async ({ data }) => {
  if (data.kind === "ack") { acknowledgement?.(); return; }
  if (data.kind !== "call" || !Number.isSafeInteger(data.id)) return;
  if (calls === 8) { postMessage({ kind: "reply", id: data.id, error: "Browser input is busy" }); return; }
  calls++;
  try {
    await initialized;
    let result;
    if (data.method === "connect") {
      result = await connect(data.payload.receipt, data.payload.devHttp);
      connected = true;
      pumping = views().catch(error => { connected = false; postMessage({ kind: "failed", error: String(error) }); });
    } else if (data.method === "command") {
      await command(data.payload);
    } else if (data.method === "disconnect") {
      connected = false;
      acknowledgement?.();
      result = JSON.parse(await disconnect(data.payload));
      await pumping;
    } else if (data.method === "resources") {
      result = JSON.parse(resource_snapshot());
    } else { throw new Error("Unknown browser input"); }
    postMessage({ kind: "reply", id: data.id, result });
  } catch (error) {
    postMessage({ kind: "reply", id: data.id, error: String(error) });
  } finally { calls--; }
};
