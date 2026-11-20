import {navigationMode} from './controls.mjs';
// Actual public product controls; IndexedDB faults affect only this isolated browser.
import {paired} from './paired-env.mjs';
import {startService,waitUntil} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {resources} from './controls.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {defaultLayout} from '../../../apps/web/presentation-layout.js';
const report=process.env.RSI_REPORT_DIR;assert(report);await mkdir(report,{recursive:false});
let service,browser,page,peer;const checks=[],errors=[];
const tabs=page=>page.locator('[data-dockkit-tab]');
async function record(page,kind="session-dock"){return page.evaluate(async({kind,shellVersion})=>{
  const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('rsi.presentation');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)});
  try{return await new Promise((resolve,reject)=>{const r=db.transaction('layouts').objectStore('layouts').getAll();r.onsuccess=()=>resolve(r.result.find(r=>kind==='shell'?r.value.version===shellVersion:r.value.kind===kind));r.onerror=()=>reject(r.error)})}finally{db.close()}
},{kind,shellVersion:defaultLayout.version})}
async function menu(page,id,action){await page.locator(`[data-dockkit-tab="${id}"]`).click({button:'right'});await page.getByRole('menuitem',{name:action,exact:true}).click()}
// Hold a real competing transaction so an activation cannot be saved during the frame check.
async function holdLayouts(page){
 await page.evaluate(()=>new Promise((resolve,reject)=>{
   const open=indexedDB.open('rsi.presentation');
   open.onerror=()=>reject(open.error);
   open.onsuccess=()=>{
     const db=open.result,tx=db.transaction('layouts','readwrite'),store=tx.objectStore('layouts');
     let held=true;
     window.releaseDockLock=()=>{held=false};
     tx.oncomplete=tx.onabort=()=>db.close();
     const read=()=>{store.get('dock-fixture-lock').onsuccess=()=>{resolve();if(held)read()}};
     read();
   };
 }));
}
async function selectBeforeCommit(page,id){
 return page.locator(`[data-dockkit-tab="${id}"]`).evaluate(async tab=>{
   const id=tab.dataset.dockkitTab;
   tab.click();
   await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
   return document.querySelector(`[data-dockkit-tab="${id}"]`)?.getAttribute('aria-selected')==='true'&&
     document.querySelector(`[data-dockkit-content="${id}"] .ui-contribution`)?.checkVisibility();
 });
}
async function resumed(page,service,id){await page.goto(service.origin);await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});await navigationMode(page,'flat');await page.locator(`[data-session-id="${id}"] [data-testid=conversation-open]`).click();await page.waitForFunction(id=>document.querySelector('.resource-dock')?.dataset.session===id,id);await resources(page)}
try{
 service=await startService({binary:paired.binary,assets:paired.assets,report});
 browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
 await context.addInitScript(()=>{
  window.dockTrace=[];const Original=Worker;
  window.Worker=class extends Original {constructor(...args){super(...args);this.addEventListener('message',({data})=>{if(data.kind==='view'){const frame=JSON.parse(data.view);window.dockTrace.push({kind:'panels',panels:frame.sections?.panels??frame.view?.panels});}else if(data.kind==='reply')window.dockTrace.push({kind:'reply',id:data.id,error:data.error});if(window.dockTrace.length>200)window.dockTrace.shift()})}postMessage(data,...rest){if(data.kind==='call'&&data.method==='command')window.dockTrace.push({kind:'command',id:data.id,command:JSON.parse(data.payload)});super.postMessage(data,...rest)}};
  const transaction=IDBDatabase.prototype.transaction;
  IDBDatabase.prototype.transaction=function(stores,mode,...rest){if(window.failLayoutSave&&this.name==='rsi.presentation'&&mode==='readwrite'&&[stores].flat().includes('layouts'))throw new DOMException('Injected isolated layout write failure','QuotaExceededError');return transaction.call(this,stores,mode,...rest)};
  const put=IDBObjectStore.prototype.put;
  IDBObjectStore.prototype.put=function(value,...rest){
    if(window.failDockPut&&this.name==='layouts'&&value?.value?.kind==='session-dock')throw new DOMException('Failed tab selection','QuotaExceededError');
    return put.call(this,value,...rest);
  };
 });
 page=await context.newPage();page.setDefaultTimeout(15000);page.on('pageerror',e=>errors.push(e.message));
 await connectWorkbench(page,service,'Layout durability');await openWorkspace(page,service);
 await page.getByLabel('Main message',{exact:true}).fill('Durable resource layout');await page.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
 const session=(await page.locator('.pane.selected').getAttribute('data-session-id'));
 await resources(page);
 peer=await context.newPage();peer.setDefaultTimeout(15000);peer.on('pageerror',e=>errors.push(e.message));await resumed(peer,service,session);
 await Promise.all([page,peer].map(p=>p.getByRole('button',{name:'Workspace files',exact:true}).click()));
 for(const p of [page,peer])await p.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2&&document.querySelectorAll('.resource-content .ui-contribution').length===2);
 assert.equal(Object.keys((await record(page)).value.state.tabs).length,2);
 checks.push('Concurrent independent Files opens merge into two tabs in both windows');
 const beforeGuide=await record(page);
 await Promise.all([page,peer].map(p=>p.locator('[data-dockkit-add-tab]').evaluate(button=>button.click())));
 for(const p of [page,peer]){
   await p.locator('[data-dockkit-content]:visible .resource-guide').waitFor();
   assert.equal(await tabs(p).count(),3);
   assert.equal(await p.locator('[data-dockkit-add-tab]').count(),0);
 }
 assert.equal(BigInt((await record(page)).revision),BigInt(beforeGuide.revision)+1n);
 const sharedGuide=await page.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab');
 await menu(page,sharedGuide,'Close resource tab');
 for(const p of [page,peer])await p.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2);
 checks.push('Two windows opening a guide in the same pane produce one tab and one saved revision');
 const original=await page.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab');
 const other=await page.locator('[data-dockkit-tab][aria-selected=false]').getAttribute('data-dockkit-tab');
 const previousRevision=BigInt((await record(page)).revision);
 await holdLayouts(page);
 try{
   assert(await selectBeforeCommit(page,other),'open tab and its panel switch while the save is pending');
   assert(await selectBeforeCommit(page,original),'latest selection is visible while both saves are pending');
 }finally{await page.evaluate(()=>window.releaseDockLock())}
 await waitUntil(async()=>{const row=await record(page);return BigInt(row.revision)===previousRevision+2n&&row.value.state.nodes[row.value.state.activePaneId].activeTabId===original},'latest tab saved');
 await holdLayouts(page);
 try{
   const chip=page.locator(`[data-dockkit-tab="${other}"]`);
   await chip.focus();await chip.press('Enter');
   await page.waitForFunction(id=>document.activeElement?.dataset.dockkitTab===id&&document.activeElement?.getAttribute('aria-selected')==='true',other);
   await page.evaluate(()=>{window.failDockPut=true});
 }finally{await page.evaluate(()=>window.releaseDockLock())}
 await page.getByRole('alert').filter({hasText:'Failed tab selection'}).waitFor();
 assert.equal(await page.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab'),original);
 assert(await page.evaluate(()=>document.activeElement?.hasAttribute('data-dockkit-tab')),'rollback keeps keyboard navigation in the tab strip');
 await page.evaluate(()=>{window.failDockPut=false});
 checks.push('Tab selection displays before commit, preserves the latest input and rolls back a failed selection');
 const beforeFocus=await record(page);
 await page.locator('[data-dockkit-tab][aria-selected=false]').first().evaluate(tab=>{
   // Both activations see the old chip before the first transaction can settle.
   tab.click();tab.click();
 });
 const first=await tabs(page).first().getAttribute('data-dockkit-tab');
 await page.getByRole('button',{name:'Split resources',exact:true}).first().click();
 const guide=page.locator('[data-dockkit-pane]').filter({has:page.locator('.resource-guide')});
 await guide.getByRole('button',{name:'Workspace files',exact:true}).waitFor();
 const guideTab=await guide.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab');
 assert.equal(await page.getByRole('alert').filter({hasText:'Failed tab selection'}).count(),0,'a successful saved action clears the earlier failure');
 await page.screenshot({path:join(report,'split-guide.png')});
 assert.equal(BigInt((await record(page)).revision),BigInt(beforeFocus.revision)+2n,'one selection and one split');
 checks.push('Repeated tab activation while the first selection is pending commits one revision');
 const opens=()=>page.evaluate(()=>window.dockTrace.filter(e=>e.kind==='command'&&e.command.action==='ui_surface').length);
 const beforeMove=await opens();
 await page.locator(`[data-dockkit-tab="${first}"]`).dragTo(guide.locator('.resource-guide'));
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-pane] .resource-content').length===2);
 assert.equal(await opens(),beforeMove,'tab drag retains existing resource authority');
 await menu(page,guideTab,'Close resource tab');
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2);
 checks.push('A split opens a resource guide, accepts a dragged tab without reopening it, and permits closing the guide');
 await menu(page,first,'Float');await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 await peer.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 const desktop=await record(page),shell=await record(page,'shell');
 assert.equal(desktop.value.version,2);assert(!Object.hasOwn(desktop.value,'history'));
 assert.ok(shell,'Shell layout is saved before viewport-only projection checks');
 await page.setViewportSize({width:390,height:844});await page.waitForFunction(()=>document.querySelector('.resource-dock').classList.contains('resource-fullscreen'));await resources(page);await page.locator('[data-dockkit-float]').waitFor({state:'visible'});
 await page.screenshot({path:join(report,'float-390.png')});
 const bounds=await page.locator('[data-dockkit-float]').boundingBox();assert(bounds.x>=0&&bounds.y>=0&&bounds.x+bounds.width<=391&&bounds.y+bounds.height<=845,JSON.stringify(bounds));
 assert.deepEqual((await record(page)).value,desktop.value,'viewport projection never overwrites desktop state');
 await page.getByRole('button',{name:'Hide resources',exact:true}).click();
 assert.deepEqual(await record(page,'shell'),shell,'closing narrow resources preserves desktop visibility');
 await resources(page);
 assert.deepEqual(await record(page,'shell'),shell,'opening narrow resources preserves desktop visibility');
 await page.setViewportSize({width:1440,height:900});
 await page.reload();await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
 await page.locator(`[data-session-id="${session}"] [data-testid=conversation-open]`).click();
 await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===2);
 assert.deepEqual((await record(page)).value,desktop.value);
 assert(await page.getByRole('button',{name:'Undo layout',exact:true}).isDisabled());
 checks.push('Float layout survives reload with a fresh undo history; narrow viewport preserves saved geometry');
 await page.getByRole('button',{name:'Dock resource',exact:true}).click();await tabs(page).filter({hasText:'Files'}).first().waitFor();
 await waitUntil(async()=>(await record(page)).value.state.floats.length===0,'docked layout saved');
 const views=()=>page.evaluate(()=>window.dockTrace.findLast(e=>e.panels)?.panels.map(p=>p.view)??[]);
 const previousViews=await views();assert.equal(previousViews.length,2);
 await menu(page,first,'Close resource tab');await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===1);
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===2);
 assert.equal((await views()).filter(id=>!previousViews.includes(id)).length,1);
 checks.push('Closing and undoing reopens one fresh view authority while retaining its sibling');
 const draft='Unsent draft survives a layout write failure';
 await page.getByLabel('Main message',{exact:true}).fill(draft);
 const before=await record(page);await page.evaluate(()=>{window.failLayoutSave=true});
 await menu(page,first,'Duplicate');await page.getByRole('alert').filter({hasText:'Injected isolated layout write failure'}).waitFor();
 assert.deepEqual(await record(page),before);assert.equal(await tabs(page).count(),2);
 assert.equal(await page.getByLabel('Main message',{exact:true}).inputValue(),draft);
 await page.evaluate(()=>{window.failLayoutSave=false});
 await menu(page,first,'Duplicate');await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===3);
 checks.push('Failed IndexedDB commit reports failure and preserves the previous document; subsequent retry succeeds');
 for(let count=3;count<16;count++){await menu(page,first,'Duplicate');await page.waitForFunction(count=>document.querySelectorAll('[data-dockkit-tab]').length===count,count+1)}
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===16);
 const full=await record(page);await menu(page,first,'Duplicate');await page.getByRole('alert').filter({hasText:'Invalid resource layout'}).waitFor();
 assert.deepEqual(await record(page),full);assert.equal(await tabs(page).count(),16);
 checks.push('Sixteen independently reopened Files views render; seventeenth tab is rejected atomically');
 assert.equal(await page.getByLabel('Main message',{exact:true}).inputValue(),draft);
 await page.screenshot({path:join(report,'capacity.png')});
 await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,checks,errors},null,2));assert.deepEqual(errors,[]);
}catch(error){for(const [name,p]of [['main',page],['peer',peer]])if(p){await p.screenshot({path:join(report,`${name}-failure.png`)}).catch(()=>{});await writeFile(join(report,`${name}-failure.txt`),`${error.stack}\n${await p.locator('body').innerText().catch(()=>'(closed)')}`);await writeFile(join(report,`${name}-trace.json`),JSON.stringify({trace:await p.evaluate(()=>window.dockTrace),record:await record(p)},null,2))}throw error}
finally{await browser?.close();await service?.close()}
