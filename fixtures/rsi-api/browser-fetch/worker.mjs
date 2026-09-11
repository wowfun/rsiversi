import assert from "node:assert/strict";

export async function executeWorker(page, glue, wasm, entry = "run_probe") {
  assert(["run_probe", "run_pool_probe", "run_shared_pool_probe", "run_malformed_probe", "run_caller_fault_probe", "run_bootstrap_cancel_probe"].includes(entry));
  return page.evaluate(({ glue, wasm, entry }) => new Promise((resolve, reject) => {
    const source = glue + `\nconst fetchCalls = new Map();
    const nativeFetch = self.fetch.bind(self);
    self.fetch = (...args) => {
      const url = new URL(args[0] instanceof Request ? args[0].url : args[0], self.origin);
      fetchCalls.set(url.pathname, (fetchCalls.get(url.pathname) || 0) + 1);
      return nativeFetch(...args);
    };
    if (${JSON.stringify(entry)} === "run_bootstrap_cancel_probe") {
      let state = 0;
      globalThis.bootstrap_state = () => state;
      self.fetch = request => new Promise((resolve, reject) => {
        state = 1;
        request.signal.addEventListener("abort", () => {
          state = 2;
          globalThis.release_bootstrap = () => { state = 3; reject(new DOMException("Fixture released aborted Fetch", "AbortError")); };
        }, { once: true });
      });
    }
    self.onmessage = async () => { try {
      const bytes = Uint8Array.from(atob(${JSON.stringify(wasm)}), c => c.charCodeAt(0));
      await __wbg_init({ module_or_path: bytes });
      const result = JSON.parse(await ${entry}(self.origin.startsWith("https://")));
      if (${JSON.stringify(entry)} === "run_malformed_probe") result.fetch_calls = [...fetchCalls];
      self.postMessage({ result });
    } catch (error) { self.postMessage({ error: String(error) }); } };`;
    const url = URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
    const worker = new Worker(url, { type: "module" });
    const timer = setTimeout(() => { worker.terminate(); URL.revokeObjectURL(url); reject(new Error("Worker did not complete")); }, 80_000);
    worker.onerror = (error) => { clearTimeout(timer); worker.terminate(); URL.revokeObjectURL(url); reject(new Error(error.message)); };
    worker.onmessage = ({ data }) => { clearTimeout(timer); worker.terminate(); URL.revokeObjectURL(url); data.error ? reject(new Error(data.error)) : resolve(data.result); };
    worker.postMessage("run");
  }), { glue, wasm, entry });
}
