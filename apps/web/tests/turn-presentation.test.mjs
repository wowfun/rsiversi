import {test} from 'node:test';
import assert from 'node:assert/strict';
import {turnVisibility} from '../turn-presentation.js';
import * as presentation from '../turn-presentation.js';
const turn={status:'Completed',partial:false,foldable:true,running:false,process:['thinking','earlier'],candidate:[],answer:['answer']};
test('detail modes preserve partial and unsuccessful content and visible streaming candidates',()=>{
  assert.deepEqual([...turnVisibility(turn,'standard',false).hidden],turn.process);
  assert.equal(turnVisibility(turn,'verbose',false).hidden.size,0);
  assert.equal(turnVisibility(turn,'compact',true).hidden.size,0);
  for(const status of ['Failed','Cancelled','Partially failed','Budget exceeded','Interrupted'])assert.equal(turnVisibility({...turn,status,foldable:false},'compact',false).hidden.size,0);
  assert.equal(turnVisibility({...turn,partial:true,foldable:false},'compact',false).hidden.size,0);
  assert.deepEqual([...turnVisibility({...turn,status:'Running',running:true,candidate:['earlier']},'compact',false).hidden],['thinking']);
  assert.equal(turnVisibility({...turn,status:'Running',running:true},'detailed',false).expanded,true);
});

test('unchanged Turn patches do not mutate block visibility or classes', async () => {
  const {TurnPresentation} = await import('../turn-presentation.js');
  let writes = 0;
  const classes = new Set();
  let hidden = false;
  const node = {
    get hidden() { return hidden; },
    set hidden(value) { writes++; hidden = value; },
    classList: {
      contains: name => classes.has(name),
      toggle(name, value) {
        writes++;
        if (value) classes.add(name); else classes.delete(name);
      },
    },
  };
  const view = {entries: {one: {
    id: 'one', status: 'Completed', partial: false, foldable:true, running:false,
    blocks: ['answer'], process: [], candidate: [], answer: ['answer'],
  }}};
  const blocks = new Map([['answer', {node}]]);
  const adapter = new TurnPresentation({}, () => {});
  adapter.apply(view, blocks, 'detailed');
  assert(classes.has('turn-expanded'));
  writes = 0;
  adapter.apply(view, blocks, 'detailed');
  assert.equal(writes, 0);
  adapter.apply({entries: {}}, blocks, 'standard');
  assert(!classes.has('turn-expanded'));
});

test('summary disclosure follows actual process visibility in running and failed turns', async () => {
  const {TurnPresentation} = await import('../turn-presentation.js');
  const classes = new Set();
  const node = {hidden: false, classList: {
    contains: name => classes.has(name),
    toggle(name, value) { if (value) classes.add(name); else classes.delete(name); },
  }};
  let expanded;
  const row = {
    nextSibling: node,
    getAttribute() { return expanded; },
    setAttribute(_name, value) { expanded = value; },
  };
  const adapter = new TurnPresentation({}, () => {});
  adapter.rows.set('one', row);
  for (const [status, visible] of [['Running',true], ['Completed',false], ['Cancelled',true]]) {
    adapter.apply({entries: {one: {
      id:'one', status, partial:false, running:status==='Running',foldable:status!=='Cancelled', blocks:['step'], process:['step'], answer:[], candidate:[],
    }}}, new Map([['step',{node}]]), 'standard');
    assert.equal(node.hidden, !visible);
    assert.equal(expanded, String(visible));
    assert(row.textContent.startsWith(visible ? '▾' : '▸'));
  }
});

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
