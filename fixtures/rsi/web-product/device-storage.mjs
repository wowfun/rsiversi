// Isolated real Chromium IndexedDB/transaction check against the current source modules.
import assert from 'node:assert/strict';
import {createServer} from '../../../apps/web/node_modules/vite/dist/node/index.js';
import {writeFile,mkdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {chromium} from 'playwright';
const root=resolve(import.meta.dirname,'../../../apps/web');
const report=process.env.RSI_REPORT_DIR;
assert(report,'RSI_REPORT_DIR must name an isolated report directory');
const server=await createServer({root,configFile:false,optimizeDeps:{noDiscovery:true,include:[]},server:{host:'127.0.0.1',port:0},plugins:[{name:'storage-fixture',configureServer(server){server.middlewares.use((req,res,next)=>{if(req.url!=='/')return next();res.setHeader('Content-Type','text/html');res.end('<!doctype html><title>Device storage verification</title>');});}}]});
await server.listen();
let browser;
try{
  browser=await chromium.launch();const context=await browser.newContext();
  const one=await context.newPage(),two=await context.newPage(),origin=`http://127.0.0.1:${server.httpServer.address().port}`;
  await Promise.all([one.goto(origin),two.goto(origin)]);
  await one.evaluate(async()=>{
    const {defaultLayout}=await import('/presentation-layout.js');
    await new Promise((resolve,reject)=>{
      const opening=indexedDB.open('rsi.presentation',1);
      opening.onupgradeneeded=()=>opening.result.createObjectStore('layouts',{keyPath:'key'});
      opening.onerror=()=>reject(opening.error);
      opening.onsuccess=()=>{
        const db=opening.result,tx=db.transaction('layouts','readwrite');tx.objectStore('layouts').put({key:'fixture-principal',used:1,layout:defaultLayout});
        tx.oncomplete=()=>{db.close();resolve();};tx.onerror=()=>reject(tx.error);
      };
    });
  });
  const open=page=>page.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js');window.store=await DeviceStore.open('fixture-principal');window.invalidated=0;
    window.store.subscribe('layouts','',()=>window.invalidated++);
    return window.store.read('layouts');
  });
  const before=await open(one);await open(two);assert.equal(before.revision,'1');
  await Promise.all([
    one.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{navigationWidth:310}})),
    two.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{resourcesWidth:.4}})),
  ]);
  const after=await one.evaluate(()=>store.read('layouts'));
  assert.equal(after.revision,'3');assert.equal(after.value.navigationWidth,310);assert.equal(after.value.resourcesWidth,.4);
  await two.waitForFunction(()=>invalidated>=2);
  await Promise.all([
    one.evaluate(()=>store.apply('layouts',{kind:'workspace',id:'one',expanded:true})),
    two.evaluate(()=>store.apply('layouts',{kind:'workspace',id:'two',expanded:true})),
  ]);
  const expanded=await one.evaluate(()=>store.read('layouts'));assert.equal(expanded.value.workspaces.length,2);
  const order=await one.evaluate(async()=>{
    const members=Array.from({length:65},(_,i)=>({id:`s-${i}`,partition:'local/project/unpinned'}));
    await store.apply('orders',{kind:'reconcile',members},'sessions');
    return store.apply('orders',{kind:'move',members,id:'s-64',position:'first',relative:null},'sessions');
  });
  assert.equal(order.value.ids.length,65);assert.equal(order.value.ids[0],'s-64');
  const rollback=await one.evaluate(async()=>{
    let error;try{await store.apply('layouts',{kind:'patch',patch:{navigation:'invalid'}});}catch(e){error=e.message;}
    let quota;try{await store.apply('orders',{kind:'reconcile',members:Array.from({length:1024},(_,i)=>({id:`${String(i).padStart(4,'0')}${'x'.repeat(252)}`,partition:'group'}))},'sessions');}catch(e){quota=e.message;}
    return {error,quota,layout:await store.read('layouts'),order:await store.read('orders','sessions')};
  });
  assert.match(rollback.error,/layout/i);assert.match(rollback.quota,/too large/);assert.deepEqual(rollback.layout,expanded);assert.deepEqual(rollback.order,order);
  const atomic=await one.evaluate(async()=>{
    await store.applyAll([
      {bucket:'orders',scope:'sessions',intent:{kind:'updated'}},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{sessionOrder:'updated'}}},
    ]);
    const before=[await store.read('orders','sessions'),await store.read('preferences')];
    let error;try{await store.applyAll([
      {bucket:'orders',scope:'sessions',intent:{kind:'reconcile',members:[{id:'new',partition:'one'}]}},
      {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{sessionOrder:'invalid'}}},
    ]);}catch(e){error=e.message;}
    return {before,after:[await store.read('orders','sessions'),await store.read('preferences')],error};
  });
  assert.deepEqual(atomic.before,atomic.after);assert.match(atomic.error,/preferences/);assert.deepEqual(atomic.after[0].value.ids,[]);
  const isolation=await one.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js');const other=await DeviceStore.open('different-principal');
    const value=await other.read('orders','sessions');await other.apply('preferences',{kind:'patch',patch:{view:'flat'}});other.close();
    const db=await new Promise((resolve,reject)=>{const q=indexedDB.open('fixture-drafts',1);q.onupgradeneeded=()=>q.result.createObjectStore('drafts');q.onsuccess=()=>resolve(q.result);q.onerror=()=>reject(q.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('drafts','readwrite');tx.objectStore('drafts').put('unchanged draft','draft');tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    await new Promise((resolve,reject)=>{const tx=store.database.transaction('layouts','readwrite');tx.objectStore('layouts').put({key:store.key,value:{damaged:true},used:1,revision:'9'});tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    let corrupt;try{await store.read('layouts');}catch(e){corrupt=e.message;}
    const draft=await new Promise((resolve,reject)=>{const request=db.transaction('drafts').objectStore('drafts').get('draft');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});db.close();
    return {value,corrupt,draft,preferences:await store.read('preferences')};
  });
  assert.deepEqual(isolation.value.value.ids,[]);assert.match(isolation.corrupt,/layout/);assert.equal(isolation.draft,'unchanged draft');assert.equal(isolation.preferences.value.view,'workspace');
  const evidence={browser:browser.version(),module:'current apps/web/device-store.js',schema:2,realIndexedDb:true,windows:2,migratedRevision:before.revision,concurrentRevision:after.revision,workspaceIntents:expanded.value.workspaces.length,completeMembers:order.value.ids.length,abortedWritesPreserved:true,multiStoreAtomicRollback:true,broadcastInvalidation:true,principalIsolation:true,draftIsolation:true};
  await mkdir(report,{recursive:true});await writeFile(join(report,'result.json'),JSON.stringify(evidence,null,2)+'\n');console.log(JSON.stringify(evidence));
}finally{await browser?.close();await server.close();}
