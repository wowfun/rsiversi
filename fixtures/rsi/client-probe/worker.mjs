import init, { run_probe } from "/pkg/rsi_client_browser_probe.js";

self.rsiProbeProgress = (state, detail) => self.postMessage({kind:"progress", state, detail});

self.onmessage = () => {
  (async () => {
    await init();
    self.postMessage(JSON.parse(await run_probe()));
  })().catch((error) => {
    setTimeout(() => { throw error; }, 0);
  });
};
