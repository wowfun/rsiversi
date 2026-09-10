import init, { Renderer, live_renderers } from "./rsi_web_renderer_fixture.js";
let initialized;
export async function mount(root, snapshot, host, signal) {
  initialized ??= init();
  await initialized;
  if (signal.aborted) throw new Error("Rust renderer mounting was cancelled");
  const renderer = new Renderer(root, JSON.stringify(snapshot.model));
  let disposed = false;
  const events = new AbortController();
  root.addEventListener("click", async event => {
    const action = event.target.closest("button[data-fixture-action]")?.dataset.fixtureAction;
    if (!action || signal.aborted) return;
    try {
      if (action === "refresh") await host.invoke("refresh", { value: null, fields: {} });
      else if (action === "raw") {
        const bytes = await host.source("raw", 0, 5);
        if (!disposed && !signal.aborted) root.querySelector("[data-fixture-bytes]").textContent = [...bytes].map(byte => byte.toString(16).padStart(2, "0")).join(" ");
      }
    } catch (error) {
      if (!disposed && !signal.aborted) { const node = document.createElement("p"); node.textContent = error.message; root.append(node); }
    }
  }, { signal: events.signal });
  return {
    async update(snapshot) { if (disposed) throw new Error("Rust renderer retired"); renderer.update(JSON.stringify(snapshot.model)); },
    async dispose() { if (!disposed) { disposed = true; events.abort(); renderer.free(); } },
  };
}
export { live_renderers };
