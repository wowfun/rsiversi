import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {appendFile, mkdir, readFile, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {startService, waitUntil} from '../web-product/service.mjs';

const require = createRequire(new URL('../web-product/package.json', import.meta.url));
const {chromium} = require('playwright');
const output = resolve(process.argv[2]);
const configuration = JSON.parse(await readFile(process.env.RSI_OPENCODE_FIXTURE, 'utf8'));
await mkdir(output, {recursive: true});
const marker = join(output, 'peer-pids');
let workspacePath;
const service = await startService({
  binary: resolve(process.env.RSI_BINARY ?? 'target/debug/rsi'),
  assets: process.env.RSI_WEB_ASSETS,
  report: output,
  async configure({config, workspace}) {
    workspacePath = workspace;
    const endpoint = {
      id: 'opencode', enabled: true, cwd: workspace, sandbox: 'danger-full-access',
      session_options: [{id:'model', value:configuration.model}, {id:'effort', value:configuration.effort}],
      launch: {
        program: '/usr/bin/python3',
        arguments: [resolve('fixtures/rsi/acp/opencode-launch.py'), marker, configuration.binary, workspace],
        environment: Object.fromEntries(Object.entries(configuration.environment).map(([key,value]) => [key,{kind:'literal',value}])),
      },
    };
    const patch = JSON.stringify({directory:join(config,'../../state/rsi/acp'), endpoints:[endpoint]});
    await appendFile(join(config,'host-profiles/fixture/host.profile.toml'),
      `\n[[steps]]\nkind="patch"\ntarget="rsi-acp"\nconfig_json=${JSON.stringify(patch)}\n`);
  },
});
const browser = await chromium.launch({headless:true});
const context = await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
const page = await context.newPage();
page.setDefaultTimeout(60_000);
const errors = [];
page.on('pageerror', error => errors.push(error.message));
const result = {model:configuration.model, effort:configuration.effort, browser:browser.version(), ok:false, phase:'setup'};
try {
  await page.goto(service.origin);
  await page.locator('#receipt').fill(JSON.stringify(service.register('OpenCode live ACP')));
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await page.locator('#workbench').waitFor({state:'visible'});
  await page.getByRole('button',{name:'Start opencode',exact:true}).click();
  const pane = page.getByRole('region',{name:'External conversation',exact:true});
  await pane.waitFor({state:'visible'});
  await pane.locator('.external-status').filter({hasText:'ready'}).waitFor();
  result.phase = 'availability';
  const answered = new Set();
  async function assistantText() {
    return pane.locator('.external-record').evaluateAll(rows => rows
      .filter(row=>row.querySelector('.external-record-label')?.textContent==='Assistant')
      .map(row=>row.querySelector('pre')?.textContent??'').join(''));
  }
  async function prompt(text, markerText, allowEdit) {
    await pane.getByRole('textbox',{name:'External message',exact:true}).fill(text);
    await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
    await waitUntil(async () => {
      const permissions = pane.locator('.external-permission');
      if (await permissions.count()) {
        assert(allowEdit, 'availability probe must not invoke tools');
        const identity = await permissions.first().getAttribute('data-request');
        if (!answered.has(identity)) {
          await page.screenshot({path:join(output,'permission.png')});
          const allow = permissions.first().getByRole('button').filter({hasText:/allow once$/i});
          assert.equal(await allow.count(), 1);
          answered.add(identity);
          await allow.click();
        }
      }
      const status = await pane.locator('.external-status').innerText();
      assert(!/failed|unknown|refusal|max_tokens/i.test(status), `ACP outcome ${status}`);
      return /completed/i.test(status) && (await assistantText()).includes(markerText);
    }, 'OpenCode live response', 180_000);
  }
  await prompt('Do not call any tools. Reply only with OPENCODE_ACP_READY.', 'OPENCODE_ACP_READY', false);
  result.phase = 'tool';
  await prompt('In this isolated test workspace, create only opencode-live.txt with exactly the UTF-8 line opencode-live-ok followed by a newline. Use a file editing tool, not a shell command. Then reply OPENCODE_TOOL_VERIFIED. Do not modify other files.', 'OPENCODE_TOOL_VERIFIED', true);
  assert.equal(await readFile(join(workspacePath,'opencode-live.txt'),'utf8'), 'opencode-live-ok\n');
  result.file_bytes = 17;
  await pane.locator('.external-transcript').evaluate(element=>{element.scrollTop=element.scrollHeight;});
  await page.screenshot({path:join(output,'conversation.png')});
  await page.setViewportSize({width:430,height:900});
  await page.screenshot({path:join(output,'narrow.png')});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1), false);
  await page.setViewportSize({width:1440,height:980});
  result.phase = 'resume';
  await pane.getByRole('button',{name:'Close peer',exact:true}).click();
  await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();
  await pane.getByRole('button',{name:'Resume connection',exact:true}).click();
  await pane.locator('.external-status').filter({hasText:'ready'}).waitFor();
  await prompt('Without tools, reply OPENCODE_RESUMED and the exact file line you previously wrote.', 'OPENCODE_RESUMED', false);
  assert((await assistantText()).includes('opencode-live-ok'));
  await pane.getByRole('button',{name:'Close peer',exact:true}).click();
  await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();
  result.phase = 'load';
  await pane.getByRole('button',{name:'Reload remote history',exact:true}).click();
  await pane.locator('.external-status').filter({hasText:'ready'}).waitFor();
  await waitUntil(async()=>(await assistantText()).includes('OPENCODE_TOOL_VERIFIED'), 'actual loaded assistant history');
  await pane.locator('.external-transcript').evaluate(element=>{element.scrollTop=element.scrollHeight;});
  await page.screenshot({path:join(output,'replay.png')});
  await pane.getByRole('button',{name:'Close peer',exact:true}).click();
  await pane.locator('.external-status').filter({hasText:'Closed'}).waitFor();
  const pids = (await readFile(marker,'utf8')).trim().split('\n').map(Number);
  for (const pid of pids) assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});
  assert.deepEqual(errors, []);
  assert.equal(service.provider.requests.length, 0);
  Object.assign(result,{ok:true,phase:'complete',peers_reaped:pids.length,mock_provider_requests:0,resume:true,load:true});
} catch (error) {
  result.error = String(error);
  await page.screenshot({path:join(output,'failure.png')}).catch(()=>{});
  process.exitCode = 1;
} finally {
  await browser.close();
  await service.close();
  await writeFile(join(output,'result.json'), JSON.stringify(result,null,2)+'\n');
}
console.log(JSON.stringify(result));
