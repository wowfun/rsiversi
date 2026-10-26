import assert from 'node:assert/strict';
import {test} from 'node:test';
import {liveLimits,readDeepseekKey,deepseekProfile} from './live-limits.mjs';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';

test('live capacities are explicit, exact and bounded without model inference',()=>{
  const environment={DEEPSEEK_CONTEXT_WINDOW_TOKENS:'1048576',DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS:'4096',DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS:'393216'};
  assert.deepEqual(liveLimits(environment),{context_window_tokens:1048576,default_output_reserve_tokens:4096,max_output_reserve_tokens:393216});
  for(const value of [undefined,'','0','1.5','1e6','4294967296','Infinity',' 4096']) {
    assert.throws(()=>liveLimits({...environment,DEEPSEEK_CONTEXT_WINDOW_TOKENS:value}),/DEEPSEEK_CONTEXT_WINDOW_TOKENS/);
  }
  assert.throws(()=>liveLimits({...environment,DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS:'4095'}),/maximum output reserve/);
  const profile=deepseekProfile('model.v1',liveLimits(environment));
  assert.match(profile,/language_models\."model.v1"/);
  assert.match(profile,/max_output_reserve_tokens=393216/);
  assert.match(profile,/default_output_tokens=\{"model.v1"=4096\}/);
  for(const model of [undefined,null,42,'','bad"model']) {
    assert.throws(()=>deepseekProfile(model,liveLimits(environment)),/model identifier/);
  }
});

test('live credential parsing reads only the explicit bounded file',async t=>{
  const directory=await mkdtemp(join(tmpdir(),'rsi-live-key-'));
  t.after(()=>rm(directory,{recursive:true,force:true}));
  const file=join(directory,'fixture.env');
  for(const line of ['DEEPSEEK_API_KEY=synthetic-key',' export DEEPSEEK_API_KEY="synthetic-key"',"DEEPSEEK_API_KEY='synthetic-key'"]) {
    await writeFile(file,line+'\r\n');assert.equal(await readDeepseekKey(file),'synthetic-key');
  }
  for(const text of ['OTHER_KEY=synthetic-key','DEEPSEEK_API_KEY=""','DEEPSEEK_API_KEY="   "','DEEPSEEK_API_KEY=\nOTHER_KEY=synthetic-key','x'.repeat(65537)]) {
    await writeFile(file,text);await assert.rejects(readDeepseekKey(file),/absent|empty|bound/);
  }
});
