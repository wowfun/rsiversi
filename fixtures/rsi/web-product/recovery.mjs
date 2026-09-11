import assert from "node:assert/strict";
import { copyFile, mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { chromium, firefox } from "playwright";
import { startService, waitUntil } from "./service.mjs";
import { png } from "./images.mjs";
import { setTimeout as pause } from "node:timers/promises";

const root = resolve(import.meta.dirname, "../../..");
const name = process.env.RSI_WEB_BROWSER ?? "chromium";
assert(["chromium", "firefox"].includes(name));
const report = process.env.RSI_WEB_REPORT;
await mkdir(report, { recursive: true });
const binary=join(report,"rsi");
await copyFile(process.env.RSI_WEB_BINARY ?? join(root,"target/debug/rsi"),binary);
const browser = await (name === "chromium" ? chromium : firefox).launch();
let service, page;
try {
  service = await startService({ binary, assets: process.env.RSI_WEB_ASSETS, report });
  const context = await browser.newContext({ ignoreHTTPSErrors:true, viewport:{width:1440,height:980} });
  await context.addInitScript(() => {
    const transaction = IDBDatabase.prototype.transaction;
    IDBDatabase.prototype.transaction = function (...args) {
      const tx = transaction.apply(this, args);
      if (window.failDraftWrites && this.name === "rsi.composer" && args[1] === "readwrite") queueMicrotask(() => tx.abort());
      return tx;
    };
    const NativeWorker = Worker;
    window.Worker = class extends NativeWorker {
      constructor(...args) {
        super(...args); this.submissions = new Set();
        this.addEventListener("message", event => {
          if (window.dropSubmissionReply && event.data.kind === "reply" && this.submissions.has(event.data.id)) event.stopImmediatePropagation();
        });
      }
      postMessage(message, ...rest) { if (message.method === "dispatch_submission") this.submissions.add(message.id); return super.postMessage(message,...rest); }
    };
    window.savedRecord = async session => {
      const db = await new Promise((resolve,reject) => { const request=indexedDB.open("rsi.composer",2); request.onsuccess=()=>resolve(request.result); request.onerror=()=>reject(request.error); });
      try { return await new Promise((resolve,reject) => { const tx=db.transaction("drafts","readonly"), request=tx.objectStore("drafts").openCursor(); let result; request.onsuccess=()=> { const cursor=request.result; if (!cursor) return; if(cursor.value.key[3]===session) result=cursor.value; else cursor.continue(); }; tx.oncomplete=()=>resolve(result); tx.onabort=()=>reject(tx.error); }); }
      finally { db.close(); }
    };
  });
  page = await context.newPage();
  const receipt = service.register(`${name} saved drafts A`);
  const login = async (tab, token) => {
    if(token) { await tab.locator("#receipt").fill(JSON.stringify(token)); await tab.locator("#connect").click(); }
    else await tab.locator("#reconnect").click();
    await tab.locator("#workbench").waitFor({state:"visible"});
  };
  const connect = async (tab, token) => { await tab.goto(service.origin); await login(tab, token); };
  const pane = tab => tab.getByRole("region", {name:"Main conversation",exact:true});
  const input = tab => pane(tab).getByRole("textbox",{name:/message$/});
  const restore = async (tab, session) => {
    await pane(tab).getByRole("button",{name:"Restore drafts",exact:true}).click();
    const row=pane(tab).locator(".saved-draft").filter({hasText:session});
    await row.getByRole("button",{name:"Open saved conversation",exact:true}).click();
    return row;
  };
  const sessionId = async tab => (await pane(tab).locator(".pane-session").innerText()).split(" · ").at(-1);
  const waitRecord = async (tab, session, predicate) => {
    const deadline=Date.now()+30_000;
    while(Date.now()<deadline) { const record=await tab.evaluate(session=>window.savedRecord(session),session); if(predicate(record)) return record; await pause(25); }
    throw new Error("Saved record did not reach its expected committed state");
  };
  const waitText = (tab,session,text) => waitRecord(tab,session,record=>record?.text===text);
  await connect(page,receipt);
  const caller = await page.evaluate(async () => (await (await fetch("/api/v1/connection/caller/1",{method:"POST",headers:{"content-type":"application/json"},body:"{}"})).json()).device_id);
  await page.locator(".workspace-add summary").click();
      await page.locator("#workspace-path").fill(service.workspace);
  await page.getByRole("button",{name:"Add workspace",exact:true}).click();
  await page.locator("#workspaces .nav-item").first().click();
  await input(page).waitFor({state:"visible"});
  await input(page).fill("Saved before the first message");
  const fresh = await sessionId(page);
  await pane(page).locator('input[type="file"]').setInputFiles({name:"saved.png",mimeType:"image/png",buffer:png(18,12,[30,110,140,255])});
  await waitRecord(page,fresh,record=>record?.images.length===1);
  await pane(page).locator(".draft-image").waitFor();
  await waitText(page,fresh,"Saved before the first message");
  assert.equal((await page.evaluate(session=>window.savedRecord(session),fresh)).images.length,1);
  await writeFile(join(report,"before-reload.json"),JSON.stringify(await page.evaluate(session=>window.savedRecord(session),fresh),null,2));
  await page.reload(); await page.locator("#reconnect").click(); await page.locator("#workbench").waitFor({state:"visible"});
  await restore(page,fresh);
  await input(page).filter({visible:true}).waitFor();
  await page.waitForFunction(() => document.querySelector('[aria-label="Main conversation"] textarea')?.value==="Saved before the first message");
  assert.equal(await sessionId(page),fresh);
  await writeFile(join(report,"after-reload.json"),JSON.stringify(await page.evaluate(session=>window.savedRecord(session),fresh),null,2));
  await pane(page).getByRole("button",{name:"Preview image",exact:true}).click();
  await page.waitForFunction(()=>document.querySelector(".image-preview")?.naturalWidth===18);
  await page.getByRole("button",{name:"Close details",exact:true}).click();
  const other = await context.newPage(); await connect(other); await restore(other,fresh);
  await other.waitForFunction(()=>document.querySelector('[aria-label="Main conversation"] textarea')?.value==="Saved before the first message");
  await input(page).fill("Saved in the first tab"); await waitText(page,fresh,"Saved in the first tab");
  await input(other).fill("Unsaved in the second tab");
  await pane(other).getByText(/Input is not saved/).waitFor();
  assert(await pane(other).getByRole("button",{name:"Send ↗",exact:true}).isDisabled());
  await other.screenshot({path:join(report,"conflict-desktop.png")});
  await other.setViewportSize({width:390,height:844}); await other.screenshot({path:join(report,"conflict-narrow.png"),fullPage:true});
  const resolveButton=pane(other).getByRole("button",{name:"Use saved input",exact:true});
  await resolveButton.scrollIntoViewIfNeeded();
  assert(await resolveButton.evaluate(button => {const rect=button.getBoundingClientRect();return rect.top>=0&&rect.bottom<=innerHeight;}));
  await other.screenshot({path:join(report,"conflict-narrow-actions.png")});
  await pane(other).getByRole("button",{name:"Use saved input",exact:true}).click();
  await other.waitForFunction(()=>document.querySelector('[aria-label="Main conversation"] textarea')?.value==="Saved in the first tab");
  await page.evaluate(()=>{window.failDraftWrites=true;});
  await input(page).fill("Local-only input after a failed save");
  await pane(page).getByText(/Input is not saved/).waitFor();
  await page.evaluate(()=>{window.failDraftWrites=false;});
  await other.getByRole("button",{name:"Sign out",exact:true}).click();
  await other.locator("#login").waitFor({state:"visible"}); await other.close();
  // A cookie switch cannot authorize an old tab as the new device.
  const deviceB = await context.newPage(); await connect(deviceB,service.register(`${name} saved drafts B`));
  assert.equal(await page.evaluate(async expected => (await fetch("/api/v1/connection/caller/1",{method:"POST",headers:{"content-type":"application/json","X-Rsi-Expected-Device":expected},body:"{}"})).status,caller),401);
  await pane(deviceB).getByRole("button",{name:"Restore drafts",exact:true}).click();
  assert.equal(await pane(deviceB).locator(".saved-draft").count(),0);
  await deviceB.getByRole("button",{name:"Sign out",exact:true}).click(); await deviceB.locator("#login").waitFor({state:"visible"}); await deviceB.close();
  // Restart the real Service: the old Fresh Session actually ceases to exist.
  await service.restart(); await page.locator("#login").waitFor({state:"visible"}); await login(page,receipt);
  const expired=await restore(page,fresh);
  await expired.getByText(/original Session is no longer available/).waitFor();
  await page.screenshot({path:join(report,"expired-fresh.png")});
  assert.equal(await expired.getByRole("textbox",{name:"Unsaved recovered input",exact:true}).inputValue(),"Local-only input after a failed save");
  await page.setViewportSize({width:390,height:844});
  const keepLocal=expired.getByRole("button",{name:"Replace saved text and images",exact:true});
  await keepLocal.scrollIntoViewIfNeeded();
  assert(await keepLocal.evaluate(button=>{
    const rect=button.getBoundingClientRect(), recovery=button.closest(".draft-recovery").getBoundingClientRect();
    const hit=document.elementFromPoint(rect.left+rect.width/2,rect.top+rect.height/2);
    return rect.left>=0 && rect.right<=innerWidth && rect.top>=Math.max(0,recovery.top) && rect.bottom<=Math.min(innerHeight,recovery.bottom) && button.contains(hit);
  }),"expired recovery keeps the local-input choice visible and clickable on narrow screens");
  await page.screenshot({path:join(report,"expired-narrow-actions.png"),fullPage:true});
  await page.setViewportSize({width:1440,height:980});
  await expired.getByRole("button",{name:"Start a new conversation with this draft",exact:true}).click();
  await waitUntil(async () => !(await page.evaluate(session=>window.savedRecord(session),fresh)) || /abort|not saved/i.test(await page.locator("#notice").innerText()),"failed-save recovery result");
  assert.equal((await page.evaluate(session=>window.savedRecord(session),fresh))?.text,"Saved in the first tab","failed-save recovery must retain the old record and its local editor");
  await expired.getByRole("button",{name:"Replace saved text and images",exact:true}).click();
  await waitText(page,fresh,"Local-only input after a failed save");
  await pane(page).getByRole("button",{name:"Close saved drafts",exact:true}).click();
  const resolved=await restore(page,fresh);
  await resolved.getByRole("button",{name:"Start a new conversation with this draft",exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('[aria-label="Main conversation"] textarea')?.value==="Local-only input after a failed save");
  const replacement=await sessionId(page); assert.notEqual(replacement,fresh);
  assert.equal(await pane(page).locator(".draft-image").count(),1);
  assert.equal(service.provider.requests.length,0);
  // Lose only the document reply after the real dispatch. Reload must query the same ID.
  await page.evaluate(()=>{window.dropSubmissionReply=true;});
  await pane(page).getByRole("button",{name:"Send ↗",exact:true}).click();
  await pane(page).locator(".transcript").getByText("Reviewed: Local-only input after a failed save",{exact:false}).waitFor();
  const pending=await page.evaluate(session=>window.savedRecord(session),replacement);
  assert.equal(pending.pending.phase,"dispatching");
  await page.reload(); await page.locator("#reconnect").click(); await page.locator("#workbench").waitFor({state:"visible"});
  await restore(page,replacement);
  await waitRecord(page,replacement,record=>record?.pending===null);
  assert.equal(service.provider.requests.length,1);
  assert.equal(await input(page).inputValue(),"");
  await page.screenshot({path:join(report,"reconciled-durable.png")});
  await page.getByRole("button",{name:"Sign out",exact:true}).click(); await page.locator("#login").waitFor({state:"visible"});
  // Opening corrupt storage must report degradation before any composer is opened.
  await page.evaluate(async () => {
    const db=await new Promise((resolve,reject)=>{const request=indexedDB.open("rsi.composer",2);request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});
    try { await new Promise((resolve,reject)=>{const tx=db.transaction("usage","readwrite");tx.objectStore("usage").put([0,0,0,0],0);tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error);}); }
    finally {db.close();}
  });
  await login(page,receipt);
  assert.match(await page.locator("#notice").innerText(),/Draft storage is unavailable/);
  await page.screenshot({path:join(report,"storage-unavailable.png")});
  await page.setViewportSize({width:390,height:844});
  assert(await page.locator("#notice").evaluate(notice=>{
    const rect=notice.getBoundingClientRect();
    return rect.left>=0 && rect.right<=innerWidth && rect.top>=0 && rect.bottom<=innerHeight;
  }),"storage warning remains visible on narrow screens");
  await page.screenshot({path:join(report,"storage-unavailable-narrow.png"),fullPage:true});
  await page.getByRole("button",{name:"Sign out",exact:true}).click(); await page.locator("#login").waitFor({state:"visible"});
  const result={status:"passed",browser:browser.version(),freshReattached:fresh,freshRecreated:replacement,originalMessage:pending.pending.id,providerRequests:1,canonicalImages:1,cases:["reload live Fresh","canonical image restoration","two-tab conflict UI","device cookie fence and namespace isolation","real Service restart expires Fresh","failed-save local input survives expired recovery and explicit conflict resolution","explicit atomic recreation","lost reply reload queries original message without replay","storage failure is visible immediately after connection"]};
  await writeFile(join(report,"result.json"),JSON.stringify(result,null,2)); console.log(JSON.stringify(result));
} catch(error) {
  if(page&&!page.isClosed()) { await page.screenshot({path:join(report,"failure.png"),fullPage:true}); await writeFile(join(report,"failure.txt"),`${error.stack}\n${await page.locator("body").innerText()}`); }
  throw error;
} finally { await browser.close(); await service?.close(); }
