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
  const errors = []; page.on("pageerror", error => errors.push(error.message));
  try {
    const document = await readFile(join(root, "fixtures/rsi/web-product/document-island.html"), "utf8");
    const standard = await readFile(join(root, "apps/web/standard.js"), "utf8");
    await page.route("http://rsi-dom.invalid/**", route => route.fulfill({ contentType: route.request().url().endsWith("standard.js") ? "text/javascript" : "text/html", body: route.request().url().endsWith("standard.js") ? standard : document.replace(/<script[^>]*>[\s\S]*?<\/script>/g, "") }));
    const admissionSource = await readFile(join(root, "apps/web/admission.js"), "utf8");
    await page.route("http://rsi-dom.invalid/admission.js", route => route.fulfill({ contentType: "text/javascript", body: admissionSource }));
    await page.goto("http://rsi-dom.invalid/");
    await page.evaluate(async () => { Object.assign(globalThis, await import("/admission.js")); });
    const offer = { revision: "a".repeat(64), catalog: { format: 1, renderers: [{ id: "rsi.standard", abi: 1, entry: "standard.js", files: [{ name: "standard.js", sha256: createHash("sha256").update(standard).digest("hex") }], schemas: [{ name: "rsi.standard.view", version: 1 }], capabilities: ["invoke", "focus"], surfaces: ["dialog"] }] } };
    await page.evaluate(offer => { window.testRendererOffer = offer; }, offer);
    await page.addStyleTag({ path: join(root, "apps/web/styles.css") });
    // Classic exposure is confined to this document-only fixture; production uses ESM.
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/mounts.js"), "utf8")).replaceAll("export class ", "class ").replaceAll("export async function ", "async function ").replaceAll("export function ", "function ")} Object.assign(globalThis,{MountTable,writeClipboard,setClipboardWriter}); })();` });
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/drafts.js"), "utf8")).replaceAll("export class ", "class ").replaceAll("export function ", "function ")} Object.assign(globalThis, {DraftStore, DraftEditor, validateEditor}); })();` });
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/settings-form.js"), "utf8")).replaceAll("export function ", "function ")} Object.assign(globalThis, {settingsForm, canUseSettingsForm}); })();` });
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/file-picker.js"), "utf8")).replace("export function ", "function ")} globalThis.openFilePicker = openFilePicker; })();` });
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/external-pane.js"), "utf8")).replace(/^import .*;\n/gm, "").replace("export function ", "function ")} globalThis.externalPaneClass = externalPaneClass; })();` });
    for(const [file,names] of [['turn-presentation.js','TurnPresentation,readingPosition,readingAnchor,restoreAnchor'],['composer-actions.js','keyboardAction']]) {
      await page.addScriptTag({content:`(()=>{${(await readFile(join(root,'apps/web',file),'utf8')).replaceAll('export class ','class ').replaceAll('export function ','function ')}Object.assign(globalThis,{${names}});})();`});
    }
    await page.addScriptTag({ content: `(() => { ${(await readFile(join(root, "apps/web/document-connection.js"), "utf8")).replace(/^import .*;\n/gm, "").replace("export class ", "class ")} globalThis.DocumentConnection = DocumentConnection; })();` });
    await page.addScriptTag({ content: 'let fixtureMounts; const resourceHosts=new Map();function publishResources(){}function clearResources(){resourceHosts.clear()}function renderDetailAtFixture(value){return renderDetail(value,modalTarget)}function renderUiDetailAtFixture(value){return renderUiDetail(value,modalTarget)}const presentationIdentity = {set(){}};function presentationKey(){return "fixture"} function publish() {} let fixtureActions; function installActions(actions) {fixtureActions=actions} function selectSurface() {}\n' + (await readFile(join(root, "apps/web/app.js"), "utf8")).replace(/^import .*;\n/gm, "").replace('export function initialize() {\n', '').replace(/\n}\s*$/, '') });
    assert.deepEqual(errors, [], "document bootstrap has no uncaught errors");
    await page.evaluate(async () => {
      fixtureMounts = await MountTable.open();
      const transport = {terminate(){}, postMessage(message){
        queueMicrotask(() => this.onmessage({data: message.method === "connect"
          ? {kind:"reply",id:message.id,result:'{}'}
          : {kind:"reply",id:message.id,error:"No document transport in projection fixture"}}));
      }};
      connection = createDocumentConnection(transport, fixtureMounts);
      await connection.authenticate({}, async () => ({drafts: await DraftStore.open("a".repeat(32), {kind:"device",device_id:"b".repeat(32)})}));
      for (const key of ["main", "compare"]) panes.set(key, new Pane(key));
      // These are projection fixtures: give each synthetic attachment valid metadata.
      for (const pane of panes.values()) {
        pane.composerActions={revision:"1",primary:{id:"queue",label:"Send"},alternative:{id:"steer",label:"Steer"}};
        const render = pane.render.bind(pane);
        pane.render = (data, models) => render(data ? { header: "c".repeat(64), creation: null, ...data } : data, models);
      }
    });
    await page.evaluate(() => renderDetailAtFixture({settings:{ticket:"numeric-switch", namespace:"fixture.numbers", text:'{"limit":2}', description:{writable:true, defaults:{limit:2}, metadata:{description:"Exact numeric input", applies:"live", sensitive_fields:[], schema:{type:"object", properties:{limit:{type:"number"}}, required:["limit"]}}}}}));
    await page.getByLabel("Settings / limit", {exact:true}).fill("1.0000000000000001");
    await page.getByRole("button", {name:"Save settings", exact:true}).click();
    await page.locator(".settings-field-error").filter({hasText:"exact number representation"}).waitFor();
    await page.getByRole("button", {name:"Edit as JSON", exact:true}).click();
    assert.equal(await page.locator(".settings-field-error").innerText(), "");
    await page.getByLabel("Settings JSON", {exact:true}).waitFor({state:"visible", timeout:2000});
    assert.match(await page.getByLabel("Settings JSON", {exact:true}).inputValue(), /1\.0000000000000001/);
    await page.screenshot({path:join(report, `${name}-settings-exact-json.png`)});
    await page.evaluate(() => {document.querySelector("#detail").close(); dialogKey=undefined;});
    const completionIdentity = await page.evaluate(() => {
      const pane=panes.get("main");
      pane.generation="completion-proof";
      pane.completionState={generation:pane.generation,sequence:"1",query:"he",text:"/he",cursor:3,selected:0};
      const data={completions:{sequence:"1",query:"he",entries:[{replacement:"/help",description:"Help",group:"command"}],notice:""}};
      pane.renderCompletions(data);
      const option=pane.completionPanel.querySelector(".completion-option"),refresh=[...pane.completionPanel.querySelectorAll("button")].at(-1);
      for(let i=0;i<40;i++)pane.renderCompletions(structuredClone(data));
      const retained=option===pane.completionPanel.querySelector(".completion-option") && refresh===[...pane.completionPanel.querySelectorAll("button")].at(-1);
      pane.completionFailure="Request failed";pane.renderCompletions(data);
      const errorUpdates=option!==pane.completionPanel.querySelector(".completion-option");
      pane.closeCompletions();pane.completionState={generation:pane.generation,sequence:"1",query:"he",text:"/he",cursor:3,selected:0};pane.renderCompletions(data);
      const reopened=!pane.completionPanel.hidden;pane.closeCompletions();pane.generation=undefined;
      return {retained,errorUpdates,reopened};
    });
    assert.deepEqual(completionIdentity,{retained:true,errorUpdates:true,reopened:true});
    const retainedHistory = await page.evaluate(() => {
      const pane=panes.get("main"), transcript={omitted:true,blocks:[{key:"retained",role:"assistant",title:"Assistant",text:"Retained reply",sources:8}]};
      pane.renderTranscript(transcript,false);
      const observer=new MutationObserver(()=>{});observer.observe(pane.transcript,{childList:true});
      for(let i=0;i<40;i++)pane.renderTranscript(structuredClone(transcript),false);
      const moves=observer.takeRecords().length;observer.disconnect();pane.renderTranscript({omitted:false,blocks:[]},false);
      return moves;
    });
    assert.equal(retainedHistory,0,"unchanged omitted history must not detach and reinsert its controls");
    const turnIdentity = await page.evaluate(() => {
      const pane=panes.get('main');
      document.documentElement.dataset.detail='standard';
      const transcript={omitted:false,blocks:[
        {key:'process',role:'reasoning',title:'Thinking',text:'Process\n'.repeat(60),sources:1},
        {key:'answer',role:'assistant',title:'Assistant',text:'Answer\n'.repeat(80),sources:1}],
        turns:{revision:'1',entries:{t:{id:'t',status:'Running',running:true,foldable:true,partial:false,blocks:['process','answer'],process:['process'],candidate:['answer'],answer:[]}}}};
      pane.transcript.style.cssText='height:200px;max-height:200px;flex:none;overflow:auto';
      pane.renderTranscript(transcript,true);
      const nodes=[...pane.blocks.values()].map(entry=>entry.node);
      const observer=new MutationObserver(()=>{});observer.observe(pane.transcript,{childList:true});
      for(let i=0;i<20;i++)pane.renderTranscript(structuredClone(transcript),false);
      const unchangedMoves=observer.takeRecords().length;observer.disconnect();
      const answer=nodes[1];pane.transcript.scrollTop=answer.offsetTop+45;pane.readingSummary=true;
      const before=answer.getBoundingClientRect().top;
      Object.assign(transcript.turns.entries.t,{status:'Completed',running:false,candidate:[],answer:['answer']});transcript.turns.revision='2';
      pane.renderTranscript(transcript,false);
      const anchorDelta=Math.abs(answer.getBoundingClientRect().top-before);
      const retained=nodes.every((node,i)=>node===[...pane.blocks.values()][i].node);
      const collapsed=nodes[0].hidden&&!answer.hidden;
      pane.renderTranscript({omitted:false,blocks:[],turns:{revision:'3',entries:{}}},false);
      const retired=!pane.turnPresentation.rows.size&&!pane.turnPresentation.expanded.size;
      pane.transcript.style.cssText='';pane.readingSummary=false;
      return {unchangedMoves,anchorDelta,retained,collapsed,retired};
    });
    // Scroll offsets are rounded by the engine while block geometry remains fractional.
    assert(turnIdentity.anchorDelta<1,`Turn collapse moved the reading anchor: ${turnIdentity.anchorDelta}`);
    assert.deepEqual({...turnIdentity,anchorDelta:0},{unchangedMoves:0,anchorDelta:0,retained:true,collapsed:true,retired:true});
    await writeFile(join(report,`${name}-turn-identity.json`),JSON.stringify(turnIdentity));
    const transcriptScroll = await page.evaluate(() => {
      const pane=panes.get("main"), transcript={omitted:true,blocks:[{key:"scroll",role:"assistant",title:"Assistant",text:"Retained line\n".repeat(100),sources:8}]};
      pane.transcript.style.cssText="height:200px;max-height:200px;flex:none;overflow:auto";
      pane.renderTranscript(transcript,true);
      const bottom=()=>pane.transcript.scrollHeight-pane.transcript.clientHeight;
      if(bottom()<=80)throw new Error("fixture must have a scrollable transcript");
      pane.transcript.scrollTop=bottom()-40;
      const before=pane.transcript.scrollTop;
      for(let i=0;i<10;i++)pane.renderTranscript(structuredClone(transcript),false);
      const unchanged=pane.transcript.scrollTop;
      transcript.blocks[0].text+="New streamed content\n";
      pane.renderTranscript(transcript,false);
      const followsContent=pane.transcript.scrollTop===bottom();
      pane.renderTranscript({omitted:false,blocks:[]},false);pane.transcript.style.cssText="";
      return {before,unchanged,followsContent};
    });
    assert.equal(transcriptScroll.unchanged,transcriptScroll.before,"unchanged frames must not move a near-tail click target");
    assert(transcriptScroll.followsContent,"new streamed text must still follow the tail");
    const resourceIdentity = await page.evaluate(() => {
      const pane = panes.get("main");
      const original = JSON.stringify;
      let deepComparisons = 0;
      JSON.stringify = function(value, ...rest) {
        if (Array.isArray(value) && value.length === 2 && (value[1]?.value?.kind === "read" || Array.isArray(value[1]))) deepComparisons++;
        return original.call(this, value, ...rest);
      };
      try {
        const data = { generation: "preview", resource_revision: "1", resource: { value: { kind: "read", resource: { name: "Review guide", source: "Fixture" }, text: "预览正文\n".repeat(20000) } } };
        pane.renderResourcePreview(data);
        const preview = pane.resourcePreview.lastElementChild;
        for (let i = 0; i < 40; i++) pane.renderResourcePreview({ ...data, resource: { ...data.resource } });
        const retained = preview === pane.resourcePreview.lastElementChild;
        pane.renderResourcePreview({ ...data, resource_revision: "2", resource: { value: { ...data.resource.value, text: "replacement" } } });
        const replaced = preview !== pane.resourcePreview.lastElementChild && pane.resourcePreview.textContent.includes("replacement");
        pane.renderResourcePreview({ ...data, resource_revision: "3", resource: null });
        const closed = pane.resourcePreview.hidden && !pane.resourcePreview.children.length;
        const saved = pane.editor;
        const reference = { snapshot: { sha256: "a".repeat(64), byte_len: 900 }, preview: "Reference", metadata: { source: {kind: "native", binding: {session_id: "source", header_sha256: "b".repeat(64)}}, target: {session_id: "target", header_sha256: "c".repeat(64)}, capture: {kind: "suffix", interval: {through_seq: "1", scanned_after_seq: "0", retained_after_seq: "0", retained_through_seq: "1", fact_prefix_sha256: "d".repeat(64), scanned_bytes: 100, omissions: []}}, text_bytes: 9 } };
        validateEditor("", [], [reference]);
        pane.editor = { referencesRevision: 1, references: [reference] };
        pane.renderReferences();
        const row = pane.referenceList.firstElementChild;
        for (let i = 0; i < 40; i++) pane.renderReferences();
        const referencesRetained = row === pane.referenceList.firstElementChild;
        pane.editor.references = []; pane.editor.referencesRevision++;
        pane.renderReferences();
        const referencesClosed = pane.referenceList.hidden && !pane.referenceList.children.length;
        pane.editor = saved;
        return { retained, replaced, closed, referencesRetained, referencesClosed, deepComparisons };
      } finally { JSON.stringify = original; }
    });
    assert.deepEqual(resourceIdentity, { retained: true, replaced: true, closed: true, referencesRetained: true, referencesClosed: true, deepComparisons: 0 });
    await writeFile(join(report, `${name}-resource-identity.json`), JSON.stringify(resourceIdentity));
    const resetClosesReference = await page.evaluate(() => {
      const pane = panes.get("main");
      pane.generation = "reset-proof";
      pane.editor = { references: [], text: "retained input", images: [] };
      pane.showReferencePicker();
      const dialog = pane.referenceDialog;
      pane.reset();
      return !dialog.open;
    });
    assert.equal(resetClosesReference, true, "Session retirement must close the reference dialog");
    const filePickerErrors=[];const fileError=error=>filePickerErrors.push(error.message);page.on("pageerror",fileError);
    await page.evaluate(() => {
      const pane=panes.get("main");pane.generation="file-error-proof";pane.editor={text:"",references:[],images:[]};pane.input.value="";
      pane.originalReferencePicker=pane.showReferencePicker;
      pane.showReferencePicker=()=>{throw new Error("Reference picker unavailable in fixture")};
      openFilePicker(pane,async()=>{throw new Error("File fixture offline")},button);
    });
    await page.getByRole("button",{name:"Reference conversation",exact:true}).click();
    assert.deepEqual(filePickerErrors,[],"picker actions must use the product error handler");
    assert.match(await page.locator("#notice").textContent(),/Reference picker unavailable in fixture/);
    page.off("pageerror",fileError);
    await page.evaluate(()=>{const pane=panes.get("main");pane.showReferencePicker=pane.originalReferencePicker;delete pane.originalReferencePicker;pane.reset();});
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
      const owner=connection, originalRequest=owner.request;
      owner.request = async (_method, payload) => { sent.push(JSON.parse(payload)); };
      try {
      for (const owner of ["parent-session", "child-session"]) {
        renderDetailAtFixture({ detail: { pane: "main", generation: "one", kind: "approval", request: {
          id: "call-1", subject: { session_id: owner }, reason: owner, action: "run tool", review: { owner },
        } } });
      }
      const text = $("detail").textContent;
      [...$("detail").querySelectorAll("button")].find(button => button.textContent === "Allow once").click();
      return { text, sent };
      } finally { owner.request=originalRequest; }
    });
    assert(approvals.text.includes("child-session") && !approvals.text.includes("parent-session"));
    assert.equal(approvals.sent.length, 1);
    assert.equal(approvals.sent[0].owner, "child-session");
    await page.evaluate(() => {$("detail").close();dialogKey=undefined;});
    const protection = await page.evaluate(async () => {
      const pane=panes.get("main");
      pane.render({generation:"protected-projection",session:"protected-session",path:"/workspace",protected:true,
        model:{deployment:"test",model:"model"},capabilities:{terminal:false,goal:false,preset:false,submit:false},
        transcript:{blocks:[],status:"Completed",omitted:false},pending:[],notice:""},[]);
      await pane.binding;
      return {composerHidden:pane.composer.hidden,noticeVisible:!pane.protectedNotice.hidden,
        commandsDisabled:pane.commands.disabled,notice:pane.protectedNotice.textContent};
    });
    assert.equal(protection.composerHidden,true);
    assert.equal(protection.noticeVisible,true);
    assert.equal(protection.commandsDisabled,true);
    assert.match(protection.notice,/Protected investigation/);
    await page.screenshot({path:join(report,`${name}-protected-investigation.png`)});
    const draftEcho = await page.evaluate(async () => {
      const pane = panes.get("main"), originalCall = call;
      const data = { generation: "draft-echo", session: "retained-session", path: "/workspace",
        model: { deployment: "test", model: "model" }, transcript: { blocks: [], status: "Ready", omitted: false }, pending: [], notice: "" };
      let submitted;
      try {
        call = async (method, payload) => {
          if (method === "prepare_submission") { submitted = JSON.parse(payload).text; return JSON.stringify({kind:"message",id:"message-one",opaque:payload,text_bytes:submitted.length,images: 0, references: 0}); }
          return JSON.stringify({status:"complete",receipt:"{}"});
        };
        pane.render(data, []); await pane.binding;
        pane.input.focus(); pane.input.value = "locally persisted draft";
        pane.input.dispatchEvent(new Event("input", { bubbles: true })); await pane.flush();
        pane.send.focus(); pane.render({...data, draft:"obsolete Worker input"}, []);
        const retained = pane.input.value;
        await pane.submit("primary");
        pane.render({...data, draft:"locally persisted draft"}, []);
        return { retained, submitted, cleared: pane.input.value };
      } finally { call = originalCall; }
    });
    assert.deepEqual(draftEcho, { retained: "locally persisted draft", submitted: "locally persisted draft", cleared: "" });
    const pendingDraftFlush = await page.evaluate(async () => {
      const pane = panes.get("main"), store = connection.drafts, ensure = store.ensure, edit = store.edit;
      let release, releaseSave, savingStarted;
      const gate = new Promise(resolve => {release = resolve;}), saveGate = new Promise(resolve => {releaseSave = resolve;});
      const entered = new Promise(resolve => {savingStarted = resolve;});
      const data = {session: "pending-binding", header: "c".repeat(64), creation: null};
      try {
        store.ensure = async (...args) => {await gate; return ensure.apply(store, args);};
        store.edit = async (...args) => {savingStarted(); await saveGate; return edit.apply(store, args);};
        pane.binding = pane.bindDraft(data).then(() => pane.edit("saved after binding 界"));
        let settled = false; const saving = pane.flush().then(() => {settled = true;});
        const unbound = pane.editor === undefined;
        release(); await entered;
        await new Promise(resolve => setTimeout(resolve, 0));
        const waitedForSave = !settled;
        releaseSave(); await saving;
        await pane.editor.flush();
        const record = (await store.list(pane.index)).find(record => record.key[3] === data.session);
        return {unbound, waitedForSave, saved: record?.text, dirty: pane.editor.dirty};
      } finally {store.ensure = ensure; store.edit = edit;}
    });
    assert.deepEqual(pendingDraftFlush, {unbound: true, waitedForSave: true, saved: "saved after binding 界", dirty: false});
    await writeFile(join(report, `${name}-pending-draft-flush.json`), JSON.stringify(pendingDraftFlush));
    const capturedDraftFlush = await page.evaluate(async () => {
      const pane = panes.get("main"), store = connection.drafts, edit = store.edit;
      let releaseOld, releaseNew, savingOld, savingNew;
      const oldGate = new Promise(resolve => {releaseOld = resolve;}), newGate = new Promise(resolve => {releaseNew = resolve;});
      const oldEntered = new Promise(resolve => {savingOld = resolve;}), newEntered = new Promise(resolve => {savingNew = resolve;});
      try {
        store.edit = async (record, text, ...rest) => {
          if (text === "captured draft") {savingOld(); await oldGate;}
          if (text === "later draft") {savingNew(); await newGate;}
          return edit.call(store, record, text, ...rest);
        };
        const old = pane.editor; pane.edit("captured draft"); await oldEntered;
        pane.binding = pane.bindDraft({session: "later-binding", header: "c".repeat(64), creation: null})
          .then(() => pane.edit("later draft"));
        let settled = false; const flush = pane.flush(true).then(() => {settled = true;});
        await newEntered; releaseOld(); await old.flush();
        await new Promise(resolve => setTimeout(resolve, 0));
        const result = {capturedSaved: !old.dirty, completed: settled, laterStillSaving: pane.editor.dirty};
        releaseNew(); await pane.editor.flush(); await flush;
        return result;
      } finally {releaseOld(); releaseNew(); store.edit = edit;}
    });
    assert.deepEqual(capturedDraftFlush, {capturedSaved: true, completed: true, laterStillSaving: true});
    await writeFile(join(report, `${name}-captured-draft-flush.json`), JSON.stringify(capturedDraftFlush));
    const retries = await page.evaluate(async () => {
      const pane = panes.get("main"), originalCall = call;
      const results = [];
      try {
        call = async () => JSON.stringify({status:"complete",receipt:"{}"});
        for (const text of ["original", "edited next draft"]) {
          pane.edit("original"); await pane.flush();
          const editor = pane.editor;
          await editor.update(record => editor.store.freeze(record, {kind:"message",id:`retry-${text.length}`,opaque:"{}",text_bytes:8,images: 0, references: 0}));
          await editor.update(record => editor.store.begin(record));
          if (text !== "original") { pane.edit(text); await pane.flush(); }
          pane.renderComposer();
          const label = pane.send.textContent, steerDisabled = pane.steer.disabled;
          await pane.submit("primary");
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
      renderUiDetailAtFixture({ ticket: "fixture-startup", model: null, binding: null, error: null });
      const loading = document.querySelector("#detail-body").textContent;
      renderUiDetailAtFixture({ ticket: "fixture-startup", model: null, binding: null, error: "Source startup rejected" });
      return { loading, failed: document.querySelector("#detail-body").textContent };
    });
    assert.deepEqual(failedSetup, { loading: "Loading…", failed: "Source startup rejected" });
    const contributed = await page.evaluate(async () => {
      const sent = [];
      const owner=connection, originalRequest=owner.request;
      owner.request = async (method, payload) => {
        const action=JSON.parse(payload);
        if(action.action==="ui_invoke") sent.push(action);
        else if(action.action!=="ui_visible") return originalRequest.call(owner,method,payload);
      };
      try {
      const reference = { application: "ui-nonce", target: "3", contribution: "4", name: "echo" };
      const bound = { reference, actions: { echo: reference }, view: { title: "Addon", elements: [
        { kind: "text", text: "<script>window.addonExecuted = true</script>" },
        { kind: "input", name: "message", label: "Addon text", value: "initial", multiline: true },
        { kind: "button", action: "echo", label: "Apply addon", value: { expected: "original" } },
      ] } };
      const detail = { pane: "main", generation: "one", ticket: "100", binding: bound.reference, error: null, busy: false,
        model: { renderer: "rsi.standard", schema: { name: "rsi.standard.view", version: 1 }, data: null, standard_view: bound.view, actions: [{ name: "echo", title: "Apply addon" }], sources: [] } };
      const show = async detail => { rendererSlots = []; renderDetailAtFixture({ ui_detail: detail }); await fixtureMounts.render(window.testRendererOffer, rendererSlots); };
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
      } finally { owner.request=originalRequest; }
    });
    assert.equal(contributed.busy, true);
    assert.equal(contributed.remaining, "edited 界");
    assert.equal(contributed.scripts, 0); assert.equal(contributed.executed, false);
    assert.match(contributed.text, /<script>/);
    assert.deepEqual(contributed.sent, [{ action: "ui_invoke", ticket: "101",
      name: "echo",
      input: { value: { expected: "original" }, fields: { message: "edited 界" } } }]);
    await page.screenshot({ path: join(report, `${name}-contributed-form-dom.png`) });
    await page.addScriptTag({content:`async function fixtureConnect(table, transport, receive) {
      transport.postMessage = message => {
        if (message.method === "connect") queueMicrotask(()=>transport.onmessage({data:{kind:"reply",id:message.id,result:JSON.stringify({endpoint_id:"a".repeat(32),principal:{kind:"local"}})}}));
        else receive?.(message, transport);
      };
      const current = createDocumentConnection(transport, table);
      connection=current;await current.authenticate({},async()=>({}));return current;
    }`});
    const staleFailure = await page.evaluate(async () => {
      await fixtureMounts.close();
      const NativeWorker = window.Worker, originalPresent = presentFrame, workers=[];
      class StubWorker { terminated = false; constructor(){workers.push(this);} terminate() { this.terminated = true; } postMessage(message) {
        if(message.method === "connect")queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:"{}"}}));
      } }
      let rejectFrame;
      try {
        window.Worker = StubWorker;
        presentFrame = () => new Promise((_, reject) => { rejectFrame = reject; });
        const old = await makeWorker(); await old.authenticate({},async()=>({}));
        const frame = workers[0].onmessage({ data: { kind: "view", view: "{}", assets: "{}" } });
        await Promise.resolve();
        workers[0].onerror({ preventDefault() {} });
        const replacement = await makeWorker(); await replacement.authenticate({},async()=>({}));
        rejectFrame(new Error("late old-render failure")); await frame;
        return { retained: connection === replacement, terminated: workers[1].terminated };
      } finally {
        await connection.fail(new Error("Fixture finished"));
        window.Worker = NativeWorker; presentFrame = originalPresent;
      }
    });
    assert.deepEqual(staleFailure, { retained: true, terminated: false });
    const detailOwners = await page.evaluate(async () => {
      let oldMessage;
      const oldTransport = {terminate(){}};
      const old = await fixtureConnect(await MountTable.open(), oldTransport, message => {oldMessage=message;});
      showDialog("old-detail", "Old detail", element("div", "", "old"));
      const closing = closeDetail();
      showDialog("replacement-detail", "Replacement detail", element("div", "", "new"));
      oldTransport.onmessage({data:{kind:"reply",id:oldMessage.id,result:null}});
      await closing;
      const replacementPreserved = dialogKey === "replacement-detail" && $("detail").open;
      const readOld = boundCall(old); await old.retire();
      const sent = [];
      const fresh = await fixtureConnect(await MountTable.open(), {terminate(){}}, message => {sent.push(message);});
      const preview = element("div"); document.body.append(preview);
      try {
        await previewImage(preview, {id:"old-owner-preview",bytes:1,width:1,height:1}, "old-ticket", readOld);
        return {replacementPreserved, freshRequests:sent.length, rejectedOldRead:preview.textContent.includes("Image unavailable")};
      } finally {preview.remove(); modalTarget.clear(); await fresh.retire();}
    });
    assert.deepEqual(detailOwners, {replacementPreserved:true,freshRequests:0,rejectedOldRead:true});
    const drainReplacement = await page.evaluate(async () => {
      const NativeWorker = window.Worker, workers = [];
      class StubWorker {
        terminated = 0;
        constructor() {workers.push(this);}
        terminate() {this.terminated++;}
        postMessage(message) {
          if (message.method === "connect") queueMicrotask(() => this.onmessage({data: {kind: "reply", id: message.id, result: "{}"}}));
        }
      }
      try {
        window.Worker = StubWorker;
        const old = await makeWorker(); await old.authenticate({}, async () => ({}));
        let rejectSave;
        const closing = old.disconnect(() => new Promise((_, reject) => {rejectSave = reject;}));
        const fresh = await makeWorker(); await fresh.authenticate({}, async () => ({}));
        rejectSave(null); await closing;
        return {oldPhase: old.phase, oldTerminations: workers[0].terminated,
          freshPhase: fresh.phase, freshTerminations: workers[1].terminated};
      } finally {
        await connection.retire(); window.Worker = NativeWorker;
      }
    });
    assert.deepEqual(drainReplacement, {oldPhase: "Closed", oldTerminations: 1, freshPhase: "Ready", freshTerminations: 0});
    const switchingReplacement = await page.evaluate(async () => {
      const NativeWorker = window.Worker;
      const savedFlushes = new Map([...panes.values()].map(pane => [pane, pane.flush]));
      class StubWorker {
        terminate() {}
        postMessage(message) {
          if (message.method === "connect") queueMicrotask(() => this.onmessage({data: {kind: "reply", id: message.id, result: "{}"}}));
        }
      }
      try {
        window.Worker = StubWorker;
        const storage = async () => ({drafts: await DraftStore.open("a".repeat(32), {kind: "local"})});
        const pane = panes.get(selected);
        const data = {generation: "switching-replacement", selection: "unchanged", session: "retained-session", path: "/workspace",
          model: {deployment: "test", model: "model"}, transcript: {blocks: [], status: "Ready", omitted: false}, pending: [], notice: ""};
        view = {surfaces: {[pane.index]: data}, catalog: {models: []}};
        const operations = [];
        for (const action of ["open", "close"]) {
          const old = await makeWorker(); await old.authenticate({}, storage);
          pane.render(data, []); await pane.binding;
          let releaseFlush; pane.flush = () => new Promise(resolve => {releaseFlush = resolve;});
          const selection = pane.selection, generation = pane.generation;
          const opening = (action === "open" ? openInSelected({action: "open", session_id: "undispatched"})
            : fixtureActions.closeSurface(pane.index)).catch(error => error.message);
          await Promise.resolve();
          if (!pane.switching) throw new Error("Navigation must fence the pane while its old command is pending");
          const fresh = await makeWorker(); await fresh.authenticate({}, storage);
          const clearedOnReplacement = pane.switching === false;
          // Another navigation owned by the fresh connection must survive A's late rejection.
          pane.switching = true; releaseFlush(); await opening;
          const freshFencePreserved = pane.switching === true;
          pane.switching = false;
          pane.render(view.surfaces[pane.index], view.catalog.models);
          operations.push({action, clearedOnReplacement, freshFencePreserved,
            inputDisabled: pane.input.disabled, selectionUnchanged: pane.selection === selection,
            generationUnchanged: pane.generation === generation});
        }
        pane.reset(); pane.switching = true; pane.reset();
        return {operations, resetSwitching: pane.switching};
      } finally { for (const [pane, flush] of savedFlushes) pane.flush = flush; await connection.retire(); window.Worker = NativeWorker; }
    });
    assert.deepEqual(switchingReplacement, {operations: ["open", "close"].map(action => ({action,
      clearedOnReplacement: true, freshFencePreserved: true, inputDisabled: false,
      selectionUnchanged: true, generationUnchanged: true})), resetSwitching: false});
    const admission = await page.evaluate(async () => {
      const sent=[],table=await MountTable.open(),transport={terminate(){}};
      const current=await fixtureConnect(table,transport,message=>sent.push(message));
      const ordinary=Array.from({length:8},()=>call("command","{}"));
      const excess=await call("command","{}").then(()=>false,error=>error.notAdmitted===true);
      let release;
      const draining=current.disconnect(()=>new Promise(resolve=>{release=resolve;}));
      const lifecycle=call("disconnect",true);
      const afterClose=await call("command","{}").then(()=>false,error=>error.notAdmitted===true);
      const kinds=sent.map(message=>message.method);
      for(const message of sent)await transport.onmessage({data:{kind:"reply",id:message.id,result:null}});
      await Promise.all([...ordinary,lifecycle]);await current.fail(new Error("Fixture finished"));release();await draining;
      return {excess,afterClose,kinds};
    });
    assert.deepEqual(admission,{excess:true,afterClose:true,kinds:[...Array(8).fill("command"),"disconnect"]});
    const drainOrder = await page.evaluate(async () => {
      const savedFlushes = new Map([...panes.values()].map(pane => [pane, pane.flush]));
      try {
      const order = [];
      let releaseDrafts, releaseMounts, enteredMounts;
      const drafts = new Promise(resolve => { releaseDrafts = resolve; });
      const disposal = new Promise(resolve => { releaseMounts = resolve; });
      const mounted = new Promise(resolve => { enteredMounts = resolve; });
      const table={close(){order.push("dispose-start");enteredMounts();return disposal.then(()=>order.push("dispose-done"));}};
      const transport={terminate(){order.push("terminate");}};
      await fixtureConnect(table,transport,(message,port)=>{order.push(message.method);queueMicrotask(()=>port.onmessage({data:{kind:"reply",id:message.id,result:{active_requests:0,pending_timers:0,active_alarms:0}}}));});
      connected=true;
      for (const pane of panes.values()) pane.flush = () => drafts;
      const draining = disconnectDocument();
      await Promise.resolve(); const beforeDrafts = [...order];
      releaseDrafts(); await mounted; const beforeDisposal = [...order];
      releaseMounts(); await draining;
      return {beforeDrafts,beforeDisposal,order};
      } finally { for (const [pane, flush] of savedFlushes) pane.flush = flush; }
    });
    assert.deepEqual(drainOrder, {beforeDrafts:[],beforeDisposal:["dispose-start"],order:["dispose-start","dispose-done","disconnect","terminate"]});
    const openingClose = await page.evaluate(async () => {
      const savedFlushes = new Map([...panes.values()].map(pane => [pane, pane.flush]));
      try {
      await connection.retire();
      let release,entered,received;
      const storage=new Promise(resolve=>{release=resolve;}),opening=new Promise(resolve=>{entered=resolve;});
      const receipt=new Promise(resolve=>{received=resolve;});
      let flushes=0,disposals=0,terminations=0,disconnects=0;
      const transport={terminate(){terminations++;},postMessage(message){
        if(message.method==="disconnect")disconnects++;
        queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:message.method==="connect"?"{}":{active_requests:0}}}));
      }};
      const owner=createDocumentConnection(transport,{close(){disposals++;}});connection=owner;
      const authentication=owner.authenticate({},()=>{entered();return storage;});await opening;
      for(const pane of panes.values())pane.flush=async()=>{flushes++;};
      document.addEventListener("rsi-disconnected",received,{once:true});
      window.dispatchEvent(new Event("rsi-native-close"));
      const before={phase:owner.phase,flushes,disconnects,terminations};
      release({});await authentication;
      let deadline;
      try {await Promise.race([receipt,new Promise((_,reject)=>{deadline=setTimeout(()=>reject(new Error("Opening native-close receipt missing")),5000);})]);}
      finally {clearTimeout(deadline);document.removeEventListener("rsi-disconnected",received);}
      return {before,phase:owner.phase,flushes,expectedFlushes:panes.size,disposals,terminations,disconnects,connected,label:$("connection-state").textContent};
      } finally { for (const [pane, flush] of savedFlushes) pane.flush = flush; }
    });
    assert.deepEqual(openingClose,{before:{phase:"Opening",flushes:0,disconnects:0,terminations:0},phase:"Closed",flushes:openingClose.expectedFlushes,expectedFlushes:openingClose.expectedFlushes,disposals:1,terminations:1,disconnects:1,connected:false,label:"Disconnected"});
    const closeCancellation = await page.evaluate(async () => {
      const originalFetch=window.fetch,originalTimeout=AbortSignal.timeout,results=[];
      try {
        for(const failure of ["network","response","deadline"]) {
          const current=await fixtureConnect(await MountTable.open(),{terminate(){}},()=>{});
          if(failure==="deadline") AbortSignal.timeout=milliseconds=>{
            if(milliseconds!==30_000)throw new Error("unexpected cancellation deadline");
            const controller=new AbortController();queueMicrotask(()=>controller.abort(new Error("fixture close cancellation deadline")));return controller.signal;
          };
          window.fetch=async (path,options)=>{
            if(path!=="/_close_cancel")throw new Error("unexpected native path");
            if(failure==="deadline") {
              if(options.signal.aborted)throw options.signal.reason;
              return new Promise((_,reject)=>options.signal.addEventListener("abort",()=>reject(options.signal.reason),{once:true}));
            }
            if(failure==="network")throw new Error("fixture offline");
            return new Response("refused",{status:503});
          };
          const accepted=await cancelNativeCloseAfterDraftFailure(current,new Error("fixture unsaved text"));
          await current.settled();
          results.push({accepted,phase:current.phase,notice:$("notice").textContent});
        }
      } finally {window.fetch=originalFetch;AbortSignal.timeout=originalTimeout;}
      return results;
    });
    for(const result of closeCancellation) {
      assert.equal(result.accepted,false);assert.equal(result.phase,"Failed");
      assert.match(result.notice,/Draft is not saved: fixture unsaved text/);
      assert.match(result.notice,/Could not cancel native close/);
      assert.match(result.notice,/Recover the draft before closing/);
    }
    const exportReplacement = await page.evaluate(async () => {
      const main = panes.get("main"), calls = {old:[],fresh:[]};
      let entered, release;
      const started = new Promise(resolve => entered=resolve);
      const gate = new Promise(resolve => release=resolve);
      const owner = async name => fixtureConnect(await MountTable.open(), {terminate(){}}, (message,port) => {
        calls[name].push(message.method);
        queueMicrotask(()=>port.onmessage({data:{kind:"reply",id:message.id,result:"{}"}}));
      });
      const old = await owner("old");
      main.generation="export-owner"; main.editor ??= {text:""};
      // The download adapter drives two requests across an explicit replacement barrier.
      globalThis.downloadSession = async invoke => {
        await invoke("export_open", ""); entered(); await gate;
        return invoke("export_save", "{}");
      };
      const exporting = main.exportSession("").then(()=>null, error=>error.message);
      await started;
      await old.fail(new Error("Export owner retired"));
      const fresh = await owner("fresh");
      release();
      const error = await exporting;
      const result={calls,error,phase:fresh.phase,retained:connection===fresh};
      delete globalThis.downloadSession;
      await fresh.retire();
      return result;
    });
    assert.deepEqual(exportReplacement.calls,{old:["export_open"],fresh:[]},"export continuations never dispatch through a replacement");
    assert.equal(typeof exportReplacement.error,"string");
    assert.equal(exportReplacement.phase,"Ready");
    assert.equal(exportReplacement.retained,true);

    const falsyDisconnects = await page.evaluate(async () => {
      const results = [];
      for (const receipt of [null, 0, ""]) {
        let terminated = 0, received = 0;
        const table = await MountTable.open();
        const owner = await fixtureConnect(table, {terminate(){terminated++;}}, (message, port) => {
          queueMicrotask(() => port.onmessage({data:{kind:"reply", id:message.id, result:receipt}}));
        });
        const acknowledged = () => {received++;};
        document.addEventListener("rsi-disconnected", acknowledged);
        connected = true;
        $("connection-state").textContent = "Connected";
        $("login").hidden = true; $("workbench").hidden = false;
        for (const pane of panes.values()) pane.flush = async () => {};
        await disconnectDocument();
        document.removeEventListener("rsi-disconnected", acknowledged);
        results.push({receipt, phase:owner.phase, terminated, received, connected,
          label:$("connection-state").textContent, login:!$("login").hidden,
          workbench:!$("workbench").hidden});
      }
      return results;
    });
    assert.deepEqual(falsyDisconnects, [null, 0, ""].map(receipt => ({receipt,
      phase:"Closed", terminated:1, received:1, connected:false,
      label:"Disconnected", login:true, workbench:false})));
    const disconnects = await page.evaluate(async () => {
      const results = [];
      for (const phase of ["storage", "disconnect"]) {
        const table=await MountTable.open();
        let terminated = 0, acknowledgements = 0;
        const acknowledged = () => { acknowledgements++; };
        document.addEventListener("rsi-disconnected", acknowledged);
        await fixtureConnect(table,{terminate(){terminated++;}},()=>{throw new Error("Disconnect failed");});
        connected=true;
        $("login").hidden = true; $("workbench").hidden = false;
        for (const pane of panes.values()) pane.flush = async () => { if (phase === "storage") throw new Error("Draft storage failed"); };
        $("sign-out").click(); await new Promise(resolve => setTimeout(resolve, 0));
        document.removeEventListener("rsi-disconnected", acknowledged);
        results.push({ phase, terminated, acknowledgements, connected,
          login: !$("login").hidden, workbench: !$("workbench").hidden,
          enabled: !$("sign-out").disabled });
        await connection.fail(new Error("Fixture finished"));
      }
      return results;
    });
    assert.deepEqual(disconnects, [
      {phase:"storage",terminated:0,acknowledgements:0,connected:true,login:false,workbench:true,enabled:true},
      {phase:"disconnect",terminated:1,acknowledgements:0,connected:false,login:true,workbench:false,enabled:true},
    ]);
    const reconnectDuringSave = await page.evaluate(async () => {
      const NativeWorker=window.Worker,workers=[],results=[];
      class StubWorker {terminated=0;calls=[];constructor(){workers.push(this);}terminate(){this.terminated++;}postMessage(message){
        this.calls.push(message.method);
        if(message.method==="connect")queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:JSON.stringify({endpoint_id:"a".repeat(32),principal:{kind:"local"}})}}));
      }}
      try {
        window.Worker=StubWorker;
        for(const failure of [false,true]) {
          await connection?.fail(new Error("Fixture setup"));await connectWith("{}");
          const old=connection,transport=workers.at(-1);let finish;
          const save=new Promise((resolve,reject)=>{finish=()=>failure?reject(new Error("old save failed")):resolve();});
          for(const pane of panes.values())pane.flush=()=>save;
          const closing=disconnectDocument();transport.onerror({preventDefault(){}});
          await connectWith("{}");const fresh=connection,newTransport=workers.at(-1);
          finish();await closing;
          results.push({failure,oldPhase:old.phase,newPhase:fresh.phase,retained:connection===fresh,
            newTerminations:newTransport.terminated,newDisconnects:newTransport.calls.filter(method=>method==="disconnect").length,
            connected,label:$("connection-state").textContent,login:!$("login").hidden,signOutEnabled:!$("sign-out").disabled,staleMessages:document.querySelectorAll(".message-text").length});
        }
      } finally {window.Worker=NativeWorker;}
      return results;
    });
    assert.deepEqual(reconnectDuringSave,[false,true].map(failure=>({failure,oldPhase:"Failed",newPhase:"Ready",retained:true,newTerminations:0,newDisconnects:0,connected:true,label:"Connected",login:false,signOutEnabled:true,staleMessages:0})));
    await page.screenshot({path:join(report,`${name}-connection-reconnect.png`)});
    const failedTeardownReplacement = await page.evaluate(async () => {
      await connection.retire();
      const NativeWorker=window.Worker;let creations=0;
      const owner=createDocumentConnection({terminate(){return Promise.reject(new Error("Fixture teardown failed"));},postMessage(message){
        queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:"{}"}}));
      }},{close(){}});connection=owner;
      await owner.authenticate({},()=>({}));
      await owner.fail(new Error("Fixture disconnect" )).catch(()=>{});
      try {
        window.Worker=class {constructor(){creations++;throw new Error("Unexpected replacement");}};
        await connectWith("{}");
        return {creations,retained:connection===owner,phase:owner.phase,notice:$("notice").textContent};
      } finally {window.Worker=NativeWorker;connection=undefined;}
    });
    assert.deepEqual(failedTeardownReplacement,{creations:0,retained:true,phase:"Failed",notice:"Fixture teardown failed"});
    const authenticationFailure = await page.evaluate(async () => {
      await connection?.retire();
      const table=await MountTable.open();let terminated=0,frames=0,release,entered;
      const storage=new Promise((_,reject)=>{release=reject;}),opening=new Promise(resolve=>{entered=resolve;});
      const originalPresent=presentFrame;
      const transport={terminate(){terminated++;},postMessage(message){
        if(message.method==="connect")queueMicrotask(()=>this.onmessage({data:{kind:"reply",id:message.id,result:"{}"}}));
      }};
      const current=createDocumentConnection(transport,table);connection=current;
      try {
        presentFrame=()=>{frames++;return {accepted:true};};
        const authentication=current.authenticate({},()=>{entered();return storage;}).catch(error=>error.message);
        await opening;
        const frame=transport.onmessage({data:{kind:"view",view:{},assets:{}}});
        release("Draft setup fixture failed");
        const message=await authentication;await frame;await current.settled();
        return {message,phase:current.phase,frames,terminated,connected,
          label:$("connection-state").textContent,login:!$("login").hidden,workbench:!$("workbench").hidden};
      } finally {presentFrame=originalPresent;await current.retire();}
    });
    assert.deepEqual(authenticationFailure,{message:"Draft setup fixture failed",phase:"Failed",frames:0,terminated:1,
      connected:false,label:"Connection failed",login:true,workbench:false});
    await page.screenshot({path:join(report,`${name}-authentication-failure.png`)});
    await writeFile(join(report,`${name}-connection-races.json`),JSON.stringify({staleFailure,drainReplacement,switchingReplacement,admission,drainOrder,openingClose,exportReplacement,falsyDisconnects,disconnects,reconnectDuringSave,failedTeardownReplacement,authenticationFailure,errors},null,2));
    assert.deepEqual(errors,[],"connection races leave no uncaught browser errors");

  } finally { await page.close(); }
}
