import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { verifyFrameDom } from "./frames.mjs";
import { verifyImageDom } from "./images-dom.mjs";
import { verifyComposer } from "./composer.mjs";

// DOM projection only: no Worker, provider, device or network lifecycle claims.
export async function verifyDom(browser, root, report, name) {
  const page = await browser.newPage();
  try {
    const document = await readFile(join(root, "fixtures/rsi/web-product/document-island.html"), "utf8");
    const standard = await readFile(join(root, "plugins/rsi/web/standard.js"), "utf8");
    await page.route("http://rsi-dom.invalid/**", route => route.fulfill({ contentType: route.request().url().endsWith("standard.js") ? "text/javascript" : "text/html", body: route.request().url().endsWith("standard.js") ? standard : document.replace(/<script[^>]*>[\s\S]*?<\/script>/g, "") }));
    await page.goto("http://rsi-dom.invalid/");
    const offer = { revision: "a".repeat(64), catalog: { format: 1, renderers: [{ id: "rsi.standard", abi: 1, entry: "standard.js", files: [{ name: "standard.js", sha256: createHash("sha256").update(standard).digest("hex") }], schemas: [{ name: "rsi.standard.view", version: 1 }], capabilities: ["invoke", "focus"], surfaces: ["dialog"] }] } };
    await page.evaluate(offer => { window.testRendererOffer = offer; }, offer);
    await page.addStyleTag({ path: join(root, "plugins/rsi/web/styles.css") });
    // Classic exposure is confined to this document-only fixture; production uses ESM.
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "plugins/rsi/web/mounts.js"), "utf8")).replace("export class MountTable", "class MountTable")} globalThis.MountTable = MountTable; })();` });
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "plugins/rsi/web/drafts.js"), "utf8")).replaceAll("export class ", "class ").replaceAll("export function ", "function ")} Object.assign(globalThis, {DraftStore, DraftEditor, validateEditor}); })();` });
    await page.addScriptTag({ content: 'function publish() {} function installActions() {} function selectSurface() {}\n' + (await readFile(join(root, "plugins/rsi/web/app.js"), "utf8")).replace(/^import .*;\n/gm, "").replace('export function initialize() {\n', '').replace(/\n}\s*$/, '') });
    await page.evaluate(async () => {
      mounts = await MountTable.open();
      connection = { drafts: await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)}), mounts, pending: new Map(), worker: { terminate() {} }, closing: false };
      for (const key of ["main", "compare"]) panes.set(key, new Pane(key));
      // These are projection fixtures: give each synthetic attachment valid metadata.
      for (const pane of panes.values()) {
        const render = pane.render.bind(pane);
        pane.render = (data, models) => render(data ? { header: "c".repeat(64), creation: null, ...data } : data, models);
      }
    });
    const results = await page.evaluate(() => {
      return ["approval", "question"].map(kind => {
        const data = { generation: "1", session: "retained-session", path: "/workspace", draft: "",
          model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Running", omitted: false },
          pending: [{ id: "request-1", owner: "retained-session", kind, title: "Act on this request" }],
          notice: "", history_more: false, historical: false };
        panes.get("main").render(data, []);
        const initial = panes.get("main").waiting.children.length;
        panes.get("main").reset();
        const closed = panes.get("main").waiting.children.length;
        panes.get("main").render(data, []);
        const reopened = panes.get("main").waiting.children.length;
        panes.get("main").render({ ...data, generation: "2" }, []);
        return { kind, initial, closed, reopened, replaced: panes.get("main").waiting.children.length };
      });
    });
    assert.deepEqual(results, ["approval", "question"].map(kind => ({ kind, initial: 1, closed: 0, reopened: 1, replaced: 1 })));
    const approvals = await page.evaluate(() => {
      const sent = [];
      command = async input => { sent.push(input); };
      for (const owner of ["parent-session", "child-session"]) {
        renderDetail({ detail: { pane: "main", generation: "one", kind: "approval", request: {
          id: "call-1", subject: { session_id: owner }, reason: owner, action: "run tool", review: { owner },
        } } });
      }
      const text = $("detail").textContent;
      [...$("detail").querySelectorAll("button")].find(button => button.textContent === "Allow once").click();
      return { text, sent };
    });
    assert(approvals.text.includes("child-session") && !approvals.text.includes("parent-session"));
    assert.equal(approvals.sent.length, 1);
    assert.equal(approvals.sent[0].owner, "child-session");
    const draftEcho = await page.evaluate(async () => {
      const pane = panes.get("main"), originalCall = call;
      const data = { generation: "draft-echo", session: "retained-session", path: "/workspace",
        model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" };
      let submitted;
      try {
        call = async (method, payload) => {
          if (method === "prepare_submission") { submitted = JSON.parse(payload).text; return JSON.stringify({kind:"message",id:"message-one",opaque:payload,text_bytes:submitted.length,images:0}); }
          return JSON.stringify({status:"complete",receipt:"{}"});
        };
        pane.render(data, []); await pane.binding;
        pane.input.focus(); pane.input.value = "locally persisted draft";
        pane.input.dispatchEvent(new Event("input", { bubbles: true })); await pane.flush();
        pane.send.focus(); pane.render({...data, draft:"obsolete Worker input"}, []);
        const retained = pane.input.value;
        await pane.submit(false);
        pane.render({...data, draft:"locally persisted draft"}, []);
        return { retained, submitted, cleared: pane.input.value };
      } finally { call = originalCall; }
    });
    assert.deepEqual(draftEcho, { retained: "locally persisted draft", submitted: "locally persisted draft", cleared: "" });
    const retries = await page.evaluate(async () => {
      const pane = panes.get("main"), originalCall = call;
      const results = [];
      try {
        call = async () => JSON.stringify({status:"complete",receipt:"{}"});
        for (const text of ["original", "edited next draft"]) {
          pane.edit("original"); await pane.flush();
          const editor = pane.editor;
          await editor.update(record => editor.store.freeze(record, {kind:"message",id:`retry-${text.length}`,opaque:"{}",text_bytes:8,images:0}));
          await editor.update(record => editor.store.begin(record));
          if (text !== "original") { pane.edit(text); await pane.flush(); }
          pane.renderComposer();
          const label = pane.send.textContent, steerDisabled = pane.steer.disabled;
          await pane.submit(false);
          results.push({ text, remaining: pane.input.value, label, steerDisabled });
        }
        return results;
      } finally { call = originalCall; }
    });
    assert.deepEqual(retries, [
      { text: "original", remaining: "", label: "Retry previous", steerDisabled: true },
      { text: "edited next draft", remaining: "edited next draft", label: "Retry previous", steerDisabled: true },
    ]);
    await writeFile(join(report, `${name}-composer.json`), JSON.stringify(await verifyComposer(page)));
    await verifyImageDom(page);
    await verifyFrameDom(page, report, name);
    const ime = await page.evaluate(() => {
      let submitted = 0;
      panes.get("main").submit = async () => { submitted++; };
      const input = panes.get("main").input;
      for (const options of [{ isComposing: true }, { keyCode: 229 }]) {
        input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true, ...options }));
      }
      const composing = submitted;
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
      return { composing, after: submitted };
    });
    assert.deepEqual(ime, { composing: 0, after: 1 });
    const failedSetup = await page.evaluate(() => {
      renderUiDetail({ ticket: "fixture-startup", model: null, binding: null, error: null });
      const loading = document.querySelector("#detail-body").textContent;
      renderUiDetail({ ticket: "fixture-startup", model: null, binding: null, error: "Source startup rejected" });
      return { loading, failed: document.querySelector("#detail-body").textContent };
    });
    assert.deepEqual(failedSetup, { loading: "Loading…", failed: "Source startup rejected" });
    const contributed = await page.evaluate(async () => {
      const sent = [];
      command = async input => { sent.push(input); };
      const reference = { application: "ui-nonce", target: "3", contribution: "4", name: "echo" };
      const bound = { reference, actions: { echo: reference }, view: { title: "Addon", elements: [
        { kind: "text", text: "<script>window.addonExecuted = true</script>" },
        { kind: "input", name: "message", label: "Addon text", value: "initial", multiline: true },
        { kind: "button", action: "echo", label: "Apply addon", value: { expected: "original" } },
      ] } };
      const detail = { pane: "main", generation: "one", ticket: "100", binding: bound.reference, error: null, busy: false,
        model: { renderer: "rsi.standard", schema: { name: "rsi.standard.view", version: 1 }, data: null, standard_view: bound.view, actions: [{ name: "echo", title: "Apply addon" }], sources: [] } };
      const show = async detail => { rendererSlots = []; renderDetail({ ui_detail: detail }); await mounts.render(window.testRendererOffer, rendererSlots); };
      await show(detail);
      document.querySelector("[data-ui-field]").value = "edited 界";
      document.querySelector("[data-ui-field]").dispatchEvent(new Event("input", { bubbles: true }));
      await show({ ...detail, ticket: "101", busy: true });
      const busy = document.querySelector("[data-ui-field]").disabled;
      await show({ ...detail, ticket: "101", error: "Validation rejected", busy: false });
      document.querySelector(".ui-contribution > button").click();
      await new Promise(resolve => setTimeout(resolve, 0));
      return { sent, busy, remaining: document.querySelector("[data-ui-field]").value,
        scripts: document.querySelectorAll(".ui-contribution script").length,
        executed: !!window.addonExecuted, text: document.querySelector(".ui-contribution").textContent };
    });
    assert.equal(contributed.busy, true);
    assert.equal(contributed.remaining, "edited 界");
    assert.equal(contributed.scripts, 0); assert.equal(contributed.executed, false);
    assert.match(contributed.text, /<script>/);
    assert.deepEqual(contributed.sent, [{ action: "ui_invoke", ticket: "101",
      name: "echo",
      input: { value: { expected: "original" }, fields: { message: "edited 界" } } }]);
    await page.screenshot({ path: join(report, `${name}-contributed-form-dom.png`) });
    const staleFailure = await page.evaluate(async () => {
      await mounts.close();
      const NativeWorker = window.Worker, originalPresent = presentFrame;
      class StubWorker { terminated = false; terminate() { this.terminated = true; } postMessage() {} }
      let rejectFrame;
      try {
        window.Worker = StubWorker;
        presentFrame = () => new Promise((_, reject) => { rejectFrame = reject; });
        const old = await makeWorker(); old.authenticationDone();
        const frame = old.worker.onmessage({ data: { kind: "view", view: "{}", assets: "{}" } });
        await Promise.resolve();
        old.worker.onerror({ preventDefault() {} });
        const replacement = await makeWorker(); replacement.authenticationDone();
        rejectFrame(new Error("late old-render failure")); await frame;
        return { retained: connection === replacement, terminated: replacement.worker.terminated };
      } finally {
        worker?.terminate(); worker = undefined;
        window.Worker = NativeWorker; presentFrame = originalPresent;
      }
    });
    assert.deepEqual(staleFailure, { retained: true, terminated: false });
    const admission = await page.evaluate(async () => {
      const sent=[];
      worker={postMessage(message) {sent.push(message);},terminate(){}};
      connection={worker,mounts,pending:new Map(),closing:false};
      pending=connection.pending; closing=false;
      const ordinary=Array.from({length:8},()=>call("command","{}"));
      const excess=await call("command","{}").then(()=>false,error=>error.notAdmitted===true);
      closing=true;
      const lifecycle=call("disconnect",true);
      const afterClose=await call("command","{}").then(()=>false,error=>error.notAdmitted===true);
      const kinds=sent.map(message=>message.method);
      for(const waiter of connection.pending.values()) waiter.resolve(null);
      connection.pending.clear(); await Promise.all([...ordinary,lifecycle]);
      return {excess,afterClose,kinds};
    });
    assert.deepEqual(admission,{excess:true,afterClose:true,kinds:[...Array(8).fill("command"),"disconnect"]});
    const drainOrder = await page.evaluate(async () => {
      await mounts.close();
      const order = [];
      let releaseDrafts, releaseMounts, enteredMounts;
      const drafts = new Promise(resolve => { releaseDrafts = resolve; });
      const disposal = new Promise(resolve => { releaseMounts = resolve; });
      const mounted = new Promise(resolve => { enteredMounts = resolve; });
      connected = true; closing = false;
      mounts = { async close() { order.push("dispose-start"); enteredMounts(); await disposal; order.push("dispose-done"); } };
      worker = { terminate() { order.push("terminate"); } };
      connection = { mounts, pending: new Map(), worker, closing: false };
      for (const pane of panes.values()) pane.flush = () => drafts;
      call = async method => { order.push(method); return {active_requests:0,pending_timers:0,active_alarms:0}; };
      const draining = disconnectDocument();
      await Promise.resolve(); const beforeDrafts = [...order];
      releaseDrafts(); await mounted; const beforeDisposal = [...order];
      releaseMounts(); await draining;
      return {beforeDrafts,beforeDisposal,order};
    });
    assert.deepEqual(drainOrder, {beforeDrafts:[],beforeDisposal:["dispose-start"],order:["dispose-start","dispose-done","disconnect","terminate"]});
    const disconnects = await page.evaluate(async () => {
      const results = [];
      for (const phase of ["storage", "disconnect"]) {
        await mounts.close(); mounts = await MountTable.open();
        let terminated = 0, acknowledgements = 0;
        const acknowledged = () => { acknowledgements++; };
        document.addEventListener("rsi-disconnected", acknowledged);
        connected = true; closing = false;
        worker = { terminate() { terminated++; } };
        connection = { mounts, pending: new Map(), worker, closing: false };
        $("login").hidden = true; $("workbench").hidden = false;
        for (const pane of panes.values()) pane.flush = async () => { if (phase === "storage") throw new Error("Draft storage failed"); };
        call = async () => { throw new Error("Disconnect failed"); };
        $("sign-out").click(); await new Promise(resolve => setTimeout(resolve, 0));
        document.removeEventListener("rsi-disconnected", acknowledged);
        results.push({ phase, terminated, acknowledgements, connected,
          login: !$("login").hidden, workbench: !$("workbench").hidden,
          enabled: !$("sign-out").disabled });
      }
      return results;
    });
    assert.deepEqual(disconnects, [
      {phase:"storage",terminated:0,acknowledgements:0,connected:true,login:false,workbench:true,enabled:true},
      {phase:"disconnect",terminated:1,acknowledgements:0,connected:false,login:true,workbench:false,enabled:true},
    ]);

  } finally { await page.close(); }
}
