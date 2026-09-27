import {test} from 'node:test';
import assert from 'node:assert/strict';
import {navigationPresentation,GroupRestoration} from '../navigation-presentation.js';
const row=id=>({session:id,created_at_ms:'1700000000000',path:'/same',metadata:{title:null}});
test('untitled rows use creation time and distinguish same-time IDs across duplicate pages',()=>{
  const first=row('same-prefix-first'),second=row('same-prefix-second');
  const {titles}=navigationPresentation([first,second,first,{...row('named'),metadata:{title:'Named'}},{...row('invalid'),created_at_ms:'18446744073709551615'}]);
  assert.notEqual(titles.get(first.session),titles.get(second.session));
  assert(!titles.get(first.session).includes('/same'));
  assert.equal(titles.get('named'),'Named');
  assert.match(titles.get('invalid'),/^Created /);
});
test('attention absent from the native index remains unknown',()=>{
  const {attention}=navigationPresentation([],{entries:[{position:{conversation:{kind:'external',id:'a'}},status:'running'},{position:{conversation:{kind:'native',id:'b'}},status:'waiting'}]});
  assert.equal(attention.get('a')??'unknown','unknown');
  assert.equal(attention.get('b'),'waiting');
});
test('large groups read each entry once and produce unique titles',()=>{
  let reads=0;
  const entries=Array.from({length:2048},(_,i)=>({...row(`same-prefix-${i}`),get metadata(){reads++;return {title:null};}}));
  const {titles}=navigationPresentation(entries);
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
