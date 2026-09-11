import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, writeFile, cp, mkdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium, firefox } from "playwright";
import { startService, waitUntil } from "./service.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");
const directory = await mkdtemp(join(tmpdir(), "rsi-rust-renderer-"));
const assets = join(directory, "assets");
const report = process.env.RSI_WEB_REPORT ?? join(directory, "report");
await mkdir(report, { recursive: true });
await cp(process.env.RSI_WEB_ASSETS, assets, { recursive: true });
const files = ["rust-entry.js", "rsi_web_renderer_fixture.js", "rsi_web_renderer_fixture_bg.wasm"];
await cp(join(root, "fixtures/rsi/web-renderer/entry.js"), join(assets, files[0]));
for (const name of files.slice(1)) await cp(join(process.env.RSI_RENDERER_ASSETS, name), join(assets, name));
const catalog = JSON.parse(await readFile(join(assets, "ui-renderers.json"), "utf8"));
catalog.renderers.push({ id: "fixture.rust", abi: 1, entry: files[0], files: await Promise.all(files.map(async name => ({ name, sha256: createHash("sha256").update(await readFile(join(assets, name))).digest("hex") }))), schemas: [{ name: "fixture.counter", version: 1 }], capabilities: ["invoke", "source"], surfaces: ["root", "pane", "sidebar", "dialog"] });
await writeFile(join(assets, "ui-renderers.json"), JSON.stringify(catalog));
const binary = join(directory, "rsi");
await cp(process.env.RSI_WEB_BINARY ?? join(root, "target/debug/rsi"), binary);
if (process.env.RSI_WEB_BROWSER && !["chromium", "firefox"].includes(process.env.RSI_WEB_BROWSER)) throw new Error("Unknown RSI_WEB_BROWSER");
const browser = await (process.env.RSI_WEB_BROWSER === "firefox" ? firefox : chromium).launch({ headless: true });
let service;
let page;
try {
  service = await startService({ binary, assets, report, configure: async ({ config, run }) => {
    const profile = join(config, "application-profiles/web/application.profile.toml");
    const all = ["index.html", "app.js", "worker.js", "styles.css", "rsi_web.js", "rsi_web_bg.wasm", "mounts.js", "drafts.js", "standard.js", "ui-renderers.json", ...files];
    const text = await readFile(profile, "utf8");
    await writeFile(profile, text.replace(`directory = ${JSON.stringify(assets)}`, `directory = ${JSON.stringify(assets)}, files = ${JSON.stringify(all)}`));
    const source = join(config, "native-ui-source"); await mkdir(source);
    await cp(process.env.RSI_NATIVE_UI_ARTIFACT, join(source, "artifact.bin"));
    const manifest = join(source, "addon.toml");
    const target = `${process.platform === "darwin" ? "macos" : process.platform}-${process.arch === "x64" ? "x86_64" : process.arch === "arm64" ? "aarch64" : process.arch}`;
    await writeFile(manifest, `format = 2\nid = 'fixture.web-ui'\nplugin = 'fixture.native-addon'\nscope = 'service'\ntarget = '${target}'\nartifact = 'artifact.bin'\n`);
    run(["addon", "install", manifest]); run(["addon", "enable", "fixture.web-ui"]);
    const host = join(config, "host-profiles/fixture/host.profile.toml");
    await writeFile(host, (await readFile(host, "utf8")) + `\n[[steps]]\nkind = 'plugin'\nid = 'native-ui'\nplugin = 'fixture.native-addon'\nconfig = { label = 'Web UI', tools = false, ui = true }\n[[steps]]\nkind = 'plugin'\nid = 'ui-adapter'\nplugin = 'rsi.service.ui.portable'\nconfig = { service = 'fixture.native.ui', business_api = true }\n`);
  } });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1280, height: 980 } });
  await context.addInitScript(() => {
    const NativeWorker = Worker;
    window.workerStarts = 0;
    window.Worker = class extends NativeWorker { constructor(...args) { super(...args); window.workerStarts++; this.addEventListener("message", event => { if (event.data.kind === "view") { window.admittedOffer = JSON.parse(event.data.assets); const frame = JSON.parse(event.data.view); const detail = frame.view?.ui_detail ?? frame.sections?.ui_detail; if (detail) window.nativeDetail = detail; } }); } };
  });
  page = await context.newPage(); const errors = []; page.on("pageerror", error => errors.push(error.message));
  await page.goto(service.origin);
  await page.locator("#receipt").fill(JSON.stringify(service.register("Rust renderer ABI")));
  await page.getByRole("button", { name: "Connect", exact: true }).click();
  await page.locator("#workbench").waitFor({ state: "visible" });
  await page.waitForFunction(() => window.admittedOffer);
  // Synthetic slots have their own document owner; the real product keeps its own.
  const abiPage = await context.newPage();
  await abiPage.route(`${service.origin}/abi-fixture`, route => route.fulfill({ contentType: "text/html", body: '<!doctype html><link rel="stylesheet" href="/styles.css"><title>Rust renderer ABI</title>' }));
  await abiPage.goto(`${service.origin}/abi-fixture`);
  const result = await abiPage.evaluate(async offer => {
    const { MountTable } = await import("/mounts.js");
    const module = await import(`/rsi-renderers/${offer.revision}/rust-entry.js`);
    const table = await MountTable.open();
    const panel = document.createElement("main"); panel.className = "login"; panel.setAttribute("aria-label", "Rust document renderers");
    document.body.append(panel);
    const slots = ["root", "pane", "sidebar", "dialog"].map((surface, index) => {
      const root = document.createElement("section"); panel.append(root);
      return { key: surface, surface, root, binding: surface, host: {}, snapshot: { model: { renderer: "fixture.rust", schema: { name: "fixture.counter", version: 1 }, data: { label: `Rust ${surface} presentation`, count: index + 1 }, actions: [{ name: "refresh", title: "Refresh" }], sources: [{ name: "raw", title: "Raw bytes", media_type: "application/octet-stream" }], standard_view: null } } };
    });
    await table.render(offer, slots);
    const mounted = module.live_renderers();
    const node = slots[0].root.querySelector("h2");
    const changed = slots.map(slot => ({ ...slot, snapshot: { model: { ...slot.snapshot.model, data: { ...slot.snapshot.model.data, count: 42 } } } }));
    await table.render(offer, changed);
    const unchangedNode = node === slots[0].root.querySelector("h2");
    await table.render(offer, changed.map(slot => ({ ...slot, snapshot: { ...slot.snapshot, busy: true } })));
    const busyInputsBlocked = slots.every(slot => [...slot.root.querySelectorAll("button")].every(button => button.disabled));
    await table.render(offer, changed);
    const readyInputsEnabled = slots.every(slot => [...slot.root.querySelectorAll("button")].every(button => !button.disabled));
    const visible = panel.innerText;
    const invalid = changed.map(slot => ({ ...slot, snapshot: { model: { ...slot.snapshot.model, schema: { name: "fixture.counter", version: 2 } } } }));
    let invalidRejected = false;
    try { await table.render(offer, invalid); } catch { invalidRejected = true; }
    const preserved = visible === panel.innerText;
    window.finishRustFixture = async () => { await table.close(); const alive = module.live_renderers(); panel.remove(); return alive; };
    return { mounted, unchangedNode, busyInputsBlocked, readyInputsEnabled, invalidRejected, preserved, visible };
  }, await page.evaluate(() => window.admittedOffer));
  assert.equal(result.mounted, 4); assert.equal(result.unchangedNode, true); assert.equal(result.invalidRejected, true); assert.equal(result.preserved, true);
  assert.equal(result.busyInputsBlocked, true, "source and action input must wait for the refreshed model's usable ticket");
  assert.equal(result.readyInputsEnabled, true);
  assert.equal((result.visible.match(/Rust\/WASM count: 42/g) ?? []).length, 4);
  await abiPage.screenshot({ path: join(report, "four-rust-wasm-mounts.png") });
  assert.equal(await abiPage.evaluate(() => window.finishRustFixture()), 0);
  await abiPage.close();
  await page.locator(".workspace-add summary").click();
      await page.locator("#workspace-path").fill(service.workspace);
  await page.getByRole("button", { name: "Add workspace", exact: true }).click();
  await page.locator("#workspaces .nav-item").first().click();
  const pane = page.getByRole("region", { name: "Main conversation", exact: true });
  await pane.locator(".pane-session").filter({ hasText: service.workspace }).waitFor();
  const session = (await pane.locator(".pane-session").innerText()).split(" · ").at(-1);
  for (let cycle = 0; cycle < 3; cycle++) {
    await page.getByRole("button", { name: "Service extensions", exact: true }).click();
    await page.locator("#detail").getByRole("button", { name: "Native Session model", exact: true }).click();
    const detail = page.locator("#detail");
    await page.waitForFunction(() => window.nativeDetail?.error || window.nativeDetail?.model);
    assert.equal((await page.evaluate(() => window.nativeDetail)).error, null);
    await detail.getByRole("heading", { name: `Native Session ${session}`, exact: true }).waitFor();
    assert.equal(await page.evaluate(async () => (await import(`/rsi-renderers/${window.admittedOffer.revision}/rust-entry.js`)).live_renderers()), 1);
    const before = await detail.locator(".renderer-mount p").innerText();
    await detail.getByRole("button", { name: "Refresh native model", exact: true }).click();
    await page.waitForFunction(before => document.querySelector("#detail .renderer-mount p")?.textContent !== before, before);
    await detail.getByRole("button", { name: "Read native bytes", exact: true }).click();
    await detail.locator("[data-fixture-bytes]").filter({ hasText: "00 ff 41 42 43" }).waitFor();
    if (cycle === 0) await page.screenshot({ path: join(report, "native-session-rust-wasm.png") });
    await page.getByRole("button", { name: "Close details", exact: true }).click();
    await waitUntil(() => page.evaluate(async () => (await import(`/rsi-renderers/${window.admittedOffer.revision}/rust-entry.js`)).live_renderers() === 0), "Rust renderer disposal");
  }
  assert.equal(await page.evaluate(() => window.workerStarts), 1);
  await page.evaluate(() => { document.addEventListener("rsi-disconnected", event => { window.closedResources = event.detail; }, { once: true }); });
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await page.locator("#login").waitFor({ state: "visible" });
  assert.deepEqual(await page.evaluate(() => window.closedResources), { pending_timers: 0, active_alarms: 0, active_requests: 0 });
  assert.deepEqual(errors, []);
  const evidence = { status: "passed", browser: browser.version(), ...result, disposed: 7, providerRequests: service.provider.requests.length, nativeSession: session, nativeCycles: 3, workerStarts: 1, nativeRawBytes: "00 ff 41 42 43", boundary: "four synthetic ABI slots plus actual native UI -> scoped Session API -> authenticated UI API -> Worker -> Rust/WASM DOM" };
  await writeFile(join(report, "result.json"), JSON.stringify(evidence, null, 2)); console.log(JSON.stringify(evidence));
} catch (error) {
  if (page) { await page.screenshot({ path: join(report, "failure.png") }); const body = await page.locator("body").innerText(); await writeFile(join(report, "failure.txt"), body); const detail = JSON.stringify(await page.evaluate(() => window.nativeDetail ?? null)); await writeFile(join(report, "failure-detail.json"), detail); console.error(JSON.stringify({ body, detail })); }
  throw error;
} finally { await browser.close(); await service?.close(); }
