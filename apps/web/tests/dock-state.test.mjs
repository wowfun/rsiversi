import assert from 'node:assert/strict';
import {test} from 'node:test';
import {emptyDock,dockIntent,savedDock,validateDock,dockStorageKey,readDock,writeDock,dockPaneIds,floatingTab,narrowDock,displayDock} from '../src/dock-state.ts';
const act=(doc,operation,fields={})=>dockIntent(doc,{kind:'dock',operation,...fields});
const open=doc=>act(doc,'open',{coordinate:{action:'surface',bundle:'files',name:'browser'},title:'Files',duplicate:true});
test('revealing a resource from a collapsed column is one undoable edit',()=>{
 const start=emptyDock();
 assert.equal(start.state.expanded,false);
 const shown=act(start,'open',{coordinate:{action:'workspace_document',path:'report.md'},title:'Report'});
 assert.equal(shown.history.entries.length,1);
 assert.equal(shown.state.expanded,true);
 assert.deepEqual(act(shown,'undo').state,start.state);
 const closed=act(shown,'expanded',{expanded:false});
 assert.deepEqual(closed.state.tabs,shown.state.tabs);
 assert.equal(act(closed,'expanded',{expanded:false}),closed);
});
test('exiting automatic fullscreen closes the dock and resets manual fullscreen in the same edit',()=>{
 const opened=open(emptyDock());
 const manual=act(opened,'toggle-fullscreen');
 assert.equal(manual.state.mode,'fullscreen');
 assert.deepEqual(act(manual,'toggle-fullscreen').state,opened.state);
 for(const before of [opened,manual]){
   const closed=act(before,'toggle-fullscreen',{automatic:true});
   assert.deepEqual(closed.state,{...opened.state,expanded:false});
   assert.deepEqual(act(closed,'undo').state,before.state);
 }
});
test('opening, copying or moving a guide respects its destination pane',()=>{
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
});
test('admitted resource coordinates retain their contents when the supplied descriptor changes',()=>{
 const coordinate={action:'inspect_source',source:{name:'first'}};
 const doc=act(emptyDock(),'open',{coordinate,title:'Source'});
 coordinate.source.name='second';
 assert.equal(JSON.parse(Object.values(doc.state.tabs)[0].contentId).source.name,'first');
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
test('restoration validates coordinates, isolates principals and starts fresh window history',t=>{
 const entries=new Map();
 const previous=Object.getOwnPropertyDescriptor(globalThis,'localStorage');
 Object.defineProperty(globalThis,'localStorage',{configurable:true,value:{
   getItem:key=>entries.get(key)??null,setItem:(key,value)=>entries.set(key,value),
 }});
 t.after(()=>{if(previous)Object.defineProperty(globalThis,'localStorage',previous);else delete globalThis.localStorage});
 const key=dockStorageKey('device-a','session');
 const a=open(readDock(key)),peer=open(open(readDock(key)));
 writeDock(key,a);writeDock(key,peer);
 assert.equal(Object.keys(a.state.tabs).length,1);
 assert.equal(Object.keys(readDock(key).state.tabs).length,2);
 assert.equal(readDock(key).history.entries.length,0);
 assert.deepEqual(readDock(dockStorageKey('device-b','session')).state.tabs,{});
 const invalid=structuredClone(savedDock(a));
 Object.values(invalid.state.tabs)[0].contentId=JSON.stringify({action:'ui_surface',reference:{ticket:'old'}});
 entries.set(key,JSON.stringify(invalid));
 assert.deepEqual(readDock(key).state.tabs,{});
 entries.set(key,'x'.repeat(32769));assert.deepEqual(readDock(key).state.tabs,{});
 assert(!entries.has(dockStorageKey('device-b','session')));
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

test('float inputs preserve restorable geometry through edits and undo',()=>{
 const start=open(emptyDock()),tab=Object.keys(start.state.tabs)[0];
 const rect={x:20,y:30,width:300,height:200};
 const floated=act(start,'float',{tab,rect}),pane=floated.state.activePaneId;
 rect.width=0;
 assert.equal(floated.state.nodes[pane].rect.width,300);
 for(const bad of [{...rect,width:99},{...rect,width:300,y:NaN},{...rect,width:300,extra:1}]){
   assert.throws(()=>act(start,'float',{tab,rect:bad}));
   assert.throws(()=>act(floated,'resize-float',{pane,rect:bad}));
   const snapshot=structuredClone(savedDock(floated));
   snapshot.state.nodes[pane].rect=bad;
   assert.throws(()=>validateDock(snapshot));
 }
 assert.throws(()=>act(floated,'move-float',{pane,x:100001,y:0}));
 assert.throws(()=>act(floated,'move-float',{pane,x:0,y:Infinity}));
 const resizedRect={x:-5,y:8,width:400,height:290};
 const resized=act(floated,'resize-float',{pane,rect:resizedRect});
 resizedRect.height=0;
 const undone=act(resized,'undo'),redone=act(undone,'redo');
 assert.deepEqual(undone.state,floated.state);
 assert.deepEqual(redone.state,resized.state);
 assert.equal(validateDock(savedDock(redone)).state.nodes[pane].rect.height,290);
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
