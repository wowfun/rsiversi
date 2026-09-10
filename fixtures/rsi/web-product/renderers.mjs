import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, writeFile, cp, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright";
import { startService } from "./service.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const directory = await mkdtemp(join(tmpdir(), "rsi-renderer-product-"));
const assets = join(directory, "assets");
const report = process.env.RSI_WEB_REPORT ?? join(directory, "report");
await mkdir(report, { recursive: true });
await cp(process.env.RSI_WEB_ASSETS, assets, { recursive: true });
const base = (await readFile(join(root, "plugins/rsi/web/standard.js"), "utf8")).replace("export async function mount", "async function standardMount");
async function writeGeneration(label, gate = false, fail = false) {
  const renderer = `${base}\nexport async function mount(root, snapshot, host, signal) {\n${gate ? "if (!window.candidateStarted) { window.candidateStarted = true; await new Promise(resolve => { window.finishCandidate = resolve; }); }" : ""}\n${fail ? 'throw new Error("intentional candidate mount failure");' : `const mounted = await standardMount(root, snapshot, host, signal);\nroot.dataset.rendererRevision = ${JSON.stringify(label)};\nwindow.oldLazy = () => import("./lazy.js").then(module => module.label);\nreturn { update: mounted.update, async dispose() { await new Promise(resolve => setTimeout(resolve, 150)); await mounted.dispose(); } };`}\n}\n`;
  const lazy = `export const label = ${JSON.stringify(label)};\n`;
  const files = [{ name: "standard.js", bytes: renderer }, { name: "lazy.js", bytes: lazy }];
  for (const file of files) await writeFile(join(assets, file.name), file.bytes);
  await writeFile(join(assets, "ui-renderers.json"), JSON.stringify({ format: 1, renderers: [{ id: "rsi.standard", abi: 1, entry: "standard.js", files: files.map(file => ({ name: file.name, sha256: createHash("sha256").update(file.bytes).digest("hex") })), schemas: [{ name: "rsi.standard.view", version: 1 }], capabilities: ["invoke", "focus"], surfaces: ["dialog"] }] }));
}
await writeGeneration("A");
const binary = join(directory, "rsi");
await cp(process.env.RSI_WEB_BINARY ?? join(root, "target/debug/rsi"), binary);
let service;
let diagnosticPage;
if (process.env.RSI_WEB_BROWSER && !["chromium", "firefox"].includes(process.env.RSI_WEB_BROWSER)) throw new Error("Unknown RSI_WEB_BROWSER");
const browser = await (process.env.RSI_WEB_BROWSER === "firefox" ? firefox : chromium).launch({ headless: true });
try {
  service = await startService({ binary, assets, report, configure: async ({ config }) => {
    const profile = join(config, "application-profiles/web/application.profile.toml");
    const files = ["index.html", "app.js", "worker.js", "styles.css", "rsi_web.js", "rsi_web_bg.wasm", "mounts.js", "standard.js", "ui-renderers.json", "lazy.js"];
    const content = await readFile(profile, "utf8");
    await writeFile(profile, content.replace(`directory = ${JSON.stringify(assets)}`, `directory = ${JSON.stringify(assets)}, watch = true, files = ${JSON.stringify(files)}`));
  } });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 980 } });
  await context.addInitScript(() => {
    window.workerStarts = 0;
    const NativeWorker = Worker;
    window.Worker = class extends NativeWorker { constructor(...args) { super(...args); window.workerStarts++; } };
  });
  const page = await context.newPage();
  diagnosticPage = page;
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  const modules = [];
  page.on("response", response => { if (response.url().includes("/rsi-renderers/") && response.url().endsWith("standard.js")) modules.push(response.url()); });
  await page.goto(service.origin);
  await page.locator("#receipt").fill(JSON.stringify(service.register("renderer reload")));
  await page.getByRole("button", { name: "Connect", exact: true }).click();
  await page.locator("#workbench").waitFor({ state: "visible" });
  await page.locator("#workspace-path").fill(service.workspace);
  await page.getByRole("button", { name: "Add workspace", exact: true }).click();
  await page.locator("#workspaces .nav-item").first().click();
  const pane = page.getByRole("region", { name: "Left conversation", exact: true });
  await pane.getByRole("textbox", { name: "Left message" }).fill("hold this turn");
  await pane.getByRole("button", { name: "Send ↗" }).click();
  await pane.locator(".transcript").filter({ hasText: "Waiting for cancellation" }).waitFor();
  await pane.getByRole("textbox", { name: "Left message" }).fill("resident draft 界");
  const session = await pane.locator(".pane-session").innerText();
  await pane.getByRole("button", { name: "Workspace files", exact: true }).click();
  const form = page.locator(".ui-contribution");
  await form.getByRole("textbox", { name: "Workspace-relative path", exact: true }).fill("kept form draft");
  await page.locator('[data-renderer-revision="A"]').waitFor();
  assert.equal(modules.length, 1);
  const oldLazy = modules[0].replace("standard.js", "lazy.js");
  await writeGeneration("B", true);
  await page.waitForFunction(() => window.candidateStarted);
  assert.equal(await page.evaluate(() => window.oldLazy()), "A");
  await page.locator('[data-renderer-revision="A"]').waitFor();
  await page.screenshot({ path: join(report, "pending-new-renderer-old-visible.png") });
  await page.evaluate(() => window.finishCandidate());
  await page.locator('[data-renderer-revision="B"]').waitFor();
  assert.equal(await form.getByRole("textbox", { name: "Workspace-relative path", exact: true }).inputValue(), "kept form draft");
  assert.equal(await pane.getByRole("textbox", { name: "Left message" }).inputValue(), "resident draft 界");
  assert.equal(await pane.locator(".pane-session").innerText(), session);
  assert.equal(await page.evaluate(() => window.workerStarts), 1);
  assert.equal(service.provider.requests.length, 1);
  await page.waitForFunction(async url => (await fetch(url)).status === 404, oldLazy);
  await page.screenshot({ path: join(report, "renderer-b-preserved-controller-and-drafts.png") });
  await writeGeneration("C", false, true);
  await page.locator("#notice").filter({ hasText: "intentional candidate mount failure" }).waitFor();
  await page.locator('[data-renderer-revision="B"]').waitFor();
  assert.equal(await form.getByRole("textbox", { name: "Workspace-relative path", exact: true }).inputValue(), "kept form draft");
  await page.screenshot({ path: join(report, "failed-candidate-keeps-b.png") });
  await page.getByRole("button", { name: "Close details", exact: true }).click();
  await pane.getByRole("button", { name: "Cancel", exact: true }).click();
  // Exercise the actual WASM Assets owner: the server commits D, but the Worker
  // never receives its reply. Mutations must not be replayed on this connection.
  let lostCommitRequests = 0;
  await context.route("**/api/v1/web-assets/commit/1", async route => {
    lostCommitRequests++;
    const committed = await route.fetch();
    assert.equal(committed.status(), 200);
    assert.equal(await committed.text(), "true");
    await route.abort("failed");
  });
  await pane.getByRole("button", { name: "Workspace files", exact: true }).click();
  await page.locator('[data-renderer-revision="B"]').waitFor();
  await writeGeneration("D");
  await page.locator("#connection-state").filter({ hasText: "Connection failed" }).waitFor();
  await page.locator("#login").waitFor({ state: "visible" });
  await page.locator("#detail").waitFor({ state: "hidden" });
  assert.equal(lostCommitRequests, 1);
  await page.screenshot({ path: join(report, "lost-commit-explicit-reconnect.png") });
  await context.unroute("**/api/v1/web-assets/commit/1");
  await page.locator("#reconnect").click();
  await page.locator("#workbench").waitFor({ state: "visible" });
  await page.locator("#workspaces .nav-item").first().click();
  await pane.getByRole("button", { name: "Workspace files", exact: true }).click();
  await page.locator('[data-renderer-revision="D"]').waitFor();
  assert.equal(await page.evaluate(() => window.workerStarts), 2);
  await page.screenshot({ path: join(report, "lost-commit-reconnected-renderer-d.png") });
  await page.getByRole("button", { name: "Close details", exact: true }).click();
  await page.locator("#detail").waitFor({ state: "hidden" });
  await pane.getByRole("textbox", { name: "Left message" }).fill("hold this turn after recovery");
  await pane.getByRole("button", { name: "Send ↗" }).click();
  await pane.locator(".transcript").filter({ hasText: "Waiting for cancellation" }).waitFor();
  await pane.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.evaluate(() => { document.addEventListener("rsi-disconnected", event => { window.closedResources = event.detail; }, { once: true }); });
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.locator("#login").waitFor({ state: "visible" });
  const resources = await page.evaluate(() => window.closedResources);
  if (!resources) console.error(JSON.stringify({ diagnostic: await page.locator("#notice").innerText(), state: await page.locator("#connection-state").innerText(), errors }));
  assert.deepEqual(resources, { pending_timers: 0, active_alarms: 0, active_requests: 0 });
  assert.deepEqual(errors, []);
  const modelRequests = service.provider.requests.length;
  await service.close(); service = undefined;
  await writeGeneration("C", false, true);
  // Cold static and executable-but-broken offers have no accepted renderer.
  // Both must leave ordinary Session input and sign-out usable.
  for (const mode of ["static", "broken"]) {
    await mkdir(join(report, mode));
    service = await startService({ binary, assets, report: join(report, mode), configure: async ({ config }) => {
      const profile = join(config, "application-profiles/web/application.profile.toml");
      const content = await readFile(profile, "utf8");
      const files = ["index.html", "app.js", "worker.js", "styles.css", "rsi_web.js", "rsi_web_bg.wasm", "mounts.js"];
      if (mode === "broken") files.push("standard.js", "ui-renderers.json", "lazy.js");
      await writeFile(profile, content.replace(`directory = ${JSON.stringify(assets)}`, `directory = ${JSON.stringify(assets)}, files = ${JSON.stringify(files)}`));
    } });
    const cold = await context.newPage();
    diagnosticPage = cold;
    cold.on("pageerror", error => errors.push(error.message));
    await cold.goto(service.origin);
    await cold.locator("#receipt").fill(JSON.stringify(service.register(`${mode} renderer`)));
    await cold.getByRole("button", { name: "Connect", exact: true }).click();
    await cold.locator("#workbench").waitFor({ state: "visible" });
    await cold.locator("#workspace-path").fill(service.workspace);
    await cold.getByRole("button", { name: "Add workspace", exact: true }).click();
    await cold.locator("#workspaces .nav-item").first().click();
    const coldPane = cold.getByRole("region", { name: "Left conversation", exact: true });
    await coldPane.getByRole("button", { name: "Workspace files", exact: true }).click();
    await cold.locator(".renderer-mount").filter({ hasText: "Renderer unavailable: rsi.standard" }).waitFor();
    await cold.screenshot({ path: join(report, `${mode}-catalog-diagnostic.png`) });
    await cold.getByRole("button", { name: "Close details", exact: true }).click();
    await cold.locator("#detail").waitFor({ state: "hidden" });
    await coldPane.getByRole("textbox", { name: "Left message" }).fill("hold this turn");
    await coldPane.getByRole("button", { name: "Send ↗" }).click();
    await coldPane.locator(".transcript").filter({ hasText: "Waiting for cancellation" }).waitFor();
    await coldPane.getByRole("button", { name: "Cancel", exact: true }).click();
    assert.equal(await cold.evaluate(() => window.workerStarts), 1);
    await cold.evaluate(() => document.addEventListener("rsi-disconnected", event => { window.closedResources = event.detail; }, { once: true }));
    await cold.getByRole("button", { name: "Sign out", exact: true }).click();
    await cold.locator("#login").waitFor({ state: "visible" });
    assert.deepEqual(await cold.evaluate(() => window.closedResources), resources);
    await cold.close();
    await service.close(); service = undefined;
  }
  assert.deepEqual(errors, []);
  const result = { browser: browser.version(), status: "passed", workerStarts: 2, lostCommitRequests, modelRequests, preservedSession: session, modules, resources, cases: ["A-to-B actual imports", "old lazy import while B mounts", "old graph expires after commit", "resident draft and pending turn", "form draft survives code replacement", "failed C preserves B", "server commit with lost reply closes actual WASM owner without replay", "explicit reconnect renders D and accepts and cancels a new Session turn", "cold static catalog keeps Worker, Session actions and clean shutdown", "cold broken executable rejects first mount without losing Worker or Session input"] };
  await writeFile(join(report, "result.json"), JSON.stringify(result, null, 2));
  console.log(JSON.stringify(result));
} catch (error) {
  if (diagnosticPage) {
    await diagnosticPage.screenshot({ path: join(report, "failure.png") });
    await writeFile(join(report, "failure.txt"), await diagnosticPage.locator("body").innerText());
  }
  throw error;
} finally { await browser.close(); await service?.close(); }
