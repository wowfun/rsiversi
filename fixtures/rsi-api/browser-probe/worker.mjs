import init, { run_probe } from "/pkg/rsi_api_browser_probe.js";

async function dispatch(data) {
  await init();
  if (data === "trap") {
    self.postMessage({ state: "started" });
    throw new Error("API fixture has no fatal-trap scenario");
  } else {
    self.postMessage({ state: "completed", result: JSON.parse(await run_probe()) });
  }
}

self.onmessage = ({ data }) => {
  dispatch(data).catch((error) => {
    // An async message handler's rejected Promise does not emit Worker.onerror.
    // Escalate it to the owner as a fatal error, without a cleanup acknowledgement.
    setTimeout(() => { throw error; }, 0);
  });
};
