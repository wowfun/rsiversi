import test from 'node:test';
import assert from 'node:assert/strict';
import {modelDefinitions,newRow,providerModelConfig,replaceProvider,tokenLimit} from '../src/settings/provider-draft.ts';
test('model limits accept compact units and reject fractional, overflowing or inconsistent capacity',()=>{
  assert.equal(tokenLimit('1M'),1000000);assert.equal(tokenLimit('2.5k'),2500);
  for(const value of ['','1.1','0','-1','Infinity','4294967296'])assert.throws(()=>tokenLimit(value));
  const row=newRow('model');row.limits.context_window_tokens='1M';assert.equal(modelDefinitions([row]).model.context_window_tokens,1000000);
  row.limits.max_output_reserve_tokens='1M';assert.throws(()=>modelDefinitions([row]));
  assert.throws(()=>modelDefinitions([newRow('same'),newRow('same')]));
});
test('DeepSeek request caps fit model capacity and deleted model metadata cannot block saving',()=>{
  const models=modelDefinitions([newRow('deepseek-flash'),newRow('new-model')]);
  const fresh=providerModelConfig('deepseek',models);
  assert.deepEqual(fresh.default_output_tokens,{'deepseek-flash':4096,'new-model':4096});
  const existing={default_output_tokens:{'deepseek-flash':256000,deleted:256000},reasoning_efforts:{'deepseek-flash':{supported:['off'],default:'off'},deleted:{supported:['off'],default:'off'}},http_timeouts:{stream_idle_timeout_ms:300000}};
  const edited=providerModelConfig('deepseek',models,existing);
  assert.deepEqual(edited.default_output_tokens,{'deepseek-flash':16384,'new-model':4096});
  assert.deepEqual(Object.keys(edited.reasoning_efforts),['deepseek-flash']);
  assert.equal(edited.http_timeouts,existing.http_timeouts);
  existing.default_output_tokens['deepseek-flash']=8192;
  assert.equal(providerModelConfig('deepseek',models,existing).default_output_tokens['deepseek-flash'],8192);
  assert.equal(providerModelConfig('openai',models).default_output_tokens,undefined);
});
test('replacement preserves unrelated provider entries and rejects ID collisions',()=>{
  const a={provider:'deepseek',config:{deployment:'a',other:'retained'}},b={provider:'openai',config:{deployment:'b'}};
  const next={...a,config:{...a.config,endpoint:'https://example.invalid'}};
  assert.deepEqual(replaceProvider([a,b],next,'a'),[next,b]);assert.equal(replaceProvider([a,b],next,'a')[1],b);
  assert.throws(()=>replaceProvider([a,b],{...next,config:{deployment:'b'}},'a'));
  assert.throws(()=>replaceProvider([b],next,'a'));
});
