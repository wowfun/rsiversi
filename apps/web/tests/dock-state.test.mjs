import assert from 'node:assert/strict';
import {test} from 'node:test';
import {emptyDock,dockIntent,validateDock,dockPaneIds,floatingTab,narrowDock,displayDock} from '../src/dock-state.ts';
import {applyIntent,boundedRecords} from '../device-store.js';
const act=(doc,operation,fields={})=>dockIntent(doc,{kind:'dock',operation,...fields});
const open=doc=>act(doc,'open',{coordinate:{action:'surface',bundle:'files',name:'browser'},title:'Files',duplicate:true});
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
 const invalid=structuredClone(empty);invalid.history.cursor=1;
 assert.throws(()=>act(invalid,'undo'),/Invalid resource layout/);
});
test('layout moves retain identity; close and undo only restore coordinates',()=>{
 let doc=open(emptyDock());const tab=Object.keys(doc.state.tabs)[0],coordinate=doc.state.tabs[tab].contentId;
 doc=act(doc,'split',{pane:doc.state.activePaneId});const right=dockPaneIds(doc.state)[1];
 doc=act(doc,'place',{tab,pane:right,index:0});assert.equal(doc.state.tabs[tab].contentId,coordinate);
 doc=act(doc,'float',{tab});assert(floatingTab(doc.state,tab));
 doc=act(doc,'close',{tab});assert.equal(Object.keys(doc.state.tabs).length,0);
 doc=act(doc,'undo');assert.equal(doc.state.tabs[tab].contentId,coordinate);assert(floatingTab(doc.state,tab));
 assert.equal(JSON.stringify(doc).includes('ticket'),false);
 assert.deepEqual(validateDock(JSON.parse(JSON.stringify(doc))).state.tabs,doc.state.tabs);
 doc=act(doc,'redo');assert.equal(Object.keys(doc.state.tabs).length,0);
});
test('two horizontal panes, 20 percent split, 16 tabs and four floats are hard limits',()=>{
 let doc=open(emptyDock());doc=act(doc,'split',{pane:doc.state.activePaneId});
 doc=act(doc,'split',{pane:doc.state.activePaneId});assert.equal(dockPaneIds(doc.state).length,2);
 doc=act(doc,'resize',{split:doc.state.rootId,sizes:[.01,.99]});assert.deepEqual(doc.state.nodes[doc.state.rootId].sizes,[.2,.8]);
 for(let i=1;i<16;i++)doc=open(doc);assert.throws(()=>open(doc));
 for(const tab of Object.keys(doc.state.tabs).slice(0,5))doc=act(doc,'float',{tab});assert.equal(doc.state.floats.length,4);
});
test('durable input rejects cycles, hidden tabs, authorities and invalid history before replay',()=>{
 const doc=open(emptyDock());const bad=structuredClone(doc);bad.state.nodes[bad.state.rootId].tabs=[];assert.throws(()=>validateDock(bad));
 const authority=structuredClone(doc);Object.values(authority.state.tabs)[0].contentId=JSON.stringify({action:'ui_surface',reference:{ticket:'old'}});assert.throws(()=>validateDock(authority));
 const history=structuredClone(doc);history.history.entries[0].inverse=[{type:'openTab',paneId:doc.state.activePaneId,tab:{id:'tab999',kind:'rsi',contentId:'null',title:'bad'},index:0}];assert.throws(()=>validateDock(history));
 const current=act(doc,'close',{tab:Object.keys(doc.state.tabs)[0]});
 const next=applyIntent('layouts',current,{kind:'dock',operation:'undo'});assert.equal(Object.keys(next.state.tabs).length,1);
 assert.equal(boundedRecords('layouts',[],'session',next,1).records.length,1);
});

test('moving a final tab merges its empty pane in the same undo step',()=>{
 let doc=open(emptyDock()),tab=Object.keys(doc.state.tabs)[0];
 doc=act(doc,'split',{pane:doc.state.activePaneId});const before=structuredClone(doc.state),right=dockPaneIds(doc.state)[1];
 doc=act(doc,'place',{tab,pane:right,index:0});assert.equal(dockPaneIds(doc.state).length,1);
 doc=act(doc,'undo');assert.deepEqual(doc.state,before);
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
