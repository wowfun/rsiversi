import {test} from 'node:test';
import assert from 'node:assert/strict';
import {TranscriptVisibility} from '../transcript-visibility.js';

test('publishing visibility waits for every current initial observation, not a render frame', () => {
  const observers=[], published=[];
  const owner=new TranscriptVisibility({},()=>published.push(owner.last(4).map(([key])=>key)), callback=>{
    const observer={callback,observe(){},unobserve(){},disconnect(){}};
    observers.push(observer);return observer;
  });
  const rows=['a','b','c'].map(key=>[key,{node:{hidden:false}}]);
  const send=(observer,indices,height=1)=>observer.callback(indices.map(i=>({target:rows[i][1].node,intersectionRect:{height}})));
  owner.update(rows,'one');
  assert.deepEqual(published,[]);
  send(observers[0],[0]);
  owner.update(rows,'one');
  assert.deepEqual(published,[],'partial observation or repeated rendering cannot publish a selection');
  send(observers[0],[1,2],0);
  assert.deepEqual(published,[['a']]);
  owner.update([rows[0],rows[2],rows[1]],'one');
  assert.deepEqual(published.at(-1),['a'],'retained rows remain observed');
  owner.update(rows,'two');
  const before=published.length;
  send(observers[0],[0,1,2]);
  assert.equal(published.length,before,'a retired generation cannot finish current observation');
  send(observers[1],[0],0);
  owner.update([rows[0]],'two');
  assert.deepEqual(published.at(-1),[],'removing unobserved rows releases the wait');
  owner.update(rows,'three');
  owner.update([],'three');
  assert.deepEqual(published.at(-1),[],'an empty transcript clears immediately');
  owner.dispose();
});

test('current visibility follows upward jumps, transcript order, edge contact and replacement', () => {
  const observers = [];
  const owner = new TranscriptVisibility({}, () => {}, callback => {
    const observed = new Set();
    const observer = {callback, observed, observe:node=>observed.add(node),unobserve:node=>observed.delete(node),disconnect:()=>observed.clear()};
    observers.push(observer); return observer;
  });
  const rows = Array.from({length:128},(_,i)=>[String(i), {node:{hidden:false}}]);
  const send = (indices,height=1,observer=observers.at(-1)) => observer.callback(indices.map(i=>({target:rows[i][1].node,intersectionRect:{height}})));
  owner.update(rows,'one'); send([120,121,122,123,124,125]);
  assert.deepEqual(owner.last(4).map(([key])=>key), ['122','123','124','125']);
  send([120,121,122,123,124,125],0); send([1,2,3,4,5]);
  assert.deepEqual(owner.last(4).map(([key])=>key), ['2','3','4','5']);
  rows[5][1].node.hidden = true;
  assert.deepEqual(owner.last(4).map(([key])=>key), ['1','2','3','4']);
  owner.update([...rows].reverse(),'one');
  assert.deepEqual(owner.last(4).map(([key])=>key), ['4','3','2','1']);
  const old = observers[0]; owner.update(rows.slice(0,3),'two'); send([120],1,old);
  assert.deepEqual(owner.last(4), []); assert.equal(old.observed.size,0);
  send([1,2]);owner.update([rows[1]],'two');
  assert.deepEqual(owner.last(4).map(([key])=>key), ['1']);
  owner.update([], undefined);
  send([1]); assert.deepEqual(owner.last(4), []);
  assert.equal(observers.at(-1).observed.size,0);
  owner.dispose();send([1]);assert.deepEqual(owner.last(4),[]);
  assert.equal(observers.at(-1).observed.size,0);
});
