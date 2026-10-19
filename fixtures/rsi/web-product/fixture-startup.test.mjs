import assert from 'node:assert/strict';
import {test} from 'node:test';
import {mkdtemp, mkdir, writeFile, rm, realpath} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {tmpdir} from 'node:os';
import {join,dirname,relative} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';

test('sibling browser entrypoints pass the verified pair to Service in both selection modes', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'rsi-fixture-startup-'));
  try {
    const name = process.platform === 'win32' ? 'rsi.exe' : 'rsi';
    const binary = join(directory, name), assets = join(directory, 'assets');
    const bytes = 'isolated fixture executable', family = 'a'.repeat(64);
    await mkdir(assets);
    await writeFile(binary, bytes);
    await writeFile(join(assets, 'rsi-build.json'), JSON.stringify({family_sha256:family}));
    await writeFile(join(directory, 'receipt.json'), JSON.stringify({format:2, kind:'web', family_sha256:family,
      artifacts:{[name]:createHash('sha256').update(bytes).digest('hex')}}));
    const configuration = join(directory, 'opencode.json');
    await writeFile(configuration, '{}');
    const canonicalBinary = await realpath(binary), canonicalAssets = await realpath(assets);
    // Import each real entrypoint and real pair selector. Stop before Service or
    // browser work; all unrelated launch attempts fail instead of escaping isolation.
    const service = new URL('./service.mjs', import.meta.url).href;
    const stub = `import assert from 'node:assert/strict';
      export async function startService({binary,assets}) {
        assert.equal(binary,${JSON.stringify(canonicalBinary)});
        assert.equal(assets,${JSON.stringify(canonicalAssets)});
        if(process.env.RSI_PAIRED_BUNDLE)assert.equal(process.env.RSI_PAIRED_BUNDLE,${JSON.stringify(dirname(canonicalBinary))});
        console.log('verified pair reached Service');process.exit(0);
      }
      export function boundedRun(){throw Error('unexpected subprocess');}
      export function waitUntil(){throw Error('unexpected browser work');}`;
    const loader = `export async function load(url,context,next) {
      return url===${JSON.stringify(service)} ? {format:'module',shortCircuit:true,source:${JSON.stringify(stub)}} : next(url,context);
    }`;
    const preload = join(directory, 'preload.mjs');
    await writeFile(preload, `import cp from 'node:child_process';
      import {register,syncBuiltinESMExports} from 'node:module';
      const launch=cp.spawn;
      cp.spawn=(binary,args,options)=>{
        if(args[0]!=='fixture-lease')throw Error('unexpected subprocess');
        return launch(binary,['-e',"console.log('ready');process.stdin.resume();"],options);
      };
      syncBuiltinESMExports();
      register(${JSON.stringify('data:text/javascript,'+encodeURIComponent(loader))},import.meta.url);`);
    for (const mode of ['bundle', 'explicit-pair']) {
      for (const fixture of ['acp/browser', 'acp/opencode-browser', 'lsp/browser', 'profile-management/browser', 'history/browser', 'workspace-review/browser', 'web-product/automation-verify']) {
        await t.test(`${fixture}: ${mode}`, () => {
          // Do not inherit live credentials or fixture selectors from the host.
          const env = Object.fromEntries(Object.entries(process.env).filter(([key]) =>
            !key.startsWith('RSI_') && !key.endsWith('_API_KEY') && key !== 'NODE_OPTIONS'));
          Object.assign(env, {RSI_APP_TOOLS_BINARY:process.execPath,
            RSI_RUST_ANALYZER:'unused', RSI_OPENCODE_FIXTURE:configuration,
            RSI_AUTOMATION_WEB_REPORT:join(directory,mode,'automation-report'),RSI_AUTOMATION_WEB_SEED:directory});
          if (mode === 'bundle') env.RSI_PAIRED_BUNDLE = relative(process.cwd(),directory);
          else Object.assign(env, {RSI_WEB_BINARY:binary, RSI_WEB_ASSETS:assets});
          const result = spawnSync(process.execPath, ['--import',pathToFileURL(preload).href,
            fileURLToPath(new URL(`../${fixture}.mjs`, import.meta.url)),
            join(directory, mode, fixture)], {env, encoding:'utf8', timeout:15_000});
          assert.equal(result.status, 0, result.error?.message ?? result.stderr);
          assert.match(result.stdout, /verified pair reached Service/);
        });
      }
    }
  } finally {
    await rm(directory, {recursive:true, force:true});
  }
});
