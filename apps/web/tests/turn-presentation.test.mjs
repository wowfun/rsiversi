import {test} from 'node:test';
import assert from 'node:assert/strict';
import {turnVisibility} from '../turn-presentation.js';
import * as presentation from '../turn-presentation.js';
const turn={status:'Completed',partial:false,foldable:true,running:false,process:['thinking','earlier'],candidate:[],answer:['answer']};
test('reading midway through a long history measures nearby fallback anchors',()=>{
  let reads=0,shift=0;
  const rows=Array.from({length:1024},(_,i)=>({isConnected:true,hidden:false,getClientRects:()=>[{}],
    getBoundingClientRect(){reads++;const top=(i-512)*40+shift;return {top,bottom:top+30};}}));
  const port={scrollHeight:40960,clientHeight:100,scrollTop:20480,children:rows,querySelectorAll:()=>rows,
    getBoundingClientRect:()=>({top:0,bottom:100})};
  const {anchors}=presentation.readingPosition(port,false);
  assert.deepEqual(anchors.map(a=>rows.indexOf(a.node)),[512,513,514,515]);
  assert(reads<=16);
  for(const row of rows.slice(512,515))row.hidden=true;
  shift=-120;
  presentation.restoreAnchor(port,anchors);
  assert.equal(port.scrollTop,20360);
});
test('toggling an individual heading anchors it without scanning siblings or writing an unchanged offset',()=>{
  let top=30,writes=0;
  const heading={hidden:false,isConnected:true,getClientRects:()=>[{}],getBoundingClientRect:()=>({top})};
  const port={getBoundingClientRect:()=>({top:5}),get scrollTop(){return 100},set scrollTop(value){writes++;assert.equal(value,110)}};
  const {atEnd,anchors}=presentation.readingPosition(port,false,heading);
  assert.equal(atEnd,false);
  presentation.restoreAnchor(port,anchors);assert.equal(writes,0);
  top+=10;presentation.restoreAnchor(port,anchors);assert.equal(writes,1);
});

test('back-to-bottom requires overflow and distance beyond the tail threshold',()=>{
  for(const [scrollHeight,clientHeight,scrollTop,away] of [[500,500,0,false],[1000,500,500,false],[1000,500,475,false],[1000,500,474.5,true],[1000,500,460,true],[500,500,-100,false]]) {
    const port={scrollHeight,clientHeight,scrollTop,querySelectorAll:()=>[],getBoundingClientRect:()=>({top:0,bottom:500})};
    assert.equal(presentation.awayFromBottom(port),away);
    assert.equal(presentation.readingPosition(port,false).atEnd,!away);
  }
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
  const container={scrollHeight:1000,clientHeight:500,scrollTop:500,querySelectorAll:()=>[node],getBoundingClientRect:()=>{reads++;return {top:20,bottom:520}}};
  assert.deepEqual(presentation.readingPosition(container,false),{atEnd:true,anchors:[]});
  assert.equal(reads,0);
  container.scrollTop=100;
  const position=presentation.readingPosition(container,false);
  assert.equal(position.atEnd,false);assert(reads<=3);
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

test('DSH detail policy distinguishes running bodies and preserves every assistant reply',()=>{
  const running={...turn,running:true};
  assert.equal(turnVisibility(running,'standard',false).expanded,false);
  assert.equal(turnVisibility(running,'detailed',false).expanded,true);
  assert.equal(turnVisibility(turn,'detailed',true).expanded,false);
  assert.equal(turnVisibility(turn,'verbose',false).expanded,true);
  assert.deepEqual([...turnVisibility(turn,'standard',false,new Set(['earlier'])).hidden],['thinking']);
  assert.equal(presentation.presentationPolicy('compact').reasoningPreview,false);
  assert.equal(presentation.presentationPolicy('standard').liveDetail,true);
  assert.equal(presentation.presentationPolicy('verbose').liveDetail,false);
});

test('a streaming Think preview advances on a completed paragraph heading',()=>{
  const preview = presentation.reasoningPreview;
  assert.equal(preview('unfinished',true),'');
  assert.equal(preview('**Read**\r\nbody\r\n \r\nNext',true),'Read');
  assert.equal(preview('**Read**\r\nbody\r\n \r\nNext\r\nextra',true),'Next');
  assert.equal(preview('**Read**\r\nbody\r\n \r\nNext',false),'Read');
});
