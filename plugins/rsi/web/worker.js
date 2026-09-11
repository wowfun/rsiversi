import init, { connect, command, restore_session, prepare_submission, dispatch_submission, ui_source, import_image, read_image, next_view, commit_renderer, disconnect, resource_snapshot } from "/rsi_web.js";

const initialized = init();
let connected = false;
let acknowledgement;
let pumping;
let calls = 0;
let lifecycle = false;
let accepting = false;
const ordinary = new Set();
let draining;
let drainSignsOut = false;
async function shutdown(signOut) {
  connected = false; accepting = false;
  acknowledgement?.finish(true);
  if (!draining) {
    drainSignsOut = signOut;
    draining = disconnect(signOut);
  }
  const resources = JSON.parse(await draining);
  await Promise.allSettled([...ordinary]);
  if (signOut && !drainSignsOut) throw new Error("Connection failure already started cleanup; sign-out is not confirmed");
  return resources;
}
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
async function dispatch(data) {
  await initialized;
  let result;
  if (data.method === "connect") {
    result = await connect(data.payload.receipt, data.payload.devHttp);
    connected = true; accepting = true;
    pumping = views().catch(async error => {
      if (!connected) return;
      try { await shutdown(false); }
      catch (cleanup) { error = new Error(`${error}; ${cleanup}`); }
      postMessage({ kind: "failed", error: String(error) });
    });
  } else if (data.method === "command") {
    result = await command(data.payload);
  } else if (data.method === "restore_session") {
    result = await restore_session(data.payload);
  } else if (data.method === "prepare_submission") {
    result = await prepare_submission(data.payload);
  } else if (data.method === "dispatch_submission") {
    result = await dispatch_submission(data.payload.pane, data.payload.generation, data.payload.opaque, data.payload.mode);
  } else if (data.method === "import_image") {
    result = await import_image(data.payload.pane, data.payload.generation, new Uint8Array(data.payload.bytes));
  } else if (data.method === "ui_source") {
    result = await ui_source(data.payload);
  } else if (data.method === "read_image") {
    result = await read_image(data.payload);
  } else if (data.method === "disconnect") {
    result = await shutdown(data.payload === true);
    await pumping;
  } else if (data.method === "resources") {
    result = JSON.parse(resource_snapshot());
  } else { throw new Error("Unknown browser input"); }
  return result;
}
self.onmessage = ({ data }) => {
  if (data.kind === "ack") { if (data.frame_id === acknowledgement?.frameId) acknowledgement.finish(data.resync === true, data.renderer); return; }
  if (data.kind !== "call" || !Number.isSafeInteger(data.id)) return;
  const control = data.method === "connect" || data.method === "disconnect";
  if (control ? lifecycle : (calls >= 8 || (!accepting && data.method !== "resources"))) {
    postMessage({ kind: "reply", id: data.id, error: "Browser input is busy or closed", notAdmitted: true }); return;
  }
  if (control) lifecycle = true; else calls++;
  if (data.method === "disconnect") { connected = false; accepting = false; acknowledgement?.finish(true); }
  const task = (async () => {
    try {
      const result = await dispatch(data);
      postMessage({ kind: "reply", id: data.id, result }, result instanceof Uint8Array ? [result.buffer] : []);
    } catch (error) {
      postMessage({ kind: "reply", id: data.id, error: String(error) });
    } finally { if (control) lifecycle = false; else calls--; }
  })();
  if (!control) {
    ordinary.add(task);
    task.then(() => ordinary.delete(task), () => ordinary.delete(task));
  }
};
