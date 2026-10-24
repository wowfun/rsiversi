import test from 'node:test';
import assert from 'node:assert/strict';
import {openHistoryPicker} from '../history-picker.js';
class Node {
  constructor(tag){this.tag=tag;this.children=[];this.events={};this.dataset={};this.value='';}
  append(...nodes){this.children.push(...nodes);}
  replaceChildren(...nodes){this.children=nodes;}
  setAttribute(name,value){this[name]=value;}
  addEventListener(name,run){this.events[name]=run;}
  querySelectorAll(tag){return this.children.flatMap(child=>[...(child.tag===tag?[child]:[]),...child.querySelectorAll(tag)]);}
  focus(){} remove(){} showModal(){this.open=true;} close(){this.open=false;this.events.close?.();}
}
const flush=()=>new Promise(resolve=>setImmediate(resolve));
test('replacing a frozen history preview releases it without waiting for dialog close',async()=>{
  const previous=globalThis.document;
  globalThis.document={createElement:tag=>new Node(tag),body:new Node('body')};
  try {
    const released=[], reference={snapshot:{capture_id:'pending'},preview:'text'};
    const pane={index:'main',generation:'1',editor:{references:[]},historyContext:{workspace:'workspace',session:'target'},
      input:{focus(){}},async releaseReference(value){released.push(value);}};
    const progress={visible_sources:1,pending_sources:0,discovery_complete:true};
    const hit={preview:'text',original:{record:{kind:'assistant',sequence:1},end:4}};
    const scope={workspace:'workspace',conversation:{id:'source',kind:'native'}};
    const call=async(_method,payload)=>{
      const op=JSON.parse(payload).operation.request.operation;
      return JSON.stringify(op==='discover'?{progress}:op==='query'?{progress,matches:[{scope,hit,label:'Source',reference_allowed:true}]}:
        op==='read'?{offset:0,next_offset:4,text:'text',has_more:false}:{reference});
    };
    const button=(text,run)=>Object.assign(new Node('button'),{textContent:text,run});
    openHistoryPicker(pane,{},call,button); await flush();
    const click=async text=>{const node=pane.referenceDialog.querySelectorAll('button').find(node=>node.textContent===text);assert(node,text);node.run();await flush();};
    await click('Search text'); await click('Open original');
    const original=pane.referenceDialog.querySelectorAll('textarea')[0];original.selectionStart=0;original.selectionEnd=4;
    await click('Freeze selected fragment'); assert.equal(released.length,0);
    await click('Search text'); assert.deepEqual(released,[reference]);
    pane.referenceDialog.close(); assert.equal(released.length,1);
  }finally{globalThis.document=previous;}
});
