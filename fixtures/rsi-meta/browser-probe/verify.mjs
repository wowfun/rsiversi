import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright";
import { runWorkerProbe } from "../../tools/browser-worker.mjs";

await runWorkerProbe({
  fixture: dirname(fileURLToPath(import.meta.url)), stem: "rsi_meta_browser_probe",
  engines: [["chromium", chromium], ["firefox", firefox]], cases: 13, trap: true,
});
