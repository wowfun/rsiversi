// Actual public product controls; IndexedDB faults affect only this isolated browser.
import {paired} from './paired-env.mjs';
import {startService,waitUntil} from './service.mjs';
import {connectWorkbench,openWorkspace} from './browser-fixture.mjs';
import {resources} from './controls.mjs';
import {chromium} from 'playwright';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import assert from 'node:assert/strict';
const report=process.env.RSI_REPORT_DIR;assert(report);await mkdir(report,{recursive:false});
let service,browser,page,peer;const checks=[],errors=[];
const tabs=page=>page.locator('[data-dockkit-tab]');
async function record(page,kind="session-dock"){return page.evaluate(async kind=>{
  const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('rsi.presentation',2);r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)});
  try{return await new Promise((resolve,reject)=>{const r=db.transaction('layouts').objectStore('layouts').getAll();r.onsuccess=()=>resolve(r.result.find(r=>kind==='shell'?r.value.version===3:r.value.kind===kind));r.onerror=()=>reject(r.error)})}finally{db.close()}
},kind)}
async function menu(page,id,action){await page.locator(`[data-dockkit-tab="${id}"]`).click({button:'right'});await page.getByRole('menuitem',{name:action,exact:true}).click()}
async function resumed(page,service,id){await page.goto(service.origin);await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});await page.getByLabel('Navigation view',{exact:true}).selectOption('flat');await page.locator(`[data-session-id="${id}"] [data-testid=conversation-open]`).click();await page.waitForFunction(id=>document.querySelector('.resource-dock')?.dataset.session===id,id);await resources(page)}
try{
 service=await startService({binary:paired.binary,assets:paired.assets,report});
 browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
 await context.addInitScript(()=>{
  window.dockTrace=[];const Original=Worker;
  window.Worker=class extends Original {constructor(...args){super(...args);this.addEventListener('message',({data})=>{if(data.kind==='view'){const frame=JSON.parse(data.view);window.dockTrace.push({kind:'panels',panels:frame.sections?.panels??frame.view?.panels});}else if(data.kind==='reply')window.dockTrace.push({kind:'reply',id:data.id,error:data.error});if(window.dockTrace.length>200)window.dockTrace.shift()})}postMessage(data,...rest){if(data.kind==='call'&&data.method==='command')window.dockTrace.push({kind:'command',id:data.id,command:JSON.parse(data.payload)});super.postMessage(data,...rest)}};
  const transaction=IDBDatabase.prototype.transaction;
  IDBDatabase.prototype.transaction=function(stores,mode,...rest){if(window.failLayoutSave&&this.name==='rsi.presentation'&&mode==='readwrite'&&[stores].flat().includes('layouts'))throw new DOMException('Injected isolated layout write failure','QuotaExceededError');return transaction.call(this,stores,mode,...rest)};
 });
 page=await context.newPage();page.setDefaultTimeout(15000);page.on('pageerror',e=>errors.push(e.message));
 await connectWorkbench(page,service,'Layout durability');await openWorkspace(page,service);
 await page.getByLabel('Main message',{exact:true}).fill('Durable resource layout');await page.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
 const session=(await page.locator('.pane.selected .pane-session').innerText()).split(' · ').at(-1);
 await resources(page);
 peer=await context.newPage();peer.setDefaultTimeout(15000);peer.on('pageerror',e=>errors.push(e.message));await resumed(peer,service,session);
 await Promise.all([page,peer].map(p=>p.getByRole('button',{name:'Workspace files',exact:true}).click()));
 for(const p of [page,peer])await p.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2&&document.querySelectorAll('.resource-content .ui-contribution').length===2);
 assert.equal(Object.keys((await record(page)).value.state.tabs).length,2);
 checks.push('Concurrent independent Files opens merge into two tabs in both windows');
 const first=await tabs(page).first().getAttribute('data-dockkit-tab');
 await page.getByRole('button',{name:'Split resources',exact:true}).first().click();
 await page.locator('[data-dockkit-empty]').waitFor();
 const opens=()=>page.evaluate(()=>window.dockTrace.filter(e=>e.kind==='command'&&e.command.action==='ui_surface').length);
 const beforeMove=await opens();
 await page.locator(`[data-dockkit-tab="${first}"]`).dragTo(page.locator('[data-dockkit-empty]'));
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-pane] .resource-content').length===2);
 assert.equal(await opens(),beforeMove,'tab drag retains existing resource authority');
 checks.push('Real pointer drag moves a tab into the second pane without reopening its resource');
 await menu(page,first,'Float');await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 await peer.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 const desktop=await record(page),shell=await record(page,'shell');
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
 assert.deepEqual((await record(page)).value,desktop.value);checks.push('Float layout survives reload; narrow viewport clamps display without changing saved desktop structure');
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
