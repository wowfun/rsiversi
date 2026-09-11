import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { mkdir, readFile, writeFile, copyFile, chmod } from "node:fs/promises";
import { resolve, join } from "node:path";
import { startService, boundedRun } from "../web-product/service.mjs";

const require = createRequire(new URL("../web-product/package.json", import.meta.url));
const { chromium, firefox } = require("playwright");
const report = resolve(process.env.RSI_WORKBENCH_REPORT ?? ".local/notes/0908/evidence/p8-workbench-browser");
const original = process.env.RSI_WORKBENCH_BINARY;
const assets = process.env.RSI_WEB_ASSETS;
assert.ok(original && assets, "RSI_WORKBENCH_BINARY and RSI_WEB_ASSETS are required");
await mkdir(report, { recursive: true });
const binary = join(report, "workbench-addon");
await copyFile(original, binary); await chmod(binary, 0o700);
const binarySha256 = createHash("sha256").update(await readFile(binary)).digest("hex");
const profile = boundedRun(binary, ["fixture-profile"]).stdout.toString();
const results = [];
for (const [name, engine] of [["chromium", chromium], ["firefox", firefox]]) {
  const destination = join(report, name); await mkdir(destination, { recursive: true });
  const requests = [];
  const service = await startService({ binary, assets, report: destination,
    async configure({ config }) {
      const preset = join(config, "agent-presets/workbench"); await mkdir(preset, { recursive: true });
      await writeFile(join(preset, "agent.profile.toml"), profile);
      const settingsPath = join(config, "settings.json");
      const settings = JSON.parse(await readFile(settingsPath));
      settings["rsi.agent-presets"] = { default: "workbench" };
      await writeFile(settingsPath, JSON.stringify(settings));
    },
    onRequest(body) {
      requests.push({ planEnabled: JSON.stringify(body.messages).includes("Plan mode is enabled"), independentTool: body.tools?.some(tool => tool.function?.description === "Independent workbench A") });
    },
  });
  const browser = await engine.launch({ headless: true });
  const errors = [];
  try {
    const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 980 } });
    const page = await context.newPage(); page.setDefaultTimeout(30_000);
    page.on("pageerror", error => errors.push(error.message));
    await page.goto(service.origin);
    await page.locator("#receipt").fill(JSON.stringify(service.register(`${name} addon acceptance`)));
    await page.getByRole("button", { name: "Connect", exact: true }).click();
    await page.locator("#workbench").waitFor({ state: "visible" });
    await page.locator(".workspace-add summary").click();
    await page.locator("#workspace-path").fill(service.workspace);
    await page.getByRole("button", { name: "Add workspace", exact: true }).click();
    await page.locator("#workspaces .nav-item").first().click();
    const pane = page.getByRole("region", { name: "Main conversation", exact: true });
    const extensions = pane.locator(".session-extensions");
    const plan = extensions.locator('[data-producer="rsi.plan-policy.view"] pre');
    await extensions.locator("summary").click();
    await plan.filter({ hasText: '"enabled": false' }).waitFor();
    assert.equal(requests.length, 0);
    await pane.getByRole("textbox", { name: "Main message" }).fill("/plan on");
    await pane.getByRole("button", { name: "Send ↗" }).click();
    await plan.filter({ hasText: '"enabled": true' }).waitFor();
    assert.match(await extensions.locator("summary").innerText(), /Draft · revision 1/);
    assert.equal(requests.length, 0);
    await page.screenshot({ path: join(destination, "draft.png") });
    await pane.getByRole("textbox", { name: "Main message" }).fill("Inspect independent addon");
    await pane.getByRole("button", { name: "Send ↗" }).click();
    await pane.locator(".pane-status").filter({ hasText: "Completed" }).waitFor();
    assert.deepEqual(requests, [{ planEnabled: true, independentTool: true }]);
    await pane.getByRole("textbox", { name: "Main message" }).fill("/plan off");
    await pane.getByRole("button", { name: "Send ↗" }).click();
    await plan.filter({ hasText: '"enabled": false' }).waitFor();
    assert.match(await extensions.locator("summary").innerText(), /Durable/);
    assert.equal(requests.length, 1);
    await page.screenshot({ path: join(destination, "durable.png") });
    await page.getByRole("button", { name: "Settings", exact: true }).click();
    await page.getByText("Additional settings", { exact: true }).click();
    await page.getByRole("button", { name: "Open registered settings", exact: true }).click();
    await page.getByRole("button", { name: "fixture.workbench", exact: true }).click();
    const editor = page.getByRole("textbox", { name: "Settings JSON" });
    await editor.fill(JSON.stringify({ note: "public browser settings" }));
    const denied = page.waitForResponse(response => response.url().endsWith('/api/v1/settings/replace/1'));
    await page.getByRole("button", { name: "Save settings", exact: true }).click();
    assert.equal((await denied).status(), 401);
    assert.deepEqual(JSON.parse(await editor.inputValue()), { note: "public browser settings" });
    await page.screenshot({ path: join(destination, "settings.png") });
    assert.deepEqual(errors, []);
    results.push({ browser: name, version: browser.version(), requests, passed: true });
  } catch (error) {
    await writeFile(join(destination, "failure.json"), JSON.stringify({ error: String(error), errors, requests }, null, 2));
    throw error;
  } finally { await browser.close(); await service.close(); }
}
await writeFile(join(report, "result.json"), JSON.stringify({ binarySha256, results }, null, 2));
console.log(JSON.stringify({ binarySha256, results }));
