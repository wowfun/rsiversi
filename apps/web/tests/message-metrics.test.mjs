import test from 'node:test';
import assert from 'node:assert/strict';
import {messageClock, usageReadings, tokenTotal, elapsedLabel, tokenCount, durationReading} from '../message-metrics.js';
import {visibleConversations} from '../navigation-presentation.js';

test('missing provider metrics remain absent and inclusive input is not double counted',()=>{
  assert.deepEqual(usageReadings(undefined),{total:undefined,cache:undefined,speed:undefined,context:undefined});
  const tokens={input_tokens:100,output_tokens:20,cache_read_tokens:80};
  assert.equal(tokenTotal(tokens),120);
  const summary={reported_attempts:1,tokens,timing:{decode_ms:2000,decode_output_tokens:20},last_context:{input_tokens:100}};
  assert.deepEqual(usageReadings(summary,1000),{total:120,cache:80,speed:10,context:10});
  assert.equal(usageReadings({...summary,timing:{decode_ms:0,decode_output_tokens:20}},1000).speed,undefined);
  assert.equal(usageReadings({...summary,tokens:{...tokens,cache_read_tokens:null}}).cache,undefined);
});
test('clocks reject absent or out-of-range evidence and elapsed time permits zero',()=>{
  for(const timestamp of [undefined,null,0,Number.MAX_VALUE,'bad'])assert.equal(messageClock(timestamp),null);
  assert.equal(messageClock(1000).iso,'1970-01-01T00:00:01.000Z');
  assert.equal(elapsedLabel(0),'0s');assert.equal(elapsedLabel(62000),'1m 2s');
});
test('collapsed workspace keeps its selected conversation and preserves order',()=>{
  const entries=Array.from({length:9},(_,i)=>({session:String(i)}));
  assert.deepEqual(visibleConversations(entries,false,'7').map(x=>x.session),['0','1','2','3','4','7']);
  assert.deepEqual(visibleConversations(entries,false,'1'),entries.slice(0,5));
  assert.deepEqual(visibleConversations(entries,true,'7'),entries);
});

test('stat display keeps M-scale counts and sub-second timing',()=>{
  assert.equal(tokenCount(15_936_069),'15.9M');
  assert.equal(durationReading(1100),'1.1s');
  assert.equal(durationReading(180),'0.2s');
  assert.equal(durationReading(753000),'12m 33s');
});
