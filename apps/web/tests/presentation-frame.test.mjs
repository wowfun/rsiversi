import assert from 'node:assert/strict';
import test from 'node:test';
import {mergePresentationFrame} from '../src/runtime/presentation-frame.js';
test('native patches preserve opaque tool results and do not mutate the prior frame',()=>{
  const opaque={future:{payload:[1,2]}};
  const view={surfaces:{main:{kind:'native',transcript:{blocks:[{key:'a',text:'old'}]}}}};
  const change={surface:'main',fields:{},transcript:{fields:{status:'Ready'},remove:['a'],upsert:[{key:'b',text:'new',tool:{result:opaque}}],order:['b']}};
  const patch={kind:'patch',frame_id:'2',base_frame_id:'1',sections:{},surfaces:[change]};
  const next=mergePresentationFrame(view,'1',patch);
  assert.equal(next.surfaces.main.transcript.blocks[0].tool.result,opaque);
  assert.equal(view.surfaces.main.transcript.blocks[0].text,'old');
  assert.throws(()=>mergePresentationFrame(view,'1',{...patch,surfaces:[{...change,fields:{kind:'external'}}]}),/kind changed/);
});
test('an external surface cannot accept a native transcript patch',()=>{
  assert.throws(()=>mergePresentationFrame({surfaces:{main:{kind:'external'}}},'1',{kind:'patch',frame_id:'2',base_frame_id:'1',sections:{},surfaces:[{surface:'main',fields:{},transcript:{}}]}),/no native transcript/);
});
