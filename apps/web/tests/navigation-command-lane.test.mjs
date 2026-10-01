import {test} from 'node:test';
import assert from 'node:assert/strict';
import {NavigationCommandLane} from '../navigation-command-lane.js';
test('view publication cannot dispatch a summary ahead of the preceding acknowledgment',async()=>{
  const lane=new NavigationCommandLane(),events=[];let release;
  const first=lane.run(async()=>{events.push('seed published');await new Promise(resolve=>{release=resolve});events.push('seed acknowledged')});
  await Promise.resolve();
  const summary=lane.run(()=>events.push('summary dispatched'));
  await Promise.resolve();assert.deepEqual(events,['seed published']);
  release();await Promise.all([first,summary]);assert.deepEqual(events,['seed published','seed acknowledged','summary dispatched']);
});
test('a failed command is not replayed and stale queued work can retire before dispatch',async()=>{
  const lane=new NavigationCommandLane();let calls=0,current=1;
  const first=lane.run(()=>{calls++;throw new Error('unknown')});
  const expected=current;const next=lane.run(()=>{if(current===expected)calls++});current=2;
  await assert.rejects(first,/unknown/);await next;assert.equal(calls,1);
  await lane.run(()=>calls++);assert.equal(calls,2);
});
test('queued closures have a finite bound',async()=>{
  const lane=new NavigationCommandLane();let release;
  const first=lane.run(()=>new Promise(resolve=>{release=resolve}));await Promise.resolve();
  const rest=Array.from({length:31},()=>lane.run(()=>{}));await assert.rejects(lane.run(()=>{}),/capacity/);
  release();await Promise.all([first,...rest]);
});
