import assert from 'node:assert/strict';
import {test} from 'node:test';
import {mkdtemp,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {spawnSync} from 'node:child_process';

test('ACP live startup uses the explicit model and provider credential without contacting a provider',async()=>{
  const directory=await mkdtemp(join(tmpdir(),'rsi-acp-live-'));
  try {
    const credential=join(directory,'synthetic.env'),preload=join(directory,'child.mjs');
    await writeFile(credential,'DEEPSEEK_API_KEY="synthetic-live-key"\n');
    await writeFile(preload,`import cp from 'node:child_process';
      import {syncBuiltinESMExports} from 'node:module';
      import {readFileSync,rmSync} from 'node:fs';
      import {join,dirname} from 'node:path';
      import assert from 'node:assert/strict';
      cp.spawn=(binary,args,{env})=>{
        try {
          const config=join(env.XDG_CONFIG_HOME,'rsi');
          const route=JSON.parse(readFileSync(join(config,'settings.json'),'utf8'))['rsi.agent'].default_model;
          assert.deepEqual(route,{deployment:'live',model:'fixture.v1'});
          assert.equal(env.DEEPSEEK_API_KEY,'synthetic-live-key');
          for(const key of ['RSI_OPENAI_COMPATIBLE_API_KEY','OPENAI_API_KEY','RSI_DEEPSEEK_API_KEY','RSI_OPENAI_API_KEY'])assert.equal(env[key],undefined);
          const profile=readFileSync(join(config,'host-profiles/fixture/host.profile.toml'),'utf8');
          assert(profile.includes('[steps.config.language_models."fixture.v1"]'));
          assert(profile.includes('credential={owner="rsi.ai.provider.deepseek",slot="default"}'));
          assert(profile.includes('context_window_tokens=1048576'));
          assert(profile.includes('max_output_reserve_tokens=393216'));
          console.log('live configuration reached isolated launch boundary');
        } finally {rmSync(dirname(env.HOME),{recursive:true,force:true});}
        process.exit(0);
      };syncBuiltinESMExports();`);
    const env=Object.fromEntries(Object.entries(process.env).filter(([key])=>
      !key.startsWith('RSI_')&&!key.startsWith('DEEPSEEK_')&&!key.endsWith('_API_KEY')&&key!=='NODE_OPTIONS'));
    Object.assign(env,{RSI_LIVE_MODEL:'fixture.v1',DEEPSEEK_CONTEXT_WINDOW_TOKENS:'1048576',
      DEEPSEEK_DEFAULT_OUTPUT_RESERVE_TOKENS:'4096',DEEPSEEK_MAX_OUTPUT_RESERVE_TOKENS:'393216',
      OPENAI_API_KEY:'must-be-removed',RSI_OPENAI_COMPATIBLE_API_KEY:'must-be-replaced'});
    const result=spawnSync(process.execPath,['--import',pathToFileURL(preload).href,
      fileURLToPath(new URL('./verify.mjs',import.meta.url)),'--live-env',credential,'--output',join(directory,'report')],
      {env,encoding:'utf8',timeout:15_000});
    assert.equal(result.status,0,result.error?.message??result.stderr);
    assert.match(result.stdout,/live configuration reached isolated launch boundary/);
  } finally {await rm(directory,{recursive:true,force:true});}
});
