import init, { Renderer, live_renderers } from "./rsi_web_renderer_fixture.js";
let initialized;
export async function mount(root, snapshot, host, signal) {
  initialized ??= init();
  await initialized;
  if (signal.aborted) throw new Error("Rust renderer mounting was cancelled");
  const renderer = new Renderer(root, JSON.stringify(snapshot.model));
  let disposed = false;
  let busy = Boolean(snapshot.busy), pending = false;
  const controls = [...root.querySelectorAll("button[data-fixture-action]")];
  const updateControls = () => { for (const control of controls) control.disabled = busy || pending; };
  updateControls();
  const events = new AbortController();
  root.addEventListener("click", async event => {
    const control = event.target.closest("button[data-fixture-action]");
    const action = control?.dataset.fixtureAction;
    if (!action || control.disabled || signal.aborted) return;
    pending = true; updateControls();
    try {
      if (action === "refresh") await host.invoke("refresh", { value: null, fields: {} });
      else if (action === "raw") {
        const bytes = await host.source("raw", 0, 5);
        if (!disposed && !signal.aborted) root.querySelector("[data-fixture-bytes]").textContent = [...bytes].map(byte => byte.toString(16).padStart(2, "0")).join(" ");
      }
    } catch (error) {
      if (!disposed && !signal.aborted) { const node = document.createElement("p"); node.textContent = error.message; root.append(node); }
    } finally {
      pending = false; if (!disposed) updateControls();
    }
  }, { signal: events.signal });
  return {
    async update(snapshot) { if (disposed) throw new Error("Rust renderer retired"); renderer.update(JSON.stringify(snapshot.model)); busy = Boolean(snapshot.busy); updateControls(); },
    async dispose() { if (!disposed) { disposed = true; events.abort(); renderer.free(); } },
  };
}
export { live_renderers };
