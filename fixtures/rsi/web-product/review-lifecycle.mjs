// Real React effect retirement and IndexedDB checks against current source modules.
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {chromium} from 'playwright';
import {createServer} from '../../../apps/web/node_modules/vite/dist/node/index.js';
const root=resolve(import.meta.dirname,'../../../apps/web'),report=process.env.RSI_REPORT_DIR;
assert(report,'RSI_REPORT_DIR must name an isolated report directory');
const entry=join(root,'review-test.tsx');
const code=`
import React from 'react';
import {createRoot} from 'react-dom/client';
import {ResourceDocks} from './src/resource-dock.tsx';
import {DeviceStore} from './device-store.js';
import {emptyDock,dockIntent} from './src/dock-state.ts';
import {presentationIdentity,LayoutContext,useViewport,useNarrow,useResourceVisibility,ResizeHandle} from './src/presentation.tsx';
import {defaultLayout} from './presentation-layout.js';
import {source,installActions} from './src/bridge.ts';
import {resources,resourceHosts,publishResources,clearResources} from './src/resource-hosts.ts';
window.modules={defaultLayout,DeviceStore,emptyDock,dockIntent,resources,resourceHosts,publishResources,clearResources};
window.mountResize=()=>{
 const node=document.createElement('main');node.id='workbench';document.body.append(node);const root=createRoot(node);
 function Resizer(){const viewport=useViewport();const [ratio,setRatio]=React.useState(.45);return <><output>{ratio}</output><ResizeHandle name="resources" value={ratio*viewport.width} min={300} max={viewport.width*.7} reverse onChange={value=>setRatio(value/viewport.width)}/></>}
 root.render(<Resizer/>);return ()=>{root.unmount();node.remove()};
};
window.mountVisibility=()=>{
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);window.visibilityWrites=[];
 function Visibility(){
  const narrow=useNarrow();const [saved,setSaved]=React.useState(defaultLayout);
  const {layout,update}=useResourceVisibility(saved,patch=>{window.visibilityWrites.push(patch);setSaved(value=>({...value,...patch}))},narrow);
  React.useEffect(()=>{window.oldVisibilityCallback=update},[]);
  return <output data-narrow={narrow} data-closed={layout.resourcesClosed} data-saved={saved.resourcesClosed}/>;
 }
 root.render(<Visibility/>);window.unmountVisibility=()=>{root.unmount();node.remove()};
};
window.lifecycle=async(stage)=>{
 const events={reads:0,subscribed:0,closed:0,focus:0,applies:0,detaches:0};let release,entered;
 const gate=new Promise(resolve=>release=resolve),started=new Promise(resolve=>entered=resolve);
 const pause=async name=>{if(stage===name){entered();await gate}};
 const saved=dockIntent(emptyDock(),{kind:'dock',operation:'open',coordinate:stage==='terminal'?{action:'resources'}:{action:'ui_block',key:'example'},title:'Example'});
 const original=DeviceStore.open;
 DeviceStore.open=async()=>{await pause('open');return {read:async()=>{events.reads++;await pause('read');return {value:saved}},apply:async()=>{events.applies++;return {value:saved}},subscribe:()=>{events.subscribed++;return()=>events.subscribed--},close:()=>events.closed++}};
 const add=window.addEventListener,remove=window.removeEventListener;
 const listeners=new Set();window.addEventListener=function(type,fn,...args){if(type==='focus')listeners.add(fn);return add.call(this,type,fn,...args)};
 window.removeEventListener=function(type,fn,...args){if(type==='focus')listeners.delete(fn);return remove.call(this,type,fn,...args)};
 installActions({command:()=>pause('reopen'),call:async(method,payload)=>{const request=JSON.parse(payload).request;if(request.type==='create'){await pause('terminal');return JSON.stringify({value:{id:'attachment',terminal:{id:'created'}}})}if(request.type==='detach')events.detaches++;return JSON.stringify({value:[]})},open:async()=>{},select(){},closeSurface:async()=>{},addSurface:async()=>{}});
 presentationIdentity.set('lifecycle:'+stage);
 source.set({surfaces:{main:{kind:'native',session:'test',generation:'1',ui_surfaces:[]}},application_surfaces:[]});
 const node=document.createElement('div');document.body.append(node);const root=createRoot(node);root.render(<LayoutContext.Provider value={{layout:{...defaultLayout,resourcesClosed:false},update(){},expand(){}}}><ResourceDocks launcher={<span>Resources</span>}/></LayoutContext.Provider>);
 if(stage==='terminal'){for(let attempt=0;attempt<100&&!node.querySelector('.terminal-list');attempt++)await new Promise(resolve=>setTimeout(resolve,10));[...node.querySelectorAll('button')].find(button=>button.textContent==='New terminal').click()}
 await started;root.unmount();release();await new Promise(resolve=>setTimeout(resolve,30));
 window.dispatchEvent(new Event('focus'));await new Promise(resolve=>setTimeout(resolve,30));
 events.focus=listeners.size;DeviceStore.open=original;window.addEventListener=add;window.removeEventListener=remove;node.remove();return events;
};
window.mountExtension=async(action,seed)=>{
 const identity='extension:'+action,scope='dock:extension-session';
 if(seed){const store=await DeviceStore.open(identity);await store.apply('layouts',{kind:'dock',operation:'open',coordinate:{action,bundle:'fixture',name:'card'},title:'Fixture card'},scope);store.close()}
 const reference={name:'card',revision:'fixture'};
 window.extensionCommands=[];
 installActions({command:async command=>{window.extensionCommands.push(command)},call:async()=>'',open:async()=>{},select(){},closeSurface:async()=>{},addSurface:async()=>{}});
 presentationIdentity.set(identity);
 const item={bundle:'fixture',reference,title:'Fixture card'};
 source.set({surfaces:{main:{kind:'native',session:'extension-session',generation:'17',ui_surfaces:[item]}},application_surfaces:[item]});
 const node=document.createElement('main');document.body.append(node);const root=createRoot(node);
 root.render(<LayoutContext.Provider value={{layout:{...defaultLayout,resourcesClosed:false},update(){},expand(){}}}><ResourceDocks launcher={<span>Resources</span>}/></LayoutContext.Provider>);
 window.unmountExtension=()=>{root.unmount();node.remove()};
};
`;
const server=await createServer({configFile:false,root,optimizeDeps:{entries:[]},esbuild:{jsx:'automatic'},server:{host:'127.0.0.1',port:0},plugins:[{name:'review-fixture',resolveId(id){if(id==='/review-test.tsx')return entry},load(id){if(id===entry)return code},configureServer(server){server.middlewares.use('/review.html',(_req,res)=>{res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Review lifecycle</title><script type="module" src="/review-test.tsx"></script>')})}}]});
let browser;const errors=[],checks=[];
try{
 await mkdir(report,{recursive:true});await server.listen();browser=await chromium.launch();
 const page=await browser.newPage();page.on('pageerror',e=>errors.push(e.message));await page.goto(`http://127.0.0.1:${server.httpServer.address().port}/review.html`);await page.waitForFunction(()=>window.lifecycle);
 await page.setViewportSize({width:1440,height:900});await page.evaluate(()=>window.mountVisibility());
 await page.waitForFunction(()=>window.oldVisibilityCallback);
 await page.setViewportSize({width:390,height:844});await page.waitForFunction(()=>document.querySelector('output')?.dataset.narrow==='true');
 await page.evaluate(()=>window.oldVisibilityCallback({resourcesClosed:false}));
 await page.waitForFunction(()=>document.querySelector('output')?.dataset.closed==='false');
 assert.deepEqual(await page.evaluate(()=>window.visibilityWrites),[]);
 await page.setViewportSize({width:1440,height:900});await page.waitForFunction(()=>document.querySelector('output')?.dataset.narrow==='false');
 assert.equal(await page.locator('output').getAttribute('data-closed'),'true');
 await page.evaluate(()=>window.oldVisibilityCallback({resourcesClosed:false}));
 await page.waitForFunction(()=>document.querySelector('output')?.dataset.saved==='false');
 assert.deepEqual(await page.evaluate(()=>window.visibilityWrites),[{resourcesClosed:false}]);
 await page.evaluate(()=>window.unmountVisibility());checks.push({retainedResourceListenerUsesCurrentViewport:true});
 for(const action of ['application_surface','surface']){
  const expected=action==='application_surface'?{action:'application_ui_surface',reference:{name:'card',revision:'fixture'}}:{action:'ui_surface',reference:{name:'card',revision:'fixture'},pane:'main',generation:'17'};
  await page.evaluate(action=>window.mountExtension(action,true),action);
  await page.waitForFunction(()=>window.extensionCommands.length===1);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected]);
  await page.locator('[data-dockkit-tab]').first().click({button:'right'});
  await page.getByRole('menuitem',{name:'Duplicate',exact:true}).click();
  await page.waitForFunction(()=>window.extensionCommands.length===2);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected,expected]);
  await page.evaluate(()=>window.unmountExtension());
  await page.evaluate(action=>window.mountExtension(action,false),action);
  await page.waitForFunction(()=>window.extensionCommands.length===2);
  assert.deepEqual(await page.evaluate(()=>window.extensionCommands),[expected,expected]);
  await page.evaluate(()=>window.unmountExtension());
  checks.push({action,restore:2,duplicate:true,exactCommand:expected});
 }
 for(const stage of ['open','read','reopen','terminal']){
  const result=await page.evaluate(stage=>window.lifecycle(stage),stage);
  assert.equal(result.subscribed,0);assert.equal(result.focus,0);assert.equal(result.closed,1);assert.equal(result.reads,stage==='open'?0:1);assert.equal(result.applies,0);assert.equal(result.detaches,stage==='terminal'?1:0);checks.push({stage,...result});
 }
 const hosts=await page.evaluate(()=>{
  const {resourceHosts,resources,publishResources,clearResources}=modules;let changes=0;
  const stop=resources.subscribe(()=>changes++),host={body:document.createElement('div'),title:'Files',entry:{view:'one',session:'test',pane:'main',generation:'1',floating:false,reopen:{action:'ui_block',key:'key'}},key:'first'};
  resourceHosts.set('one',host);publishResources();const snapshot=resources.getSnapshot();
  for(let i=0;i<100;i++){host.key=String(i);publishResources()}
  const stable=resources.getSnapshot()===snapshot,contentChanges=changes;
  host.title='New title';publishResources();host.entry.floating=true;publishResources();clearResources();stop();return {stable,contentChanges,changes};
 });assert.deepEqual(hosts,{stable:true,contentChanges:1,changes:4});checks.push(hosts);
 const storage=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock}=modules,a=await DeviceStore.open('review'),b=await DeviceStore.open('review');
  await Promise.all([a.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'resources'},title:'Resources'},'dock:session'),b.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'ui_block',key:'one'},title:'One'},'dock:session')]);
  const before=await a.read('layouts','dock:session');
  let puts=0,deletes=0,clears=0;const put=IDBObjectStore.prototype.put,del=IDBObjectStore.prototype.delete,clear=IDBObjectStore.prototype.clear;
  IDBObjectStore.prototype.put=function(...args){puts++;return put.apply(this,args)};IDBObjectStore.prototype.delete=function(...args){deletes++;return del.apply(this,args)};IDBObjectStore.prototype.clear=function(...args){clears++;return clear.apply(this,args)};
  await a.apply('layouts',{kind:'dock',operation:'mode',mode:'fullscreen'},'dock:session');
  IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.delete=del;IDBObjectStore.prototype.clear=clear;
  const after=await b.read('layouts','dock:session');a.close();b.close();return {tabs:Object.keys(before.value.state.tabs).length,before:before.revision,after:after.revision,puts,deletes,clears,mode:after.value.state.mode};
 });assert.deepEqual(storage,{tabs:2,before:'2',after:'3',puts:1,deletes:0,clears:0,mode:'fullscreen'});checks.push(storage);
 const noops=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock,dockIntent}=modules,a=await DeviceStore.open('noops'),b=await DeviceStore.open('noops'),scope='dock:capped';
  const act=(doc,operation,extra={})=>dockIntent(doc,{kind:'dock',operation,...extra});
  let doc=emptyDock();
  for(let i=0;i<6;i++)doc=act(doc,'open',{coordinate:{action:'resources'},title:`Resource ${i}`,duplicate:true});
  doc=act(doc,'split',{pane:doc.state.activePaneId});
  for(const tab of Object.keys(doc.state.tabs).slice(0,4))doc=act(doc,'float',{tab});
  await new Promise((resolve,reject)=>{const tx=a.database.transaction('layouts','readwrite');tx.objectStore('layouts').put({key:a.scopedKey(scope),value:doc,revision:'7',used:1});tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error)});
  const before=await b.read('layouts',scope),tab=Object.keys(doc.state.tabs).at(-1);
  let puts=0,deletes=0,broadcasts=0,notifications=0;
  const put=IDBObjectStore.prototype.put,del=IDBObjectStore.prototype.delete,post=BroadcastChannel.prototype.postMessage;
  const stops=[a.subscribe('layouts',scope,()=>notifications++),b.subscribe('layouts',scope,()=>notifications++)];
  IDBObjectStore.prototype.put=function(...args){puts++;return put.apply(this,args)};
  IDBObjectStore.prototype.delete=function(...args){deletes++;return del.apply(this,args)};
  BroadcastChannel.prototype.postMessage=function(...args){broadcasts++;return post.apply(this,args)};
  let empty;
  try{
   empty=await a.apply('layouts',{kind:'dock',operation:'undo'},'dock:absent');
   await a.apply('layouts',{kind:'dock',operation:'redo'},'dock:absent');
   await a.apply('layouts',{kind:'dock',operation:'split',pane:doc.state.activePaneId},scope);
   await a.apply('layouts',{kind:'dock',operation:'float',tab},scope);
   await a.apply('layouts',{kind:'dock',operation:'drop',tab,pane:doc.state.activePaneId,zone:'outside'},scope);
  }finally{IDBObjectStore.prototype.put=put;IDBObjectStore.prototype.delete=del;BroadcastChannel.prototype.postMessage=post}
  const after=await b.read('layouts',scope);await new Promise(resolve=>setTimeout(resolve,20));
  stops.forEach(stop=>stop());a.close();b.close();
  return {puts,deletes,broadcasts,notifications,emptyRevision:empty.revision,revision:after.revision,unchanged:JSON.stringify(before)===JSON.stringify(after)};
 });assert.deepEqual(noops,{puts:0,deletes:0,broadcasts:0,notifications:0,emptyRevision:'0',revision:'7',unchanged:true});checks.push(noops);
 await writeFile(join(report,'dock-noops.json'),JSON.stringify(noops,null,2));
 const history=await page.evaluate(async()=>{
  const {DeviceStore,emptyDock,dockIntent}=modules,a=await DeviceStore.open('history'),b=await DeviceStore.open('history'),scope='dock:session';
  let doc=emptyDock();
  for(let i=0;i<16;i++)doc=dockIntent(doc,{kind:'dock',operation:'open',coordinate:{action:'resources'},title:`Resource ${i}`,duplicate:true});
  for(let i=0;i<64;i++)doc=dockIntent(doc,{kind:'dock',operation:'mode',mode:i%2?'push':'fullscreen'});
  const write=record=>new Promise((resolve,reject)=>{const tx=b.database.transaction('layouts','readwrite');tx.objectStore('layouts').put(record);tx.oncomplete=resolve;tx.onerror=tx.onabort=()=>reject(tx.error)});
  await write({key:a.scopedKey(scope),value:doc,revision:'1',used:1});
  const intent={kind:'dock',operation:'mode',mode:'fullscreen'},clone=globalThis.structuredClone;
  let copies=0,expected,once,actual;
  globalThis.structuredClone=(...args)=>{copies++;return clone(...args)};
  try{expected=dockIntent(doc,intent);once=copies;copies=0;actual=(await a.apply('layouts',intent,scope)).value}
  finally{globalThis.structuredClone=clone}
  const equal=JSON.stringify(actual)===JSON.stringify(expected),saved=await b.read('layouts',scope);
  const timing=[];
  for(let i=0;i<30;i++){const start=performance.now();await a.apply('layouts',{...intent,mode:i%2?'push':'fullscreen'},scope);if(i>=5)timing.push(performance.now()-start)}
  timing.sort((x,y)=>x-y);
  const bad=clone(saved.value);bad.history.entries[0].inverse=[{type:'openTab',paneId:bad.state.activePaneId,tab:{id:'tab999',kind:'rsi',contentId:'null',title:'bad'},index:0}];
  await write({key:b.scopedKey(scope),value:bad,revision:'99',used:2});
  let rejected=false;try{await a.apply('layouts',intent,scope)}catch(error){rejected=String(error).includes('Invalid resource layout')}
  const retained=await new Promise((resolve,reject)=>{const tx=b.database.transaction('layouts','readonly'),read=tx.objectStore('layouts').get(b.scopedKey(scope));tx.oncomplete=()=>resolve(read.result.revision);tx.onerror=()=>reject(tx.error)});
  a.close();b.close();return {tabs:Object.keys(doc.state.tabs).length,history:doc.history.entries.length,once,copies,equal,rejected,retained,p50ms:timing[12],p95ms:timing[23]};
 });checks.push(history);
 await writeFile(join(report,'history.json'),JSON.stringify(history,null,2));
 assert.equal(history.tabs,16);assert.equal(history.history,64);assert.equal(history.copies,history.once);assert.equal(history.equal,true);assert.equal(history.rejected,true);assert.equal(history.retained,'99');

 const oversized=await page.evaluate(async()=>{
  const owner=await modules.DeviceStore.open('oversized'),scope='dock:large';
  let saved=await owner.read('layouts',scope),accepted=0,rejected=false;
  for(let i=0;i<16;i++){
   try{saved=await owner.apply('layouts',{kind:'dock',operation:'open',coordinate:{action:'inspect_source',source:{payload:'x'.repeat(12000),id:i}},title:'Large source',duplicate:true},scope);accepted++}
   catch(error){if(!String(error).includes('too large'))throw error;rejected=true;break}
  }
  const after=await owner.read('layouts',scope),preserved=JSON.stringify(saved)===JSON.stringify(after);
  const tab=Object.keys(after.value.state.tabs)[0];
  const closed=await owner.apply('layouts',{kind:'dock',operation:'close',tab},scope);
  owner.close();return {accepted,rejected,preserved,closedTabs:Object.keys(closed.value.state.tabs).length};
 });assert.equal(oversized.rejected,true);assert.equal(oversized.preserved,true);assert.equal(oversized.closedTabs,oversized.accepted-1);checks.push({oversized});
 for(const legacy of ['valid','invalid','over-count']){
  const migration=await browser.newPage();
  await migration.goto(`http://127.0.0.1:${server.httpServer.address().port}/review.html`);await migration.waitForFunction(()=>window.modules);
  const result=await migration.evaluate(async legacy=>{
   const records=Array.from({length:legacy==='over-count'?65:1},(_,i)=>({key:i===0?'legacy':`legacy-${i}`,layout:modules.defaultLayout,used:legacy==='invalid'?-1:1}));
   await new Promise((resolve,reject)=>{
    const opening=indexedDB.open('rsi.presentation',1);opening.onupgradeneeded=()=>opening.result.createObjectStore('layouts',{keyPath:'key'});opening.onerror=()=>reject(opening.error);
    opening.onsuccess=()=>{const db=opening.result,tx=db.transaction('layouts','readwrite');for(const record of records)tx.objectStore('layouts').put(record);tx.oncomplete=()=>{db.close();resolve()};tx.onabort=()=>reject(tx.error)};
   });
   const owner=await modules.DeviceStore.open('legacy'),before=await owner.read('layouts');
   const archived=await new Promise((resolve,reject)=>{const tx=owner.database.transaction('legacy-layouts-v1','readonly'),read=tx.objectStore('legacy-layouts-v1').getAll();tx.oncomplete=()=>resolve(read.result);tx.onabort=()=>reject(tx.error)});
   const saved=await owner.apply('layouts',{kind:'patch',patch:{navigationWidth:311}});owner.close();
   const reopened=await modules.DeviceStore.open('legacy'),after=await reopened.read('layouts');reopened.close();
   return {revision:before.revision,retained:JSON.stringify(archived)===JSON.stringify([...records].sort((a,b)=>a.key<b.key?-1:1)),archived:archived.length,after:after.revision,saved:saved.revision,width:after.value.navigationWidth};
  },legacy);
  assert.equal(result.revision,legacy==='valid'?'1':'0');assert.equal(result.retained,true);assert.equal(result.after,result.saved);assert.equal(result.width,311);checks.push({legacy,...result});await migration.close();
 }
 await page.setViewportSize({width:1400,height:900});
 await page.evaluate(()=>{window.unmountResize=window.mountResize()});
 const handle=page.getByRole('separator',{name:'Resize resources'});
 await page.waitForFunction(()=>document.querySelector('[aria-label="Resize resources"]')?.getAttribute('aria-valuenow')==='630');
 await page.setViewportSize({width:1200,height:800});
 await page.waitForFunction(()=>document.querySelector('[aria-label="Resize resources"]')?.getAttribute('aria-valuenow')==='540');
 await handle.focus();await page.keyboard.press('ArrowLeft');
 await page.waitForFunction(()=>Math.abs(Number(document.querySelector('output').textContent)-550/1200)<1e-9);
 checks.push({viewportResize:{before:630,after:540,keyboard:550}});
 await page.evaluate(()=>window.unmountResize());
 assert.deepEqual(errors,[]);await writeFile(join(report,'result.json'),JSON.stringify({checks,errors},null,2));
}finally{await browser?.close();await server.close()}
