import {test} from 'node:test';
import assert from 'node:assert/strict';
import {navigationTitles,navigationAttention,GroupRestoration,conversationRecency,sessionWorkspaceGroup,sidebarConversations,workspaceRowWindow} from '../navigation-presentation.js';
test('conversation recency handles future clocks and minute/hour/day boundaries',()=>{
  const now=1_000_000_000;
  for(const [age,label] of [[-1,'now'],[59_999,'now'],[60_000,'1min'],[3_600_000,'1h'],[86_400_000,'1d']])assert.equal(conversationRecency(String(now-age),now),label);
  assert.equal(conversationRecency('bad',now),'');
});
const row=id=>({session:id,created_at_ms:'1700000000000',path:'/same',metadata:{title:null}});
test('an explicit search result reveals its row beyond the automatic limit',()=>{
  const rows=Array.from({length:60},(_,i)=>row(String(i+1)));
  assert.equal(workspaceRowWindow(rows,'58',new Map(),5,'58').visible,58);
  assert.equal(workspaceRowWindow(rows,'58',new Map(),5).visible,5);
  assert.equal(workspaceRowWindow(rows,'3',new Map(),5,'3').visible,5);
});
test('only the selected provisional conversation leads its window beside five established rows',()=>{
  const started=Array.from({length:6},(_,i)=>row(String(i)));
  const queued={...row('queued'),blank:true,metadata:{title:'Queued message'}};
  const empty={...row('empty'),blank:true};
  const members=[...started,queued,empty];
  assert.deepEqual(sidebarConversations(members,'queued'),[queued,...started]);
  assert.deepEqual(sidebarConversations(members,'empty'),[empty,...started]);
  assert.deepEqual(sidebarConversations(members,undefined),started);
  const listed=sidebarConversations(members,'queued');
  const {visible}=workspaceRowWindow(listed,'queued',new Map(),5);
  assert.deepEqual(listed.slice(0,visible),[queued,...started.slice(0,5)]);
  assert.deepEqual(members,[...started,queued,empty]);
  assert.equal(navigationTitles(members).has('queued'),false);
});
test('the workspace reveals the loaded prefix through selected and attention-bearing rows in the first fifty',()=>{
  const rows=Array.from({length:60},(_,index)=>row(String(index+1)));
  const attention=new Map([['12',{status:'unread'}],['15',{status:'waiting'}],['18',{status:'running'}],['22',{status:'unknown'}],['51',{status:'running'}]]);
  assert.deepEqual(workspaceRowWindow(rows,'7',attention,5),{minimum:18,limit:18,visible:18,canFold:true});
  assert.equal(workspaceRowWindow(rows,'50',attention,5).visible,50);
  assert.equal(workspaceRowWindow(rows,'51',attention,5).visible,18);
  assert.deepEqual(workspaceRowWindow(rows,'1',new Map(),18),{minimum:5,limit:18,visible:18,canFold:true});
  assert.equal(workspaceRowWindow(rows,undefined,attention,Infinity).visible,60);
});
test('untitled rows use creation time and distinguish same-time IDs across duplicate pages',()=>{
  const first=row('same-prefix-first'),second=row('same-prefix-second');
  const titles=navigationTitles([first,second,first,{...row('named'),metadata:{title:'Named'}},{...row('invalid'),created_at_ms:'18446744073709551615'}]);
  assert.notEqual(titles.get(first.session),titles.get(second.session));
  assert(!titles.get(first.session).includes('/same'));
  assert.equal(titles.get('named'),'Named');
  assert.match(titles.get('invalid'),/^Created /);
});
test('absent native attention stays distinct from an observed unknown state',()=>{
  const attention=navigationAttention({entries:[{position:{conversation:{kind:'external',id:'a'}},status:'running'},{position:{conversation:{kind:'native',id:'b'}},status:'waiting'},{position:{conversation:{kind:'native',id:'c'}},status:'unknown'}]});
  assert.equal(attention.get('a'),undefined);
  assert.equal(attention.get('b').status,'waiting');
  assert.equal(attention.get('c').status,'unknown');
  assert.equal(attention.get('unobserved'),undefined);
});
test('large groups read each entry once and produce unique titles',()=>{
  let reads=0;
  const entries=Array.from({length:2048},(_,i)=>({...row(`same-prefix-${i}`),get metadata(){reads++;return {title:null};}}));
  const titles=navigationTitles(entries);
  assert.equal(reads,entries.length);
  assert.equal(new Set(titles.values()).size,entries.length);
});
test('group restoration bounds concurrent work and releases failed and completed attempts',async()=>{
  const gate=new GroupRestoration();let calls=0,release;
  const wait=new Promise(resolve=>{release=resolve;});
  const load=()=>{calls++;return wait;};
  const pending=Array.from({length:16},(_,i)=>gate.run(String(i),load));
  await gate.run('0',load);await gate.run('overflow',load);
  assert.equal(calls,16);release();await Promise.all(pending);
  assert.equal(gate.pending.size,0);
  await assert.rejects(gate.run('0',()=>Promise.reject(new Error('offline'))));
  await gate.run('0',async()=>{calls++;});
  assert.equal(calls,17);assert.equal(gate.pending.size,0);
});

test('the sidebar uses workspace membership for same-directory Agents, private drafts and forks',()=>{
  const surface={kind:'native',session:'agent',draft:true,workspace:{registration:{status:'registered',id:'work'}}};
  const navigation={entries:[{session:'chat',workspace:'work'},{session:'agent',workspace:null}],pins:[],groups:{}};
  assert.equal(sessionWorkspaceGroup(surface,navigation),'other');
  assert.equal(sessionWorkspaceGroup(surface,{...navigation,entries:[]}),undefined);
  const fresh={...surface,session:'fresh',creation:{workspace_id:'work'}};
  assert.equal(sessionWorkspaceGroup(fresh,navigation),'work');
  const fork=source=>({...fresh,creation:{workspace_id:'work',fork:{source_session_id:source}}});
  assert.equal(sessionWorkspaceGroup(fork('agent'),navigation),undefined);
  assert.equal(sessionWorkspaceGroup(fork('chat'),navigation),'work');
});
