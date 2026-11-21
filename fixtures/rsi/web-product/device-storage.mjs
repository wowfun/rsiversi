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
        tx.objectStore('layouts').put({key:'fixture-principal',used:1,revision:'8',value:{...oldLayout,version:3,navigationWidth:301,navigation:"expanded"}});
        window.retainedDock={key:JSON.stringify(['fixture-principal','dock:retained']),used:2,revision:'11',value:{...emptyDock(),version:1}};
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
  const before=await open(one);await open(two);assert.equal(before.revision,'0');assert.equal(before.value.version,7);
  const preserved=await one.evaluate(async()=>{
    const read=(bucket,key)=>new Promise((resolve,reject)=>{const tx=store.database.transaction(bucket,'readonly'),q=tx.objectStore(bucket).get(key);tx.oncomplete=()=>resolve(q.result);tx.onabort=()=>reject(tx.error)});
    return {obsoleteDockRemoved:await read('layouts',retainedDock.key)===undefined,preferences:await read('preferences','unrelated'),order:await read('orders','unrelated')};
  });
  assert.equal(preserved.obsoleteDockRemoved,true);assert.equal(preserved.preferences.value.view,'flat');assert.deepEqual(preserved.order.value.ids,['saved-order']);
  const scopeReads=await one.evaluate(async()=>{
    const {dockStorageKey,emptyDock,dockIntent,writeDock}=await import('/src/dock-state.ts');
    const key=dockStorageKey('fixture-principal','scoped');
    const doc=dockIntent(emptyDock(),{kind:'dock',operation:'open',coordinate:{action:'resources'},title:'Resources'});
    writeDock(key,doc);
    const prototype=IDBObjectStore.prototype,all=prototype.getAll,get=prototype.get;
    const reads={key:0,bucketRows:0};
    try{
      prototype.get=function(...args){reads.key++;return get.apply(this,args)};
      prototype.getAll=function(...args){
        const request=all.apply(this,args);
        request.addEventListener('success',()=>{reads.bucketRows+=request.result.length});
        return request;
      };
      writeDock(key,dockIntent(doc,{kind:'dock',operation:'close',tab:Object.keys(doc.state.tabs)[0]}));
    }finally{prototype.get=get;prototype.getAll=all}
    return reads;
  });
  assert.deepEqual(scopeReads,{key:0,bucketRows:0},'closing a tab needs no IndexedDB reads');
  await Promise.all([
    one.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{conversationWidth:710}})),
    two.evaluate(()=>store.apply('layouts',{kind:'patch',patch:{detail:'detailed'}})),
  ]);
  const after=await one.evaluate(()=>store.read('layouts'));
  assert.equal(after.revision,'2');assert.equal(after.value.conversationWidth,710);assert.equal(after.value.detail,'detailed');
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
    let error;try{await store.apply('layouts',{kind:'patch',patch:{detail:'invalid'}});}catch(e){error=e.message;}
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
  assert.equal(isolation.reset.value.conversationWidth,null);assert.equal(isolation.saved.value.conversationWidth,1200);assert.equal(isolation.scopeRejected,true);assert.equal(isolation.dock.revision,'0');
  await one.evaluate(()=>new Promise((resolve,reject)=>{const opening=indexedDB.open('rsi.presentation',store.database.version+1);opening.onsuccess=()=>{opening.result.close();resolve()};opening.onerror=()=>reject(opening.error)}));
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
  assert.match(blocked.error,/blocked/);assert.equal(blocked.version,9);await blockedContext.close();
  for(const version of [3,6,8]){
   const priorContext=await browser.newContext(),prior=await priorContext.newPage();await prior.goto(origin);
   const upgraded=await prior.evaluate(async version=>{
    const {defaultLayout}=await import('/presentation-layout.js');
    const {emptyDock}=await import('/src/dock-state.ts');
    const {DeviceStore}=await import('/device-store.js');
    const layout={...defaultLayout,detail:'verbose',conversationWidth:733,workspaces:[{id:'kept',expanded:true}]};
    const {storedNamespace,idBytes}=await import('/storage-namespace.js');
    const dockKey=version===3?JSON.stringify(['previous','dock:open']):[...storedNamespace('11'.repeat(16),'local'),idBytes('ab'.repeat(16))];
    const dock={key:dockKey,used:7,revision:'3',value:{...emptyDock(),version:1}};
    await new Promise((resolve,reject)=>{
      const req=indexedDB.open('rsi.presentation',version);
      req.onupgradeneeded=()=>{
        const bucket=req.result.createObjectStore('layouts',{keyPath:'key'});
        const shell={...layout,version:6,resourcesClosed:false};
        const previous=version===8?shell:{...shell,version:5,navigation:'rail',navigationWidth:333};
        bucket.put({key:'previous',used:9,revision:'8',value:version===3?{...previous,version:4,resourcesWidth:.35}:previous});
        bucket.put(dock);
      };
      req.onsuccess=()=>{req.result.close();resolve()};req.onerror=()=>reject(req.error);
    });
    const store=await DeviceStore.open('previous');
    try{
      const all=await new Promise((resolve,reject)=>{
        const req=store.database.transaction('layouts').objectStore('layouts').getAll();
        req.onsuccess=()=>resolve(req.result);req.onerror=()=>reject(req.error);
      });
      const shell={key:'previous',used:9,revision:'8',value:layout};
      return {actual:all,expected:[shell],dockKeyMatches:all.some(item=>indexedDB.cmp(item.key,dockKey)===0)};
    }finally{store.close()}
   },version);
   assert.deepEqual(upgraded.actual,upgraded.expected);assert.equal(upgraded.dockKeyMatches,false);await priorContext.close();
  }
  const evidence={browser:browser.version(),module:'current apps/web/device-store.js',scopeReads,schema:blocked.version,widthUpgradePreservesOtherFields:true,realIndexedDb:true,windows:2,resetRevision:before.revision,concurrentRevision:after.revision,workspaceIntents:expanded.value.workspaces.length,completeMembers:order.value.ids.length,abortedWritesPreserved:true,multiStoreAtomicRollback:true,automaticReconciliationPreservesUpdated:true,broadcastInvalidation:true,principalIsolation:true,draftIsolation:true,upgrade:preserved,corruptSelectedRecordRepaired:true,versionChangeClosesOwners:true,blockedOpenRetry:blocked};
  await mkdir(report,{recursive:true});await writeFile(join(report,'result.json'),JSON.stringify(evidence,null,2)+'\n');console.log(JSON.stringify(evidence));
}finally{await browser?.close();await server.close();}
