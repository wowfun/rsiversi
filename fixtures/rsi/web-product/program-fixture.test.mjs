import assert from 'node:assert/strict';
import {test} from 'node:test';
import {mkdtemp, mkdir, writeFile, readFile, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {assertNoPendingWorkflowCleanup,configureProgram} from './program-fixture.mjs';

test('Program setup requires explicit Node and emits one valid Workflow Host patch', async () => {
  const config = await mkdtemp(join(tmpdir(),'rsi-program-config-'));
  try {
    const profile = join(config,'host-profiles/fixture/host.profile.toml');
    await mkdir(join(config,'host-profiles/fixture'),{recursive:true});
    await writeFile(join(config,'settings.json'),JSON.stringify({'fixture':true}));
    for (const source of ['steps = []\n', 'steps = [] \n', '[[steps]]\nkind="include"\npath="base.profile.toml"\n']) {
      await writeFile(profile,source);
      await assert.rejects(configureProgram({config,node:'relative-node'}),/explicit absolute RSI_TEST_NODE/);
      assert.equal(await readFile(profile,'utf8'),source,'refusal leaves configuration untouched');
      const node = '/fixture/node"quoted';
      await configureProgram({config,node});
      const parsed = spawnSync('python3',['-c','import tomllib,json,sys; print(json.dumps(tomllib.load(open(sys.argv[1],"rb"))))',profile],{encoding:'utf8'});
      assert.equal(parsed.status,0,parsed.stderr);
      const steps = JSON.parse(parsed.stdout).steps;
      const patches = steps.filter(step=>step.kind==='patch');
      assert.deepEqual(patches,[{kind:'patch',target:'program-runtime',config_json:JSON.stringify({node})},{kind:'patch',target:'program-runtime',enabled:true}]);
      const settings=JSON.parse(await readFile(join(config,'settings.json'),'utf8'));
      assert.equal(settings.fixture,true);
      assert.deepEqual(settings['rsi.agent-presets'],{default:'workflow'});
    }
  } finally { await rm(config,{recursive:true,force:true}); }
});

test('terminal cleanup check rejects the real pending feedback as a positive control', () => {
  assert.throws(() => assertNoPendingWorkflowCleanup('Cancelling · cleanup pending'), /terminal readback/);
  assertNoPendingWorkflowCleanup('Cancelled');
});
