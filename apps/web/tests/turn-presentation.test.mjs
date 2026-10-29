import {test} from 'node:test';
import assert from 'node:assert/strict';
import {turnVisibility} from '../turn-presentation.js';
import * as presentation from '../turn-presentation.js';
const turn={status:'Completed',partial:false,foldable:true,running:false,process:['thinking','earlier'],candidate:[],answer:['answer']};
test('back-to-bottom requires overflow and distance beyond the tail threshold',()=>{
  for(const [scrollHeight,clientHeight,scrollTop,expected] of [[500,500,0,false],[1000,500,500,false],[1000,500,430,false],[1000,500,429,true],[500,500,-100,false]])
    assert.equal(presentation.awayFromBottom({scrollHeight,clientHeight,scrollTop}),expected);
});
test('detail modes preserve partial and unsuccessful content and visible streaming candidates',()=>{
  assert.deepEqual([...turnVisibility(turn,'standard',false).hidden],turn.process);
  assert.equal(turnVisibility(turn,'verbose',false).hidden.size,0);
  assert.equal(turnVisibility(turn,'compact',true).hidden.size,0);
  for(const status of ['Failed','Cancelled','Partially failed','Budget exceeded','Interrupted'])assert.equal(turnVisibility({...turn,status,foldable:false},'compact',false).hidden.size,0);
  assert.equal(turnVisibility({...turn,partial:true,foldable:false},'compact',false).hidden.size,0);
  assert.deepEqual([...turnVisibility({...turn,status:'Running',running:true,candidate:['earlier']},'compact',false).hidden],['thinking']);
  assert.equal(turnVisibility({...turn,status:'Running',running:true},'detailed',false).expanded,true);
});

// DOM mutation, summary disclosure and identity invariants run against the real
// React transcript in fixtures/rsi/web-product/frame-commit.mjs.

test('display labels do not control folding',()=>{
  assert.deepEqual(turnVisibility({...turn,status:'All done'},'standard',false),turnVisibility(turn,'standard',false));
  assert.equal(turnVisibility({...turn,status:'Completed',foldable:false},'compact',false).hidden.size,0);
});

test('tail following skips per-message geometry while reading preserves a visible anchor',()=>{
  let reads=0;
  const node={hidden:false,isConnected:true,getBoundingClientRect:()=>{reads++;return {top:40,bottom:80}},getClientRects:()=>[{}]};
  const container={scrollHeight:1000,clientHeight:500,scrollTop:500,children:[node],getBoundingClientRect:()=>{reads++;return {top:20}}};
  assert.deepEqual(presentation.readingPosition(container,false),{previousHeight:1000,atEnd:true,anchors:[]});
  assert.equal(reads,0);
  container.scrollTop=100;
  const position=presentation.readingPosition(container,false);
  assert.equal(position.atEnd,false);assert.equal(reads,2);
  assert.equal(position.anchors[0].node,node);assert.equal(position.anchors[0].offset,20);
  node.getBoundingClientRect=()=>({top:70,bottom:110});
  presentation.restoreAnchor(container,position.anchors);
  assert.equal(container.scrollTop,130);
  container.scrollTop=500;
  assert.equal(presentation.readingPosition(container,true).atEnd,false);
});

test('only a complete successful answer replaces its typed terminal with the turn heading',()=>{
  const completed={...turn,terminal:'terminal'};
  assert(turnVisibility(completed,'standard',false).hidden.has('terminal'));
  for(const changes of [{foldable:false},{running:true},{answer:[]}])assert(!turnVisibility({...completed,...changes},'standard',false).hidden.has('terminal'));
  assert(!turnVisibility(completed,'verbose',false).hidden.has('terminal'));
});
