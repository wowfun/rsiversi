import {navigationMode} from './controls.mjs';
// Two windows share a saved snapshot and keep independent live resource owners.
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
  if(kind==='session-dock'){
    const key=Object.keys(localStorage).find(key=>key.startsWith('rsi.dock.v1.'));
    return key?{value:JSON.parse(localStorage.getItem(key))}:undefined;
  }
  const db=await new Promise((resolve,reject)=>{const r=indexedDB.open('rsi.presentation');r.onsuccess=()=>resolve(r.result);r.onerror=()=>reject(r.error)});
  try{return await new Promise((resolve,reject)=>{const r=db.transaction('layouts').objectStore('layouts').getAll();r.onsuccess=()=>resolve(r.result.find(r=>kind==='shell'?r.value.version===shellVersion:r.value.kind===kind));r.onerror=()=>reject(r.error)})}finally{db.close()}
},{kind,shellVersion:defaultLayout.version})}
async function menu(page,id,action){await page.locator(`[data-dockkit-tab="${id}"]`).click({button:'right'});await page.getByRole('menuitem',{name:action,exact:true}).click()}
async function resumed(page,service,id){await page.goto(service.origin);await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});await navigationMode(page,'flat');await page.locator(`[data-session-id="${id}"] [data-testid=conversation-open]`).click();await page.waitForFunction(id=>document.querySelector('.resource-dock')?.dataset.session===id,id);await resources(page)}
try{
 service=await startService({binary:paired.binary,assets:paired.assets,report});
 browser=await chromium.launch();const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:900}});
 await context.addInitScript(()=>{
  window.dockTrace=[];const Original=Worker;
  window.Worker=class extends Original {constructor(...args){super(...args);this.addEventListener('message',({data})=>{if(data.kind==='view'){const frame=JSON.parse(data.view);window.dockTrace.push({kind:'panels',panels:frame.sections?.panels??frame.view?.panels});}else if(data.kind==='reply')window.dockTrace.push({kind:'reply',id:data.id,error:data.error});if(window.dockTrace.length>200)window.dockTrace.shift()})}postMessage(data,...rest){if(data.kind==='call'&&data.method==='command')window.dockTrace.push({kind:'command',id:data.id,command:JSON.parse(data.payload)});super.postMessage(data,...rest)}};
  const write=Storage.prototype.setItem;
  Storage.prototype.setItem=function(key,value){
    if(key.startsWith('rsi.dock.v1.') && window.failLayoutSave)throw new DOMException('Fixture storage is full','QuotaExceededError');
    return write.call(this,key,value);
  };
 });
 page=await context.newPage();page.setDefaultTimeout(15000);page.on('pageerror',e=>errors.push(e.message));
 await connectWorkbench(page,service,'Layout durability');await openWorkspace(page,service);
 await page.getByLabel('Main message',{exact:true}).fill('Durable resource layout');await page.getByTestId('composer-send').click();await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
 const session=(await page.locator('.pane.selected').getAttribute('data-session-id'));
 await resources(page);
 peer=await context.newPage();peer.setDefaultTimeout(15000);peer.on('pageerror',e=>errors.push(e.message));await resumed(peer,service,session);
 const initialPeer=await tabs(peer).count();
 await page.getByRole('button',{name:'Workspace files',exact:true}).click();
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===1);
 assert.equal(await tabs(peer).count(),initialPeer);
 await menu(page,await tabs(page).first().getAttribute('data-dockkit-tab'),'Duplicate');
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===2);
 await peer.getByRole('button',{name:'Workspace files',exact:true}).click();
 await peer.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===1);
 assert.equal(await tabs(page).count(),2);
 assert.equal(Object.keys((await record(page)).value.state.tabs).length,1);
 checks.push('Independent windows retain their tabs and the most recent save wins');
 await page.locator('[data-dockkit-add-tab]').click();
 await page.locator('[data-dockkit-content]:visible .resource-guide').waitFor();
 const guideToClose=await page.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab');
 await menu(page,guideToClose,'Close resource tab');
 assert.equal(await tabs(peer).count(),1);
 const first=await tabs(page).first().getAttribute('data-dockkit-tab');
 await page.getByRole('button',{name:'Split resources',exact:true}).first().click();
 const guide=page.locator('[data-dockkit-pane]').filter({has:page.locator('.resource-guide')});
 await guide.getByRole('button',{name:'Workspace files',exact:true}).waitFor();
 const guideTab=await guide.locator('[data-dockkit-tab][aria-selected=true]').getAttribute('data-dockkit-tab');
 await page.screenshot({path:join(report,'split-guide.png')});
 const opens=()=>page.evaluate(()=>window.dockTrace.filter(e=>e.kind==='command'&&e.command.action==='ui_surface').length);
 const beforeMove=await opens();
 await page.locator(`[data-dockkit-tab="${first}"]`).dragTo(guide.locator('.resource-guide'));
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-pane] .resource-content').length===2);
 assert.equal(await opens(),beforeMove,'tab drag retains existing resource authority');
 await menu(page,guideTab,'Close resource tab');
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2);
 checks.push('A split opens a resource guide, accepts a dragged tab without reopening it, and permits closing the guide');
 await menu(page,first,'Float');await page.getByRole('button',{name:'Dock resource',exact:true}).waitFor();
 assert.equal(await peer.getByRole('button',{name:'Dock resource',exact:true}).count(),0);
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
 await menu(page,first,'Duplicate');await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===3);
 assert.deepEqual(await record(page),before);
 assert.equal(await page.getByLabel('Main message',{exact:true}).inputValue(),draft);
 await page.evaluate(()=>{window.failLayoutSave=false});
 await page.getByRole('button',{name:'Undo layout',exact:true}).click();
 await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===2);
 await menu(page,first,'Duplicate');await page.waitForFunction(()=>document.querySelectorAll('[data-dockkit-tab]').length===3);
 assert.equal(Object.keys((await record(page)).value.state.tabs).length,3);
 checks.push('A failed snapshot write retains the new tab in the active window and the previous saved layout; later edits persist');
 for(let count=3;count<16;count++){await menu(page,first,'Duplicate');await page.waitForFunction(count=>document.querySelectorAll('[data-dockkit-tab]').length===count,count+1)}
 await page.waitForFunction(()=>document.querySelectorAll('.resource-content .ui-contribution').length===16);
 const full=await record(page);await menu(page,first,'Duplicate');await page.getByRole('alert').filter({hasText:'Invalid resource layout'}).waitFor();
 assert.deepEqual(await record(page),full);assert.equal(await tabs(page).count(),16);
 checks.push('Sixteen independently reopened Files views render; seventeenth tab is rejected atomically');
 assert.equal(await page.getByLabel('Main message',{exact:true}).inputValue(),draft);
 await page.screenshot({path:join(report,'capacity.png')});
 await peer.locator('[data-dockkit-add-tab]').click();
 await peer.getByRole('button',{name:'New terminal',exact:true}).click();
 const consolePanel=p=>p.getByRole('region',{name:'Session terminals'});
 await consolePanel(peer).getByRole('status').filter({hasText:'You have control'}).waitFor();
 assert.equal(await tabs(page).count(),16);
 const follower=await context.newPage();
 try{
   await resumed(follower,service,session);
   await consolePanel(follower).getByRole('status').filter({hasText:'Read only'}).waitFor();
   const terminalTab=await tabs(follower).filter({hasText:'Terminal'}).getAttribute('data-dockkit-tab');
   await menu(follower,terminalTab,'Close resource tab');
   await consolePanel(follower).waitFor({state:'hidden'});
   assert(await consolePanel(peer).getByRole('status').filter({hasText:'You have control'}).isVisible());
   await follower.getByRole('button',{name:'Undo layout',exact:true}).click();
   await consolePanel(follower).getByRole('status').filter({hasText:'Read only'}).waitFor();
   await follower.screenshot({path:join(report,'terminal-follower.png')});
   checks.push('A restored terminal attaches read-only; closing or undoing that tab retains control in the creating window');
 }finally{await follower.close()}
 await writeFile(join(report,'result.json'),JSON.stringify({family:paired.family,checks,errors},null,2));assert.deepEqual(errors,[]);
}catch(error){for(const [name,p]of [['main',page],['peer',peer]])if(p){await p.screenshot({path:join(report,`${name}-failure.png`)}).catch(()=>{});await writeFile(join(report,`${name}-failure.txt`),`${error.stack}\n${await p.locator('body').innerText().catch(()=>'(closed)')}`);await writeFile(join(report,`${name}-trace.json`),JSON.stringify({trace:await p.evaluate(()=>window.dockTrace),record:await record(p)},null,2))}throw error}
finally{await browser?.close();await service?.close()}
