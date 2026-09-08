import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

for (const secure of [false, true]) {
  const result = spawnSync(process.execPath, [fileURLToPath(new URL("verify.mjs", import.meta.url))], {
    stdio: "inherit", timeout: 900_000,
    env: { ...process.env, RSI_FETCH_TLS: secure ? "1" : "0" },
  });
  assert.equal(result.status, 0, result.error?.message || `Fetch ${secure ? "TLS" : "HTTP"} failed`);
}
