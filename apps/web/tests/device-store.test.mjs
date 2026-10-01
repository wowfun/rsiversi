import assert from 'node:assert/strict';
import {test} from 'node:test';
import {applyIntent,boundedRecords,reconcileOrder,defaultPreferences,budgets} from '../device-store.js';
import {defaultLayout} from '../presentation-store.js';
const members=Array.from({length:65},(_,i)=>({id:`s-${String(i).padStart(2,'0')}`,partition:'local/project/unpinned'}));
const move=(id,position,relative=null,list=members)=>({kind:'move',members:list,id,position,relative});
test('complete membership reconciles before cross-page moves and rejects partition crossing',()=>{
  const all=reconcileOrder(['s-64','removed'],members);
  assert.equal(all.length,66);assert.equal(all[0],'s-64');assert.equal(all[1],'removed');
  let order=applyIntent('orders',{ids:members.map(m=>m.id)},move('s-64','first'));
  assert.equal(order.ids[0],'s-64');
  order=applyIntent('orders',order,move('s-64','last'));
  assert.equal(order.ids[64],'s-64');
  order=applyIntent('orders',order,move('s-63','next'));
  assert.deepEqual(order.ids.slice(-2),['s-64','s-63']);
  const partitioned=[...members,{id:'remote',partition:'ssh/project/unpinned'},{id:'pin',partition:'local/project/pinned'}];
  for(const relative of ['remote','pin'])assert.throws(()=>applyIntent('orders',order,move('s-64','after',relative,partitioned)),/partitions/);
  assert.throws(()=>reconcileOrder([],Array.from({length:1025},(_,i)=>({id:`s-${i}`,partition:'group'}))),/membership/);
  assert.throws(()=>applyIntent('orders',order,{kind:'reconcile',members:null}),/membership/);
  assert.deepEqual(applyIntent('orders',order,{kind:'updated'}),{ids:[]});
});
test('semantic intents preserve unrelated values and reject malformed saves',()=>{
  let layout=applyIntent('layouts',defaultLayout,{kind:'patch',patch:{navigationWidth:300}});
  layout=applyIntent('layouts',layout,{kind:'workspace',id:'one',expanded:true});
  layout=applyIntent('layouts',layout,{kind:'workspace',id:'two',expanded:true});
  layout=applyIntent('layouts',layout,{kind:'patch',patch:{resourcesWidth:.35}});
  assert.equal(layout.navigationWidth,300);assert.equal(layout.resourcesWidth,.35);assert.equal(layout.workspaces.length,2);
  for(const intent of [{kind:'patch',patch:{navigation:'nope'}},{kind:'patch',patch:{version:3}},{kind:'patch',patch:{workspaces:[]}},{kind:'workspace',expanded:true}])assert.throws(()=>applyIntent('layouts',layout,intent));
  assert.equal(applyIntent('preferences',defaultPreferences,{kind:'patch',patch:{view:'workspace_tree'}}).sessionOrder,'updated');
});
test('each store independently enforces record, cardinality, bytes and monotonic revision',()=>{
  for(const [bucket,value] of [['preferences',defaultPreferences],['layouts',defaultLayout],['orders',{ids:members.map(m=>m.id)}]]){
    const count=budgets[bucket].count;
    let records=Array.from({length:count},(_,i)=>({key:`key-${i}`,value,revision:'1',used:i}));
    records=boundedRecords(bucket,records,'key-0',value,100).records;
    const next=boundedRecords(bucket,records,'new',value,101);
    assert.equal(next.records.length,count);assert(next.records.some(r=>r.key==='key-0'));assert(!next.records.some(r=>r.key==='key-1'));
    assert.equal(records.find(r=>r.key==='key-0').revision,'2');
    assert.throws(()=>boundedRecords(bucket,[records[0],records[0]],'new',value,102));
  }
  const tooLarge={ids:Array.from({length:1024},(_,i)=>`${String(i).padStart(4,'0')}${'x'.repeat(252)}`)};
  assert.throws(()=>boundedRecords('orders',[],'large',tooLarge,1),/too large/);
  assert.throws(()=>boundedRecords('preferences',[{key:'one',value:defaultPreferences,revision:'18446744073709551615',used:1}],'one',defaultPreferences,2),/exhausted/);
});

test('workspace tree partitions machines before ancestry and keeps foreign path roots',async()=>{
  const {workspaceTree}=await import('../workspace-tree.js');
  const entries=[
    {id:'local',path:'/project',location:{kind:'local'}},
    {id:'child',path:'/project/child',location:{kind:'local'}},
    {id:'remote',path:'/project',location:{kind:'ssh',target:'a'.repeat(32)}},
    {id:'windows',path:'C:\\project',location:{kind:'local'}},
  ];
  const tree=workspaceTree(entries);
  assert.equal(tree.length,2);
  const local=tree.find(group=>group.key==='local');
  const project=local.children.find(node=>node.label==='project');
  assert.equal(project.workspace.id,'local');assert.equal(project.children[0].workspace.id,'child');
  assert.equal(local.children.find(node=>node.label==='C:').children[0].workspace.id,'windows');
  assert.equal(tree.find(group=>group.key.startsWith('ssh:')).children[0].workspace.id,'remote');
});

test('workspace order preserves exact location and immediate parent partitions',async()=>{
  const {workspaceMembers,workspaceTree}=await import('../workspace-tree.js');
  const workspaces=[
    {id:'a',path:'/root/a',location:{kind:'local'}},
    {id:'b',path:'/root/b',location:{kind:'local'}},
    {id:'child',path:'/root/a/child',location:{kind:'local'}},
    {id:'remote',path:'/root/a',location:{kind:'ssh',target:'a'.repeat(32)}},
  ];
  const complete=workspaceMembers(workspaces);
  const order=applyIntent('orders',{ids:[]},move('b','first',null,complete));
  assert.deepEqual(order.ids,['b','a','child','remote']);
  for(const relative of ['child','remote'])assert.throws(()=>applyIntent('orders',order,move('a','after',relative,complete)),/partitions/);
  const tree=workspaceTree(workspaces,order.ids);
  assert.deepEqual(tree.find(group=>group.key==='local').children[0].children.map(node=>node.workspace.id),['b','a']);
});

test('bucket accounting does not replay another Sessions history or rewrite its value',()=>{
  const unrelated={key:'dock:other',value:{kind:'session-dock',history:{invalid:'unread'}},revision:'8',used:1};
  const next=boundedRecords('layouts',[unrelated],'current',defaultLayout,2);
  assert.equal(next.records[0],unrelated);
  assert.throws(()=>applyIntent('layouts',unrelated.value,{kind:'dock',operation:'undo'}),/Invalid resource layout/);
  assert.throws(()=>boundedRecords('layouts',[{...unrelated,value:'x'.repeat(32768)}],'current',defaultLayout,2),/too large/);
  assert.throws(()=>boundedRecords('layouts',[{...unrelated,revision:'invalid'}],'current',defaultLayout,2),/record/);
});

test('POSIX backslashes cannot collapse distinct workspaces or their order partitions', async()=>{
  const {workspaceMembers,workspaceTree}=await import('../workspace-tree.js');
  const entries=[
    {id:'literal',path:'/root/a\\b',location:{kind:'local'}},
    {id:'nested',path:'/root/a/b',location:{kind:'local'}},
    {id:'sibling',path:'/root/c',location:{kind:'local'}},
  ];
  const tree=workspaceTree(entries)[0].children[0];
  assert.equal(tree.children.find(node=>node.label==='a\\b').workspace.id,'literal');
  assert.equal(tree.children.find(node=>node.label==='a').children[0].workspace.id,'nested');
  const members=workspaceMembers(entries);
  assert.equal(members[0].partition,members[2].partition);
  assert.notEqual(members[0].partition,members[1].partition);
});

test('visibility reconciliation preserves absent positions without granting move authority',()=>{
 const original={ids:['b','hidden','a']},visible=[{id:'a',partition:'local'},{id:'b',partition:'local'}];
 const hidden=applyIntent('orders',original,{kind:'reconcile',members:visible});
 assert.deepEqual(hidden,original);
 assert.throws(()=>applyIntent('orders',hidden,move('hidden','first',null,visible)),/intent/);
 const restored=applyIntent('orders',hidden,{kind:'reconcile',members:[...visible,{id:'hidden',partition:'local'}]});
 assert.deepEqual(restored,original);
 const full={ids:Array.from({length:1024},(_,i)=>`saved-${i}`)};
 assert.throws(()=>applyIntent('orders',full,{kind:'reconcile',members:visible}),/select Updated/);
 assert.equal(full.ids.length,1024);
 assert.deepEqual(applyIntent('orders',full,{kind:'updated'}),{ids:[]});
});
