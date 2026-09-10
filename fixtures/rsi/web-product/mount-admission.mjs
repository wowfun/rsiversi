import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

// Document ABI evidence: real ESM imports, no simulated Worker or domain authority.
export async function verifyMountAdmission(browser, root) {
  const context = await browser.newContext();
  const bridge = await readFile(join(root, "plugins/rsi/web/mounts.js"), "utf8");
  await context.route("http://renderer.test/**", route => {
    const path = new URL(route.request().url()).pathname;
    const body = path === "/" ? "<!doctype html><body>" : path === "/mounts.js" ? bridge : `
      export async function mount(root, snapshot, host, signal) {
        if (window.rejectedRevision && import.meta.url.includes('/' + window.rejectedRevision + '/')) throw new Error('fixture broken module');
        window.hosts.push(host);
        if (snapshot.model.data.pause) {
          window.mountStarted = true;
          await new Promise(resolve => signal.addEventListener('abort', () => {
            window.mountAborted = true;
            window.finishMount = resolve;
          }, { once: true }));
        }
        if (snapshot.model.data.fail) throw new Error('fixture mount failed');
        window.live++;
        root.textContent = 'mounted';
        return { async update(snapshot) {
          if (snapshot.model.data.updateFail) throw new Error('fixture update failed');
          if (snapshot.model.data.updatePause) await new Promise(resolve => { window.finishUpdate = resolve; });
          window.updates++;
        }, async dispose() {
          if (snapshot.model.data.disposePause) await new Promise(resolve => { window.finishDisposal = resolve; });
          await new Promise(resolve => setTimeout(resolve, 5));
          window.live--; window.disposals++; root.replaceChildren();
          if (snapshot.model.data.disposeFail) throw new Error('fixture dispose failed');
        } };
      }`;
    return route.fulfill({ contentType: path === "/" ? "text/html" : "text/javascript", body });
  });
  try {
    const page = await context.newPage();
    await page.goto("http://renderer.test/");
    const result = await page.evaluate(async () => {
      const { MountTable } = await import("/mounts.js");
      const check = (value, label) => { if (!value) throw new Error(label); };
      const rejects = async (body, label) => { let rejected = false; try { await body(); } catch { rejected = true; } check(rejected, label); };
      window.hosts = []; window.live = 0; window.updates = 0; window.disposals = 0;
      const offer = number => ({ revision: number.toString(16).padStart(64, "0"), catalog: { renderers: [{
        id: "fixture", abi: 1, entry: "fixture.js", files: [{ name: "fixture.js", sha256: "a".repeat(64) }],
        schemas: [{ name: "fixture.model", version: 1 }], capabilities: ["invoke", "source", "focus"], surfaces: ["pane"],
      }] } });
      const source = (key = "one", data = {}) => {
        const root = document.createElement("section"); document.body.append(root);
        return { key, root, binding: key, surface: "pane", snapshot: { model: { renderer: "fixture", schema: { name: "fixture.model", version: 1 }, data, actions: [{ name: "apply" }], sources: [{ name: "raw" }] } }, host: {
          invoke: async () => "done", source: async () => new Uint8Array([255]),
        } };
      };
      let table = new MountTable(); const slot = source();
      check(await table.render(offer(1), []) === undefined, "unexercised cold generation accepted");
      await rejects(() => table.render(offer(1), Array.from({ length: 17 }, (_, i) => source(`s${i}`))), "17 mounts admitted");
      await rejects(() => table.render(offer(1), [slot, slot]), "duplicate slots admitted");
      check((await table.render(offer(1), [slot])).accept, "initial mount rejected");
      const host = window.hosts.at(-1);
      check((await host.source("raw", 0, 1))[0] === 255, "binary source changed");
      await rejects(() => host.source("foreign", 0, 1), "foreign source admitted");
      await rejects(() => host.source("raw", 0, 65537), "large source admitted");
      await rejects(() => host.source("raw", Number.MAX_SAFE_INTEGER + 1, 1), "unsafe offset admitted");
      await rejects(() => host.invoke("foreign", {}), "foreign action admitted");
      await rejects(() => host.invoke("apply", "x".repeat(65536)), "large action admitted");
      await rejects(() => host.focus(document.body), "foreign focus admitted");
      host.setInput("draft", "kept");
      const bad = { ...slot, snapshot: { model: { ...slot.snapshot.model, data: { fail: true } } } };
      check(!(await table.render(offer(2), [bad])).accept, "failed candidate accepted");
      check(window.live === 1 && slot.root.textContent === "mounted", "failed candidate removed old DOM");
      const added = source("added", { fail: true });
      added.snapshot.model.renderer = "new-renderer";
      const expanded = offer(2);
      expanded.catalog.renderers.push({ ...expanded.catalog.renderers[0], id: "new-renderer" });
      // Use a fresh table to exercise rejection of a newly introduced renderer.
      const warm = new MountTable();
      await warm.render(offer(1), [slot]);
      check(!(await warm.render(expanded, [slot, added])).accept, "broken new renderer accepted");
      check(added.root.textContent.includes("Renderer unavailable: new-renderer"), "warm missing renderer has no placeholder");
      check(slot.root.textContent === "mounted", "warm failure removed working renderer");
      check(await warm.render(expanded, [slot, added]) === undefined, "warm rejected offer acknowledged twice");
      await warm.close();
      check((await table.render(offer(3), [slot])).accept, "replacement failed");
      check(window.hosts.at(-1).input("draft") === "kept", "draft lost on code replacement");
      const current = window.hosts.at(-1);
      await rejects(() => host.invoke("apply", {}), "retired host admitted input");
      const updateFailure = { ...slot, snapshot: { model: { ...slot.snapshot.model, data: { updateFail: true } } } };
      await rejects(() => table.render(offer(3), [source("staged"), updateFailure]), "failed update accepted");
      check(window.live === 1, "failed update leaked a detached candidate");
      await table.render(offer(3), [slot]);
      const oldCalls = [], newCalls = [];
      slot.host = { ...slot.host, invoke: async action => { oldCalls.push(action); } };
      await table.render(offer(3), [slot]);
      const next = { ...slot, host: { ...slot.host, invoke: async action => { newCalls.push(action); } },
        snapshot: { model: { ...slot.snapshot.model, data: { updatePause: true }, actions: [{ name: "next" }] } } };
      const admittedBeforeUpdate = current.invoke("apply", {});
      const updating = table.render(offer(3), [next]);
      while (!window.finishUpdate) await new Promise(resolve => setTimeout(resolve, 1));
      await admittedBeforeUpdate;
      check(oldCalls.join() === "apply" && newCalls.length === 0, "admitted input changed host during update");
      await rejects(() => current.invoke("next", {}), "pending update admitted new-model input");
      await rejects(() => current.invoke("apply", {}), "pending update admitted old-model input");
      window.finishUpdate(); await updating;
      await current.invoke("next", {});
      check(newCalls.join() === "next", "committed input used the old host");
      await rejects(() => current.invoke("apply", {}), "committed model admitted an old action");
      await table.render(offer(3), [slot]);
      const release = [];
      slot.host.invoke = () => new Promise(resolve => release.push(resolve));
      const admitted = Array.from({ length: 8 }, () => current.invoke("apply", {}));
      await Promise.resolve();
      await rejects(() => current.invoke("apply", {}), "ninth action admitted");
      let closed = false; const closing = table.close().then(() => { closed = true; });
      await new Promise(resolve => setTimeout(resolve, 15));
      check(!closed, "close did not drain admitted actions");
      await rejects(() => current.invoke("apply", {}), "closing host admitted action");
      release.forEach(resolve => resolve()); await Promise.all(admitted); await closing;
      check(window.live === 0, "action drain leaked mount");

      table = new MountTable();
      const idleSlot = source("idle");
      await table.render(offer(1), [idleSlot]);
      check(await table.render(offer(2), []) === undefined, "idle replacement was accepted without a mount");
      window.rejectedRevision = offer(2).revision;
      check(!(await table.render(offer(2), [idleSlot])).accept, "broken idle replacement accepted");
      check(idleSlot.root.textContent === "mounted", "idle replacement lost the prior generation");
      await table.close();
      table = new MountTable();
      check(await table.render(offer(2), []) === undefined, "broken cold offer accepted before use");
      const coldSlot = source("cold");
      check(!(await table.render(offer(2), [coldSlot])).accept, "broken cold offer accepted on use");
      check(coldSlot.root.textContent.includes("Renderer unavailable"), "cold mount failure removed the resident diagnostic");
      check(await table.render(offer(2), [coldSlot]) === undefined, "rejected offer was acknowledged twice");
      window.rejectedRevision = undefined;
      check((await table.render(offer(3), [coldSlot])).accept, "cold failure blocked a subsequent valid offer");
      await table.close();

      table = new MountTable();
      const dotted = source(".slot");
      dotted.snapshot.model.renderer = ".renderer";
      dotted.snapshot.model.schema.name = ".schema";
      dotted.snapshot.model.actions[0].name = ".apply";
      const dottedOffer = offer(1);
      dottedOffer.catalog.renderers[0].id = ".renderer";
      dottedOffer.catalog.renderers[0].schemas[0].name = ".schema";
      check((await table.render(dottedOffer, [dotted])).accept, "valid leading-dot identity rejected");
      await window.hosts.at(-1).invoke(".apply", {});
      await table.close();

      table = new MountTable();
      const staticOffer = { ...offer(90), catalog: null }, staticSlot = source("static");
      check((await table.render(staticOffer, [])).accept, "static application rejected");
      await table.render(staticOffer, [staticSlot]);
      check(staticSlot.root.textContent.includes("Renderer unavailable"), "static slot has no diagnostic");
      check((await table.render(offer(1), [staticSlot])).accept, "static catalog could not acquire a renderer");
      check(!(await table.render({ ...offer(91), catalog: null }, [staticSlot])).accept, "static replacement discarded a working renderer");
      check(staticSlot.root.textContent === "mounted", "rejected static replacement removed the old renderer");
      await table.close();

      table = new MountTable();
      const rendering = table.render(offer(4), [source("pending", { pause: true })]);
      // The rejection is expected when close retires this unfinished candidate.
      const rendered = rendering.then(() => false, () => true);
      while (!window.mountStarted) await new Promise(resolve => setTimeout(resolve, 1));
      closed = false; const pendingClose = table.close().then(() => { closed = true; });
      await new Promise(resolve => setTimeout(resolve, 15));
      check(window.mountAborted, "close did not abort the pending mount");
      check(!closed, "close returned before candidate disposal");
      window.finishMount();
      check(await rendered, "closed candidate was committed"); await pendingClose;
      check(window.live === 0, "closed candidate leaked a renderer");

      // Reconnecting a Worker does not unload document ESM records.
      for (let generation = 5; generation <= 32; generation++) {
        table = new MountTable(); await table.render(offer(generation), [source(`g${generation}`)]); await table.close();
      }
      table = new MountTable();
      check(!(await table.render(offer(33), [source("overflow")])).accept, "document module budget reset on reconnect");
      await table.close(); check(window.live === 0, "module limit leaked renderer");
      table = new MountTable(); window.mountStarted = false; window.mountAborted = false;
      const disposalRender = table.render(offer(1), [source("bad-dispose", { pause: true, disposeFail: true })]).catch(() => {});
      while (!window.mountStarted) await new Promise(resolve => setTimeout(resolve, 1));
      const failedClose = table.close(); failedClose.catch(() => {});
      while (!window.mountAborted) await new Promise(resolve => setTimeout(resolve, 1));
      window.finishMount(); await disposalRender;
      await rejects(() => failedClose, "failed candidate disposal reported clean closure");
      return { live: window.live, disposals: window.disposals, imported_generation_limit: 32, pending_mount_joined: true };
    });
    assert.equal(result.live, 0); assert.equal(result.pending_mount_joined, true);
    await page.clock.install();
    await page.evaluate(async () => {
      const { MountTable } = await import("/mounts.js");
      const offer = { revision: "1".padStart(64, "0"), catalog: { renderers: [{
        id: "fixture", abi: 1, entry: "fixture.js", files: [{ name: "fixture.js", sha256: "a".repeat(64) }],
        schemas: [{ name: "fixture.model", version: 1 }], capabilities: [], surfaces: ["pane"],
      }] } };
      const root = document.createElement("section"); document.body.append(root);
      const slot = { key: "hung", root, binding: "hung", surface: "pane", host: {}, snapshot: { model: {
        renderer: "fixture", schema: { name: "fixture.model", version: 1 }, actions: [], sources: [], data: { disposePause: true },
      } } };
      const table = new MountTable(); await table.render(offer, [slot]);
      window.deadlineResult = table.close().then(() => "incorrect success", error => error.message);
      window.retryAfterDeadline = () => new MountTable().render(offer, [slot]).then(() => "incorrect success", error => error.message);
    });
    await page.clock.fastForward(30001);
    assert.match(await page.evaluate(() => window.deadlineResult), /cleanup exceeded 30 seconds/);
    assert.match(await page.evaluate(() => window.retryAfterDeadline()), /reload the page/);
    await page.evaluate(() => window.finishDisposal());
    await page.clock.fastForward(10);
    assert.equal(await page.evaluate(() => window.live), 0);
    return { ...result, stalled_disposal_deadline: true };
  } finally { await context.close(); }
}
