import init, { connect, command, ui_source, import_image, read_image, next_view, commit_renderer, disconnect, resource_snapshot } from "/rsi_web.js";

const initialized = init();
let connected = false;
let acknowledgement;
let pumping;
let calls = 0;
async function views() {
  let base;
  while (connected) {
    let frame;
    try { frame = await next_view(base); }
    catch (error) { if (connected) throw error; return; }
    if (!connected) return;
    const [frameId, view, assets] = frame;
    const settled = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { acknowledgement = undefined; reject(new Error("Document acknowledgement timed out")); }, 30_000);
      acknowledgement = { frameId, finish(resync, renderer) { clearTimeout(timer); resolve({ base: resync ? undefined : frameId, renderer }); } };
      postMessage({ kind: "view", view, assets });
    });
    acknowledgement = undefined;
    if (!connected) return;
    if (settled.renderer) {
      try { await commit_renderer(settled.renderer.revision, settled.renderer.accept); }
      catch (error) { if (connected) throw error; return; }
    }
    base = settled.base;
  }
}
self.onmessage = async ({ data }) => {
  if (data.kind === "ack") { if (data.frame_id === acknowledgement?.frameId) acknowledgement.finish(data.resync === true, data.renderer); return; }
  if (data.kind !== "call" || !Number.isSafeInteger(data.id)) return;
  if (calls === 8) { postMessage({ kind: "reply", id: data.id, error: "Browser input is busy" }); return; }
  calls++;
  try {
    await initialized;
    let result;
    if (data.method === "connect") {
      result = await connect(data.payload.receipt, data.payload.devHttp);
      connected = true;
      pumping = views().catch(async error => { connected = false; try { await disconnect(false); } catch {} postMessage({ kind: "failed", error: String(error) }); });
    } else if (data.method === "command") {
      await command(data.payload);
    } else if (data.method === "import_image") {
      await import_image(data.payload.pane, data.payload.generation, new Uint8Array(data.payload.bytes));
    } else if (data.method === "ui_source") {
      result = await ui_source(data.payload);
    } else if (data.method === "read_image") {
      result = await read_image(data.payload);
    } else if (data.method === "disconnect") {
      connected = false;
      acknowledgement?.finish(true);
      result = JSON.parse(await disconnect(data.payload));
      await pumping;
    } else if (data.method === "resources") {
      result = JSON.parse(resource_snapshot());
    } else { throw new Error("Unknown browser input"); }
    postMessage({ kind: "reply", id: data.id, result }, result instanceof Uint8Array ? [result.buffer] : []);
  } catch (error) {
    postMessage({ kind: "reply", id: data.id, error: String(error) });
  } finally { calls--; }
};
