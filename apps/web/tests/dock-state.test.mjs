import assert from 'node:assert/strict';
import {test} from 'node:test';
import 'fake-indexeddb/auto';
import {IDBFactory} from 'fake-indexeddb';
import {emptyDock,dockIntent,savedDock,restoreDock,validateDock,dockPaneIds,floatingTab,narrowDock,displayDock} from '../src/dock-state.ts';
import {applyIntent,boundedRecords,DeviceStore} from '../device-store.js';
import {storedNamespace} from '../storage-namespace.js';
const act=(doc,operation,fields={})=>dockIntent(doc,{kind:'dock',operation,...fields});
const open=doc=>act(doc,'open',{coordinate:{action:'surface',bundle:'files',name:'browser'},title:'Files',duplicate:true});
test('opening, copying or moving a guide respects its destination pane',async()=>{
  const guide={coordinate:{action:'resources'},title:'Resources',duplicate:true};
  const start=act(open(emptyDock()),'open',guide);
  const left=start.state.activePaneId,leftGuide=start.state.nodes[left].activeTabId;
  assert.equal(act(start,'open',guide),start);
  assert.equal(act(start,'duplicate',{tab:leftGuide}),start);
  const split=act(start,'split',{pane:left});
  const right=split.state.activePaneId,rightGuide=split.state.nodes[right].activeTabId;
  assert.notEqual(leftGuide,rightGuide);
  const revealed=act(split,'open',{...guide,pane:left});
  assert.equal(revealed.state.activePaneId,left);
  assert.equal(revealed.state.nodes[left].activeTabId,leftGuide);
  assert.equal(Object.keys(revealed.state.tabs).length,3);
  const float=act(revealed,'float',{tab:rightGuide});
  const floatPane=float.state.floats[0];
  assert.equal(act(float,'place',{tab:leftGuide,pane:floatPane,index:0}),float);
  assert.equal(act(float,'drop',{tab:leftGuide,pane:floatPane,zone:'center'}),float);
  assert.throws(()=>act(revealed,'dock',{pane:right}),'a docked guide cannot return from a float');
  for(const [before,operation,fields] of [
    [split,'place',{tab:rightGuide,pane:left,index:0}],
    [split,'drop',{tab:rightGuide,pane:left,zone:'center'}],
    [float,'dock',{pane:float.state.floats[0]}],
  ]){
    const merged=act(before,operation,fields);
    assert.equal(merged.state.tabs[rightGuide],undefined);
    assert.equal(merged.state.nodes[left].activeTabId,leftGuide);
    assert.deepEqual(act(merged,'undo').state,before.state);
  }
  const a=await DeviceStore.open('concurrent-guides'),b=await DeviceStore.open('concurrent-guides');
  try{
    const request={kind:'dock',operation:'open',...guide};
    await Promise.all([a.apply('layouts',request,'dock:session'),b.apply('layouts',request,'dock:session')]);
    const row=await a.read('layouts','dock:session');
    assert.equal(row.revision,'1');
    assert.equal(Object.keys(row.value.state.tabs).length,1);
  }finally{a.close();b.close()}
});
test('an open-tab selection projects from committed state and follows a peer removing the target',()=>{
  const doc=open(open(emptyDock()));
  const pane=doc.state.activePaneId,[first,second]=doc.state.nodes[pane].tabs;
  const preview=displayDock(doc.state,1000,700,false,first);
  assert.equal(preview.nodes[pane].activeTabId,first);
  assert.equal(doc.state.nodes[pane].activeTabId,second);
  assert.equal(displayDock(doc.state,1000,700,false,second),doc.state);
  const closed=act(doc,'close',{tab:first});
  assert.equal(displayDock(closed.state,1000,700,false,first),closed.state);
});
test('redundant tab and pane focus preserve redo and the latest committed revision',async()=>{
  const store=await DeviceStore.open('dock-focus'),peer=await DeviceStore.open('dock-focus');
  const scope='dock:focus';
  const apply=(owner,operation,fields,live)=>owner.apply('layouts',{kind:'dock',operation,...fields},scope,undefined,live);
  try{
    const files={coordinate:{action:'surface',bundle:'files',name:'browser'},title:'Files',duplicate:true};
    const first=await apply(store,'open',files);
    const second=await apply(store,'open',files,first.value);
    const undone=await apply(store,'undo',{},second.value);
    const pane=undone.value.state.activePaneId;
    const tab=undone.value.state.nodes[pane].activeTabId;
    for(const [operation,fields] of [['focus',{tab}],['focus-pane',{pane}]]){
      const result=await apply(store,operation,fields,undone.value);
      assert.equal(result.revision,undone.revision);
      assert.equal(result.value,undone.value);
    }
    const redone=await apply(store,'redo',{},undone.value);
    assert.deepEqual(redone.value.state,second.value.state);
    const changed=await apply(peer,'focus',{tab});
    assert.notEqual(changed.revision,redone.revision);
    const latest=await apply(store,'focus',{tab},redone.value);
    assert.equal(latest.revision,changed.revision);
    assert.equal(latest.value.history.entries.length,0);
  }finally{store.close();peer.close()}
});
test('schema 4 Dock addresses retain the exact namespace, scope, revision and LRU across upgrade',async t=>{
 const factory=new IDBFactory();
 t.mock.method(globalThis.indexedDB,'open',factory.open.bind(factory));
 const endpoint='1'.repeat(32),device='device:'+'2'.repeat(32),scope='dock:文件/3';
 const names=['local',device].map(principal=>JSON.stringify([endpoint,principal]));
 const saved=savedDock(open(emptyDock()));
 await new Promise((resolve,reject)=>{
   const req=indexedDB.open('rsi.presentation',4);
   req.onupgradeneeded=()=>{
     const layouts=req.result.createObjectStore('layouts',{keyPath:'key'});
     names.forEach((owner,index)=>layouts.put({key:JSON.stringify([owner,scope]),value:saved,used:11+index,revision:String(2+index)}));
   };
   req.onsuccess=()=>{req.result.close();resolve()};req.onerror=()=>reject(req.error);
 });
 const local=await DeviceStore.open(names[0]),remote=await DeviceStore.open(names[1]);
 try {
   const raw=await new Promise((resolve,reject)=>{
     const req=local.database.transaction('layouts').objectStore('layouts').getAll();
     req.onsuccess=()=>resolve(req.result);req.onerror=()=>reject(req.error);
   });
   assert.deepEqual(raw.map(row=>[row.key[0].byteLength,row.key[1].byteLength,row.key[2],row.used]),[[16,0,scope,11],[16,16,scope,12]]);
   assert.deepEqual(raw.map(row=>row.value),[saved,saved]);
   assert.equal((await local.read('layouts',scope)).revision,'2');
   assert.equal((await remote.read('layouts',scope)).revision,'3');
   await local.apply('layouts',{kind:'dock',operation:'close',tab:Object.keys(saved.state.tabs)[0]},scope);
   assert.deepEqual(Object.keys((await local.read('layouts',scope)).value.state.tabs),[]);
   assert.deepEqual(savedDock((await remote.read('layouts',scope)).value),saved);
   const intent={kind:'dock',operation:'open',coordinate:{action:'resources'},title:'Resources'};
   for(let i=0;i<63;i++)await local.apply('layouts',intent,'dock:next-'+i);
   assert.equal((await remote.read('layouts',scope)).revision,'0','evict the oldest untouched binary address');
   assert.equal((await local.read('layouts',scope)).revision,'3');
 }finally{local.close();remote.close()}
});
test('schema 5 Web dock scopes compact losslessly and remain distinct from custom session names',async t=>{
  const factory=new IDBFactory();
  t.mock.method(globalThis.indexedDB,'open',factory.open.bind(factory));
  const owner=['1'.repeat(32),'local'],prefix=storedNamespace(...owner);
  const id='ab'.repeat(16),scopes=['dock:web-'+id,'dock:web-'+id.toUpperCase(),'dock:文件/3'];
  const value=savedDock(open(emptyDock()));
  await new Promise((resolve,reject)=>{
    const req=indexedDB.open('rsi.presentation',5);
    req.onupgradeneeded=()=>{
      const layouts=req.result.createObjectStore('layouts',{keyPath:'key'});
      for(const [i,scope] of scopes.entries())layouts.put({key:[...prefix,scope],used:100+i,revision:String(9+i),value});
    };
    req.onsuccess=()=>{req.result.close();resolve()};
    req.onerror=()=>reject(req.error);
  });
  const store=await DeviceStore.open(JSON.stringify(owner));
  try{
    const read=store.database.transaction('layouts').objectStore('layouts').getAll();
    const rows=await new Promise((resolve,reject)=>{read.onsuccess=()=>resolve(read.result);read.onerror=()=>reject(read.error)});
    assert.equal(rows.length,3);
    const compact=rows.find(row=>row.key[2] instanceof ArrayBuffer);
    assert.deepEqual(Array.from(new Uint8Array(compact?.key[2])),Array(16).fill(0xab));
    assert.deepEqual([compact.used,compact.value],[100,value]);
    for(const [i,scope] of scopes.entries())assert.equal((await store.read('layouts',scope)).revision,String(9+i));
    await store.apply('layouts',{kind:'dock',operation:'close',tab:Object.keys(value.state.tabs)[0]},scopes[0]);
    assert.deepEqual((await store.read('layouts',scopes[0])).value.state.tabs,{});
    assert.deepEqual((await store.read('layouts',scopes[1])).value.state.tabs,value.state.tabs);
  }finally{store.close()}
});
test('admitted resource coordinates retain their contents when the supplied descriptor changes',()=>{
 const coordinate={action:'inspect_source',source:{name:'first'}};
 const doc=act(emptyDock(),'open',{coordinate,title:'Source'});
 coordinate.source.name='second';
 assert.equal(JSON.parse(Object.values(doc.state.tabs)[0].contentId).source.name,'first');
});
test('a Dock consuming its own commit still receives changes from a peer', {timeout:5000}, async t=>{
 const first=await DeviceStore.open('dock-notifications'),peer=await DeviceStore.open('dock-notifications');
 t.after(()=>{first.close();peer.close()});
 const scope='dock:shared',fromFirst=Promise.withResolvers(),fromPeer=Promise.withResolvers();
 let firstChanges=0;
 first.subscribe('layouts',scope,()=>{firstChanges++;fromPeer.resolve()},{includeOwn:false});
 peer.subscribe('layouts',scope,fromFirst.resolve,{includeOwn:false});
 const open={kind:'dock',operation:'open',coordinate:{action:'surface',bundle:'files',name:'browser'},title:'Files',duplicate:true};
 const committed=await first.apply('layouts',open,scope);
 await fromFirst.promise;
 assert.equal(firstChanges,0);
 assert.equal(committed.revision,'1');
 await peer.apply('layouts',open,scope);
 await fromPeer.promise;
 assert.equal(firstChanges,1);
 assert.equal((await first.read('layouts',scope)).revision,'2');
});
test('no-op gestures retain the validated document identity',()=>{
 const empty=emptyDock();
 assert.equal(act(empty,'undo'),empty);assert.equal(act(empty,'redo'),empty);
 let doc=act(open(empty),'split',{pane:empty.state.activePaneId});
 assert.equal(act(doc,'split',{pane:doc.state.activePaneId}),doc);
 assert.equal(act(doc,'drop',{tab:Object.keys(doc.state.tabs)[0],pane:doc.state.activePaneId,zone:'outside'}),doc);
 for(let i=0;i<5;i++)doc=open(doc);
 for(const tab of Object.keys(doc.state.tabs).slice(0,4))doc=act(doc,'float',{tab});
 const tab=Object.keys(doc.state.tabs).find(tab=>!floatingTab(doc.state,tab));
 assert.equal(act(doc,'float',{tab}),doc);
});
test('layout moves retain identity; close and undo only restore coordinates',()=>{
 let doc=open(emptyDock());const tab=Object.keys(doc.state.tabs)[0],coordinate=doc.state.tabs[tab].contentId;
 doc=act(doc,'split',{pane:doc.state.activePaneId});const right=dockPaneIds(doc.state)[1];
 doc=act(doc,'place',{tab,pane:right,index:0});assert.equal(doc.state.tabs[tab].contentId,coordinate);
 doc=act(doc,'float',{tab});assert(floatingTab(doc.state,tab));
 doc=act(doc,'close',{tab});assert.deepEqual(Object.values(doc.state.tabs).map(t=>JSON.parse(t.contentId)),[{action:'resources'}]);
 doc=act(doc,'undo');assert.equal(doc.state.tabs[tab].contentId,coordinate);assert(floatingTab(doc.state,tab));
 assert.equal(JSON.stringify(doc).includes('ticket'),false);
 assert.deepEqual(validateDock(JSON.parse(JSON.stringify(savedDock(doc)))).state.tabs,doc.state.tabs);
 doc=act(doc,'redo');assert.deepEqual(Object.values(doc.state.tabs).map(t=>JSON.parse(t.contentId)),[{action:'resources'}]);
});
test('two horizontal panes, 20 percent split, 16 tabs and four floats are hard limits',()=>{
 let doc=open(emptyDock());doc=act(doc,'split',{pane:doc.state.activePaneId});
 doc=act(doc,'split',{pane:doc.state.activePaneId});assert.equal(dockPaneIds(doc.state).length,2);
 doc=act(doc,'resize',{split:doc.state.rootId,sizes:[.01,.99]});assert.deepEqual(doc.state.nodes[doc.state.rootId].sizes,[.2,.8]);
 while(Object.keys(doc.state.tabs).length<16)doc=open(doc);assert.throws(()=>open(doc));
 for(const tab of Object.keys(doc.state.tabs).slice(0,5))doc=act(doc,'float',{tab});assert.equal(doc.state.floats.length,4);
});
test('only validated current layouts are durable; undo belongs to the matching live window',()=>{
 const doc=open(emptyDock()),saved=savedDock(doc);
 const bad=structuredClone(saved);bad.state.nodes[bad.state.rootId].tabs=[];assert.throws(()=>validateDock(bad));
 const authority=structuredClone(saved);Object.values(authority.state.tabs)[0].contentId=JSON.stringify({action:'ui_surface',reference:{ticket:'old'}});assert.throws(()=>validateDock(authority));
 assert.throws(()=>validateDock({...saved,history:doc.history}));
 const legacy=restoreDock({...saved,version:1,history:{entries:[{unexpected:true}],cursor:1}});
 assert.deepEqual(legacy.state,saved.state);assert.equal(legacy.history.entries.length,0);
 assert.equal(restoreDock(structuredClone(saved),doc),doc);
 const current=act(doc,'close',{tab:Object.keys(doc.state.tabs)[0]});
 assert.equal(restoreDock(savedDock(current),doc).history.entries.length,0);
 assert.equal(act(current,'undo').state.tabs[Object.keys(doc.state.tabs)[0]].title,'Files');
 const next=applyIntent('layouts',savedDock(current),{kind:'dock',operation:'undo'});assert.equal(Object.keys(next.state.tabs).length,0);
 assert(!Object.hasOwn(next,'history'));
 assert.equal(boundedRecords('layouts',[],'session',next,1).records.length,1);
});

test('a second window can restore the layout without borrowing undo or overwriting newer edits',async()=>{
 const a=await DeviceStore.open('dock-window-test'),b=await DeviceStore.open('dock-window-test');
 const scope='dock:session',intent=(operation,fields={})=>({kind:'dock',operation,...fields});
 const openTab=name=>intent('open',{coordinate:{action:'surface',bundle:'files',name},title:name});
 try{
   const first=await a.apply('layouts',openTab('One'),scope);
   assert.equal(first.value.history.cursor,1);
   assert.equal((await a.read('layouts',scope,first.value)).value,first.value);
   const other=await b.read('layouts',scope);
   assert.equal(other.value.history.cursor,0);
   const second=await b.apply('layouts',openTab('Two'),scope,undefined,other.value);
   const stale=await a.apply('layouts',intent('undo'),scope,undefined,first.value);
   assert.equal(stale.revision,second.revision);
   assert.deepEqual(stale.value.state,second.value.state);
   assert.equal(stale.value.history.cursor,0);
   await assert.rejects(b.apply('layouts',intent('undo'),scope,()=>{throw Error('cleanup unavailable')},second.value),/cleanup unavailable/);
   const undone=await b.apply('layouts',intent('undo'),scope,undefined,second.value);
   assert.deepEqual(Object.values(undone.value.state.tabs).map(tab=>tab.title),['One']);
   assert.equal(undone.value.history.cursor,0);
   const row=await new Promise((resolve,reject)=>{
     const request=a.database.transaction('layouts').objectStore('layouts').get(a.scopedKey(scope));
     request.onsuccess=()=>resolve(request.result);
     request.onerror=()=>reject(request.error);
   });
   assert.deepEqual(row.value,savedDock(undone.value));
 }finally{a.close();b.close()}
});

test('moving a final tab merges its empty pane in the same undo step',()=>{
 let doc=open(emptyDock()),tab=Object.keys(doc.state.tabs)[0];
 doc=act(doc,'split',{pane:doc.state.activePaneId});const before=structuredClone(doc.state),right=dockPaneIds(doc.state)[1];
 doc=act(doc,'place',{tab,pane:right,index:0});assert.equal(dockPaneIds(doc.state).length,1);
 doc=act(doc,'undo');assert.deepEqual(doc.state,before);
});
test('splitting opens the guide in the new pane with one undo step and can still split at capacity',()=>{
  const original=open(emptyDock()),split=act(original,'split',{pane:original.state.activePaneId});
  const right=dockPaneIds(split.state)[1];
  assert.equal(split.state.activePaneId,right);
  const tab=split.state.nodes[right].activeTabId;
  assert.deepEqual(JSON.parse(split.state.tabs[tab].contentId),{action:'resources'});
  assert.deepEqual(act(split,'undo').state,original.state);
  let full=original;
  for(let i=1;i<16;i++)full=open(full);
  const atCapacity=act(full,'split',{pane:full.state.activePaneId});
  assert.equal(dockPaneIds(atCapacity.state).length,2);
  assert.equal(Object.keys(atCapacity.state.tabs).length,16);
});
test('narrow display combines strips without changing desktop tree or tab identities',()=>{
 let doc=open(emptyDock());doc=act(doc,'split',{pane:doc.state.activePaneId});
 const right=dockPaneIds(doc.state)[1];doc=act(doc,'open',{coordinate:{action:'resources'},title:'Resources',pane:right});
 const before=JSON.stringify(doc),display=narrowDock(doc.state);
 assert.equal(dockPaneIds(display).length,1);assert.equal(dockPaneIds(doc.state).length,2);
 assert.deepEqual(Object.keys(display.tabs),Object.keys(doc.state.tabs));
 assert.equal(display.nodes[display.rootId].activeTabId,doc.state.nodes[right].activeTabId);
 assert.equal(JSON.stringify(doc),before);
});
test('identity exhaustion rejects atomically before returning an unusable document',()=>{
 const doc={...emptyDock(),counter:1e14};assert.throws(()=>open(doc));assert.equal(Object.keys(doc.state.tabs).length,0);
});

test('float visibility clamps to a narrow viewport without persisting temporary bounds',()=>{
 let doc=open(emptyDock());doc=act(doc,'float',{tab:Object.keys(doc.state.tabs)[0],rect:{x:900,y:700,width:600,height:500}});
 const saved=JSON.stringify(doc),shown=displayDock(doc.state,390,844,true),rect=shown.nodes[shown.floats[0]].rect;
 assert(rect.x>=8&&rect.y>=8&&rect.x+rect.width<=382&&rect.y+rect.height<=836);
 assert.equal(JSON.stringify(doc),saved);
});

test('narrow projection preserves floating focus and the dock selection',()=>{
 let doc=open(open(emptyDock()));
 const root=doc.state.activePaneId,selected=doc.state.nodes[root].activeTabId;
 doc=act(doc,'split',{pane:root});
 const right=dockPaneIds(doc.state)[1];
 doc=act(doc,'open',{coordinate:{action:'resources'},title:'Floating',pane:right});
 const floating=doc.state.nodes[right].activeTabId;
 doc=act(doc,'open',{coordinate:{action:'ui_block',key:'other'},title:'Other',pane:right});
 doc=act(doc,'float',{tab:floating});
 const before=JSON.stringify(doc),shown=narrowDock(doc.state);
 assert.equal(shown.activePaneId,doc.state.activePaneId);
 assert.equal(shown.nodes[shown.activePaneId].activeTabId,floating);
 assert.equal(shown.nodes[shown.rootId].activeTabId,selected);
 assert.equal(JSON.stringify(doc),before);
});
