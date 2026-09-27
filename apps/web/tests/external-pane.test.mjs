import assert from 'node:assert/strict';
import {test} from 'node:test';
import {externalPaneClass} from '../external-pane.js';
class Element {
  children=[]; dataset={}; value=''; scrollHeight=0; scrollTop=0; clientHeight=0;
  listeners={}; setAttribute() {} addEventListener(name,handler) {this.listeners[name]=handler;} append(...nodes) {this.children.push(...nodes);}
  replaceChildren(...nodes) {this.children=nodes;}
}
test('external switch survives old frames and prevents keyboard submission until new binding',async()=>{
  const original=globalThis.document, calls=[];
  globalThis.document={getElementById:()=>new Element()};
  try {
    const Pane=externalPaneClass({element:()=>new Element(),button:()=>new Element(),command:async value=>calls.push(value),perform:work=>work()});
    const pane=new Pane(0);
    const frame=generation=>({generation,external:{observed:{connected:true,snapshot:{id:'external',generation:'1',endpoint:'fixture',cwd:'/workspace',status:'ready'},permissions:[]},capabilities:{submit:true},blocks:[],following:true,busy:false}});
    pane.render(frame('one')); pane.input.value='one prompt'; pane.switching=true;
    pane.render(frame('one'));
    assert.equal(pane.switching,true); assert.equal(pane.input.disabled,true); assert.equal(pane.send.disabled,true);
    await pane.submit(); assert.equal(calls.length,0);
    pane.render(frame('two'));
    assert.equal(pane.switching,false); assert.equal(pane.input.disabled,false);
    await pane.submit(); assert.equal(calls.length,1); assert.equal(calls[0].generation,'two');
  }finally{globalThis.document=original;}
});

test('external composer shares Enter preference, modifier and IME handling',async()=>{
  const original=globalThis.document;
  globalThis.document={getElementById:()=>new Element()};
  try {
    const Pane=externalPaneClass({element:()=>new Element(),button:()=>new Element(),command:async()=>{},perform:work=>work()});
    const pane=new Pane(0);let sends=0,prevented=0;
    pane.submit=async()=>{sends++};
    for(const [submitKey,extra,expected] of [
      ['enter',{},true],['mod_enter',{},false],['mod_enter',{ctrlKey:true},true],
      ['enter',{metaKey:true},true],['enter',{altKey:true},false],['enter',{shiftKey:true},false],
      ['enter',{isComposing:true},false],['enter',{keyCode:229},false],
    ]){
      pane.submitKey=submitKey;const before=sends;
      pane.input.listeners.keydown({key:'Enter',preventDefault:()=>prevented++,...extra});
      assert.equal(sends-before,Number(expected));
    }
    assert.equal(prevented,sends);
  }finally{globalThis.document=original;}
});
