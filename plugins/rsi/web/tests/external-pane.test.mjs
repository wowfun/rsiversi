import assert from 'node:assert/strict';
import {test} from 'node:test';
import {externalPaneClass} from '../external-pane.js';
class Element {
  children=[]; dataset={}; value=''; scrollHeight=0; scrollTop=0; clientHeight=0;
  setAttribute() {} addEventListener() {} append(...nodes) {this.children.push(...nodes);}
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
