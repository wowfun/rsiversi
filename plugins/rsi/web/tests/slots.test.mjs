import assert from 'node:assert/strict';
import {test} from 'node:test';
import {SlotCore} from '../vendor/dsh/slots/index.ts';

test('parent disposal retires child authority and stale disposal cannot remove its replacement',async()=>{
  const core=new SlotCore();
  assert.throws(()=>core.register({name:'resources'},()=>null),/not declared/);
  const root=core.register({name:'root',children:{resources:{kind:'single',scope:'session'}}},()=>null);
  const child=core.register({name:'resources',children:{details:{kind:'list',scope:'session'}}},()=>null);
  core.register({name:'details',id:'one'},()=>null);
  const retained=core.entries('resources')[0],epoch=core.declarationEpoch('resources');
  root();assert(!core.isLive(retained));assert.equal(core.entries('details').length,0);
  const next=core.register({name:'root',children:{resources:{kind:'single',scope:'session'}}},()=>null);
  core.register({name:'resources'},()=>null);child();
  assert.equal(core.entriesOfSlot('resources').length,1);assert(core.declarationEpoch('resources')>epoch);
  next();await Promise.resolve();assert.equal(core.entriesOfSlot('resources').length,0);
});

test('a crashing elected component yields to the next priority without changing declared scope',()=>{
  const core=new SlotCore();
  core.register({name:'root',children:{navigation:{kind:'single',scope:'root'}}},()=>null);
  core.register({name:'navigation',priority:0},()=>null);
  core.register({name:'navigation',priority:1},()=>null);
  const original=core.entriesOfSlot('navigation')[0];
  core.reportEntryError('navigation',original,new Error('fixture'),{abdicate:true});
  assert.notEqual(core.entriesOfSlot('navigation')[0],original);assert(core.isLive(original));
  assert.deepEqual(core.specDynamic('navigation'),{kind:'single',scope:'root'});
});
