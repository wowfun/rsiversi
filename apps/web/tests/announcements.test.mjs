import {test} from 'node:test';
import assert from 'node:assert/strict';
import {ConversationAnnouncements} from '../src/session/announcements.js';

test('the first selected live frame announces its current rejection without replaying history', () => {
  const observer = new ConversationAnnouncements();
  const frame = {session:'one',transcript:{turns:{entries:{old:{id:'old',terminal:'done',status:'completed'}}}},pending:[],projections:{generation_rejection:'Unsupported checkpoint'}};
  assert.equal(observer.update(frame,true).text,'Plugin update failed; previous plugins remain in use. Unsupported checkpoint');
  assert.equal(observer.update(structuredClone(frame),true),undefined);
});

test('a pane first observed in the background announces outstanding attention when selected', () => {
  const observer = new ConversationAnnouncements();
  const frame = {session:'background',transcript:{turns:{entries:{}}},pending:[{kind:'approval',owner:'tool',id:'new',title:'Approve'}]};
  assert.equal(observer.update(frame,false),undefined);
  assert.equal(observer.update(frame,true)?.text,'Approve. Your attention is needed.');
});
import {mergePresentationFrame} from '../src/runtime/presentation-frame.js';

test('completion and attention announce once, without replaying history, reconnects or background activity', () => {
  const observer = new ConversationAnnouncements();
  const frame = {session:'one',transcript:{turns:{entries:{old:{id:'old',terminal:'completed',status:'completed'}}}},pending:[]};
  assert.equal(observer.update(frame,true), undefined);
  frame.transcript.turns = {entries:{...frame.transcript.turns.entries,next:{id:'next',terminal:'completed',status:'completed'}}};
  assert.match(observer.update(frame,true).text, /completed/);
  assert.equal(observer.update(frame,true), undefined);
  assert.equal(observer.update(undefined,true), undefined);
  assert.equal(observer.update(frame,true), undefined);
  frame.pending = [{id:'approval',kind:'approval',owner:'tool',title:'Approve file write'}];
  assert.match(observer.update(frame,true).text, /Approve file write/);
  frame.transcript.turns = {entries:{...frame.transcript.turns.entries,background:{id:'background',terminal:'failed',status:'failed'}}};
  assert.equal(observer.update(frame,false), undefined);
  assert.equal(observer.update(frame,true), undefined);
  assert.equal(observer.update({...frame, session:'two'},true), undefined);
});

test('text patches do not rescan turn history; same-count terminal changes and reconnect snapshots remain observable', () => {
  let scans=0;
  const entries=new Proxy(Object.fromEntries(Array.from({length:1000},(_,i)=>[String(i),{id:String(i),terminal:i===999?null:'completed',status:i===999?'running':'completed'}])),{
    ownKeys(target){scans++;return Reflect.ownKeys(target);},
  });
  let view={surfaces:{main:{kind:'native',session:'one',pending:[],transcript:{turns:{revision:'1',entries},blocks:[{key:'active',text:''}]}}}};
  const observer=new ConversationAnnouncements();
  assert.equal(observer.update(view.surfaces.main,true),undefined);
  assert.equal(scans,1);
  for(let i=1;i<=20;i++) {
    view=mergePresentationFrame(view,String(i),{kind:'patch',frame_id:String(i+1),base_frame_id:String(i),sections:{},surfaces:[{
      surface:'main',fields:{},transcript:{fields:{},remove:[],upsert:[{key:'active',text:`delta ${i}`}]},
    }]});
    assert.equal(observer.update(view.surfaces.main,true),undefined);
  }
  assert.equal(scans,1,'streaming text must not copy or scan the turn registry');
  const previous=view.surfaces.main.transcript.turns;
  view=mergePresentationFrame(view,'21',{kind:'patch',frame_id:'22',base_frame_id:'21',sections:{},surfaces:[{
    surface:'main',fields:{},transcript:{fields:{},remove:[],upsert:[],turns:{revision:'2',remove:[],upsert:{999:{id:'999',terminal:'completed',status:'completed'}}}},
  }]});
  assert.equal(previous.entries[999].terminal,null);
  assert.equal(observer.update(view.surfaces.main,true).text,'Response completed.');
  assert.equal(observer.update(undefined,true),undefined);
  const snapshot=structuredClone(view);
  assert.equal(observer.update(snapshot.surfaces.main,true),undefined);
  snapshot.surfaces.main.pending=[{kind:'approval',owner:'tool',id:'same-count',title:'Approve write'}];
  assert.match(observer.update(snapshot.surfaces.main,true).text,/Approve write/);
  snapshot.surfaces.main.pending=[{kind:'approval',owner:'tool',id:'replacement',title:'Approve replacement'}];
  assert.match(observer.update(snapshot.surfaces.main,true).text,/Approve replacement/);
});

test('historical windows do not seed the live announcement baseline', () => {
  const observer=new ConversationAnnouncements();
  const data={session:'one',historical:true,transcript:{turns:{entries:{}}},pending:[]};
  assert.equal(observer.update(data,true),undefined);
  data.historical=false;
  data.transcript.turns={entries:{done:{id:'done',terminal:'completed',status:'completed'}}};
  assert.equal(observer.update(data,true),undefined);
});

test('window removal and reconnect do not reannounce a known terminal or request', () => {
  const observer=new ConversationAnnouncements();
  const terminal={id:'turn',terminal:'{"kind":"terminal","turn":"turn"}',status:'Completed'};
  const request={id:'approval',kind:'approval',owner:'one',title:'Approve write'};
  const frame={session:'one',transcript:{turns:{entries:{}}},pending:[]};
  observer.update(frame,true);
  frame.transcript.turns={entries:{turn:terminal}};
  frame.pending=[request];
  assert.equal(observer.update(frame,true).text,'Response completed. Approve write. Your attention is needed.');
  frame.transcript.turns={entries:{}}; frame.pending=[];
  assert.equal(observer.update(frame,true),undefined);
  frame.transcript.turns={entries:{turn:terminal}}; frame.pending=[request];
  assert.equal(observer.update(frame,true),undefined);
  observer.update({...frame,session:'two'},true);
  assert.equal(observer.update({...frame,session:'one'},true),undefined);
});

test('pending background attention announces on selection while resolved requests and completions stay silent', () => {
  const observer=new ConversationAnnouncements();
  const frame={session:'one',transcript:{turns:{entries:{}}},pending:[]};
  observer.update(frame,true);
  frame.pending=[{kind:'approval',owner:'tool',id:'gone',title:'Expired.'}];
  assert.equal(observer.update(frame,false),undefined);
  frame.pending=[{kind:'approval',owner:'tool',id:'current',title:'Approve write!'}];
  frame.transcript.turns={entries:{done:{id:'done',terminal:'done',status:'Completed'}}};
  assert.equal(observer.update(frame,false),undefined);
  assert.equal(observer.update(frame,true).text,'Approve write. Your attention is needed.');
  assert.equal(observer.update(frame,true),undefined);
});

test('new generation failures join polite announcements and clear without replay', () => {
  const observer=new ConversationAnnouncements();
  const frame={session:'one',transcript:{turns:{entries:{}}},pending:[],projections:{}};
  observer.update(frame,true);
  frame.projections={generation_rejection:'Unsupported checkpoint'};
  assert.match(observer.update(frame,true).text,/Plugin update failed.*Unsupported checkpoint/);
  assert.equal(observer.update(frame,true),undefined);
  frame.projections={}; assert.equal(observer.update(frame,true),null);
  frame.projections={generation_rejection:'Unsupported checkpoint'};
  assert.match(observer.update(frame,true).text,/Unsupported checkpoint/);
  frame.transcript.turns={entries:{failed:{id:'failed',terminal:'failed',status:'tool_failed'}}};
  assert.equal(observer.update(frame,true).text,'Response tool failed.');
});

test('outstanding generation rejection first seen in background announces on selection', () => {
  const observer = new ConversationAnnouncements();
  const frame = {session:'background',transcript:{turns:{entries:{}}},pending:[],projections:{generation_rejection:'Bad state'}};
  assert.equal(observer.update(frame,false), undefined);
  assert.match(observer.update(frame,true).text, /Bad state/);
  observer.update(frame,false);
  assert.equal(observer.update(frame,true), undefined);
  frame.projections={}; observer.update(frame,false);
  frame.projections={generation_rejection:'Bad state'}; observer.update(frame,false);
  assert.match(observer.update(frame,true).text, /Bad state/);
  frame.projections={generation_rejection:'Fixed before selection'}; observer.update(frame,false);
  frame.projections={}; observer.update(frame,false);
  assert.equal(observer.update(frame,true), undefined);
});
