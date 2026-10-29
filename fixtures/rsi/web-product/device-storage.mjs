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
    const {emptyDock}=await import('/src/dock-state.ts');
    const {conversationWidth,...oldLayout}=defaultLayout;
    await new Promise((resolve,reject)=>{
      const opening=indexedDB.open('rsi.presentation',2);
      opening.onupgradeneeded=()=>{for(const bucket of ['layouts','preferences','orders'])opening.result.createObjectStore(bucket,{keyPath:'key'});};
      opening.onerror=()=>reject(opening.error);
      opening.onsuccess=()=>{
        const db=opening.result,tx=db.transaction(['layouts','preferences','orders'],'readwrite');
        tx.objectStore('layouts').put({key:'fixture-principal',used:1,revision:'8',value:{...oldLayout,version:3,navigationWidth:301}});
        window.retainedDock={key:JSON.stringify(['fixture-principal','dock:retained']),used:2,revision:'11',value:emptyDock()};
        tx.objectStore('layouts').put(window.retainedDock);
        tx.objectStore('preferences').put({key:'unrelated',used:3,revision:'6',value:{view:'flat',sessionOrder:'manual',workspaceOrder:'updated'}});
        tx.objectStore('orders').put({key:'unrelated',used:3,revision:'7',value:{ids:['saved-order']}});
        tx.oncomplete=()=>{db.close();resolve();};tx.onerror=()=>reject(tx.error);
      };
    });
  });
  const open=page=>page.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js');window.versionChanged=0;window.store=await DeviceStore.open('fixture-principal',{onVersionChange:()=>window.versionChanged++});window.invalidated=0;
    window.store.subscribe('layouts','',()=>window.invalidated++);
    return window.store.read('layouts');
  });
  const before=await open(one);await open(two);assert.equal(before.revision,'0');assert.equal(before.value.version,4);
  const preserved=await one.evaluate(async()=>{
    const read=(bucket,key)=>new Promise((resolve,reject)=>{const tx=store.database.transaction(bucket,'readonly'),q=tx.objectStore(bucket).get(key);tx.oncomplete=()=>resolve(q.result);tx.onabort=()=>reject(tx.error)});
    return {dock:JSON.stringify(await read('layouts',retainedDock.key))===JSON.stringify(retainedDock),preferences:await read('preferences','unrelated'),order:await read('orders','unrelated')};
  });
  assert.equal(preserved.dock,true);assert.equal(preserved.preferences.value.view,'flat');assert.deepEqual(preserved.order.value.ids,['saved-order']);
  await Promise.all([
    one.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{navigationWidth:310}})),
    two.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{resourcesWidth:.4}})),
  ]);
  const after=await one.evaluate(()=>store.read('layouts'));
  assert.equal(after.revision,'2');assert.equal(after.value.navigationWidth,310);assert.equal(after.value.resourcesWidth,.4);
  await two.waitForFunction(()=>invalidated>=2);
  await Promise.all([
    one.evaluate(()=>store.apply('layouts',{kind:'workspace',id:'one',expanded:true})),
    two.evaluate(()=>store.apply('layouts',{kind:'workspace',id:'two',expanded:true})),
  ]);
  const expanded=await one.evaluate(()=>store.read('layouts'));assert.equal(expanded.value.workspaces.length,2);
  const reservedRollback=await one.evaluate(async()=>{
    const before=await store.read('layouts');let observed,error;
    try{await store.apply('layouts',{kind:'patch',patch:{navigationWidth:320}},'',values=>{observed=values[0].navigationWidth;throw Error('cleanup capacity full')})}catch(failure){error=failure.message}
    return {before,after:await store.read('layouts'),observed,error};
  });
  assert.equal(reservedRollback.observed,320);assert.match(reservedRollback.error,/cleanup capacity full/);assert.deepEqual(reservedRollback.after,reservedRollback.before);
  const order=await one.evaluate(async()=>{
    const members=Array.from({length:65},(_,i)=>({id:`s-${i}`,partition:'local/project/unpinned'}));
    await store.apply('orders',{kind:'reconcile',members},'sessions');
    return store.apply('orders',{kind:'move',members,id:'s-64',position:'first',relative:null},'sessions');
  });
  assert.equal(order.value.ids.length,65);assert.equal(order.value.ids[0],'s-64');
  const rollback=await one.evaluate(async()=>{
    let error;try{await store.apply('layouts',{kind:'patch',patch:{navigation:'invalid'}});}catch(e){error=e.message;}
    const members=length=>Array.from({length},(_,i)=>({id:`${String(i).padStart(4,'0')}${'x'.repeat(252)}`,partition:'group'}));
    let quota;try{await store.apply('orders',{kind:'reconcile',members:members(768)},'sessions');}catch(e){quota=e.message;}
    let full;try{await store.apply('orders',{kind:'reconcile',members:members(1024)},'sessions');}catch(e){full=e.message;}
    return {error,quota,full,layout:await store.read('layouts'),order:await store.read('orders','sessions')};
  });
  assert.match(rollback.error,/layout/i);assert.match(rollback.quota,/too large/);assert.match(rollback.full,/Saved order is full/);assert.deepEqual(rollback.layout,expanded);assert.deepEqual(rollback.order,order);
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
  for(const [scope,preference] of [['sessions:all','sessionOrder'],['workspaces','workspaceOrder']]){
    const active=await one.evaluate(async({scope,preference})=>{
      const before=await store.apply('preferences',{kind:'patch',patch:{[preference]:'manual'}});
      const [order,after]=await store.reconcileNavigation(scope,[{id:'member',partition:'group'}]);
      return {before,after,order};
    },{scope,preference});
    assert.deepEqual(active.before,active.after);assert.deepEqual(active.order.value.ids,['member']);
    await Promise.all([
      one.evaluate(scope=>store.reconcileNavigation(scope,[{id:'later',partition:'group'}]),scope),
      two.evaluate(({scope,preference})=>store.applyAll([
        {bucket:'orders',scope,intent:{kind:'updated'}},
        {bucket:'preferences',scope:'',intent:{kind:'patch',patch:{[preference]:'updated'}}},
      ]),{scope,preference}),
    ]);
    const cleared=await one.evaluate(async scope=>{
      const before=[await store.read('orders',scope),await store.read('preferences')];
      return {before,after:await store.reconcileNavigation(scope,[{id:'stale',partition:'group'}])};
    },scope);
    assert.deepEqual(cleared.before,cleared.after);assert.deepEqual(cleared.after[0].value.ids,[]);assert.equal(cleared.after[1].value[preference],'updated');
  }
  const malformedGuard=await one.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js'),broken=await DeviceStore.open('corrupt-navigation');
    await new Promise((resolve,reject)=>{const tx=broken.database.transaction('preferences','readwrite');tx.objectStore('preferences').put({key:broken.key,value:null,revision:'1',used:1});tx.oncomplete=resolve;tx.onabort=()=>reject(tx.error)});
    let error;try{await broken.reconcileNavigation('sessions:all',[{id:'member',partition:'group'}])}catch(e){error=e.message}
    const order=await broken.read('orders','sessions:all');broken.close();return {error,order};
  });
  assert.match(malformedGuard.error,/preferences/);assert.equal(malformedGuard.order.revision,'0');assert.deepEqual(malformedGuard.order.value.ids,[]);
  const isolation=await one.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js');const other=await DeviceStore.open('different-principal');
    const value=await other.read('orders','sessions');await other.apply('preferences',{kind:'patch',patch:{view:'flat'}});other.close();
    const db=await new Promise((resolve,reject)=>{const q=indexedDB.open('fixture-drafts',1);q.onupgradeneeded=()=>q.result.createObjectStore('drafts');q.onsuccess=()=>resolve(q.result);q.onerror=()=>reject(q.error);});
    await new Promise((resolve,reject)=>{const tx=db.transaction('drafts','readwrite');tx.objectStore('drafts').put('unchanged draft','draft');tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    await new Promise((resolve,reject)=>{const tx=store.database.transaction('layouts','readwrite');tx.objectStore('layouts').put({key:store.key,value:{damaged:true},used:-1,revision:'invalid'});tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);});
    let corrupt;try{await store.read('layouts');}catch(e){corrupt=e.message;}
    const draft=await new Promise((resolve,reject)=>{const request=db.transaction('drafts').objectStore('drafts').get('draft');request.onsuccess=()=>resolve(request.result);request.onerror=()=>reject(request.error);});db.close();
    const reset=await store.apply('layouts',{kind:'reset'}),saved=await store.apply('layouts',{kind:'patch',patch:{conversationWidth:1200}});
    let scopeRejected=false;try{await store.apply('layouts',{kind:'reset'},'dock:retained')}catch{scopeRejected=true}
    return {value,corrupt,draft,reset,saved,scopeRejected,preferences:await store.read('preferences'),dock:await store.read('layouts','dock:retained')};
  });
  assert.deepEqual(isolation.value.value.ids,[]);assert.match(isolation.corrupt,/record/);assert.equal(isolation.draft,'unchanged draft');assert.equal(isolation.preferences.value.view,'workspace');
  assert.equal(isolation.reset.value.conversationWidth,null);assert.equal(isolation.saved.value.conversationWidth,1200);assert.equal(isolation.scopeRejected,true);assert.equal(isolation.dock.revision,'11');
  await one.evaluate(()=>new Promise((resolve,reject)=>{const opening=indexedDB.open('rsi.presentation',4);opening.onsuccess=()=>{opening.result.close();resolve()};opening.onerror=()=>reject(opening.error)}));
  assert.deepEqual(await one.evaluate(()=>({closed:store.closed,changed:versionChanged})),{closed:true,changed:1});
  assert.deepEqual(await two.evaluate(()=>({closed:store.closed,changed:versionChanged})),{closed:true,changed:1});
  const blockedContext=await browser.newContext(),blockedPage=await blockedContext.newPage();await blockedPage.goto(origin);
  const blocked=await blockedPage.evaluate(async()=>{
    const {DeviceStore}=await import('/device-store.js');
    const old=await new Promise((resolve,reject)=>{const q=indexedDB.open('rsi.presentation',2);q.onsuccess=()=>resolve(q.result);q.onerror=()=>reject(q.error)});
    let error;try{await DeviceStore.open('retry')}catch(e){error=e.message}
    old.close();
    const retry=await DeviceStore.open('retry'),version=retry.database.version;retry.close();return {error,version};
  });
  assert.match(blocked.error,/blocked/);assert.equal(blocked.version,3);await blockedContext.close();
  const evidence={browser:browser.version(),module:'current apps/web/device-store.js',schema:3,realIndexedDb:true,windows:2,resetRevision:before.revision,concurrentRevision:after.revision,workspaceIntents:expanded.value.workspaces.length,completeMembers:order.value.ids.length,abortedWritesPreserved:true,multiStoreAtomicRollback:true,automaticReconciliationPreservesUpdated:true,broadcastInvalidation:true,principalIsolation:true,draftIsolation:true,upgradePreservesDockPreferencesOrders:preserved,corruptSelectedRecordRepaired:true,versionChangeClosesOwners:true,blockedOpenRetry:blocked};
  await mkdir(report,{recursive:true});await writeFile(join(report,'result.json'),JSON.stringify(evidence,null,2)+'\n');console.log(JSON.stringify(evidence));
}finally{await browser?.close();await server.close();}
