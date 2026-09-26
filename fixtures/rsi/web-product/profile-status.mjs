import {openBrowserPage, connectWorkbench} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Real watcher failure and recovery through redacted configuration status.
import assert from 'node:assert/strict';
import {readFile, writeFile, mkdir, copyFile, chmod} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium, firefox} from 'playwright';
import {startService} from './service.mjs';
const report=process.env.RSI_WEB_REPORT, assets=process.env.RSI_WEB_ASSETS;
assert(report && assets, 'explicit report and built assets required');
await mkdir(report,{recursive:false});
const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY??resolve('target/debug/rsi'),binary);await chmod(binary,0o700);
const results=[];
for(const [name,engine] of [['chromium',chromium],['firefox',firefox]]) {
  const directory=join(report,name);await mkdir(directory);
  let profile,original,service,browser,page;const errors=[];
  try {
  service=await startService({binary,assets,report:directory,configure:async({config})=>{
    profile=join(config,'host-profiles/fixture/host.profile.toml');original=await readFile(profile,'utf8');
  }});
  ({browser, page} = await openBrowserPage(engine, errors));
  await connectWorkbench(page, service, 'profile status observation');
    await page.getByRole('button',{name:'Settings',exact:true}).click();await page.getByRole('button',{name:'Plugins',exact:true}).click();
    const panel=page.getByRole('region',{name:'Plugins',exact:true});await panel.locator('.plugin-row').first().waitFor();
    const before=await panel.locator('.plugin-revisions').innerText();
    const refresh=async pattern=>{
      const deadline=Date.now()+30000;
      while(Date.now()<deadline) {
        await panel.getByRole('button',{name:'Refresh plugin status',exact:true}).click();
        if(pattern.test(await panel.innerText()))return;
        await page.waitForTimeout(150);
      }
      assert.match(await panel.innerText(),pattern);
    };
    await writeFile(profile,original+'\nPRIVATE_FAILURE_MARKER = [\n');
    await refresh(/Last update #\d+: failed before changing the running configuration\./);
    const failed=await panel.locator('.plugin-revisions').innerText();
    const revisions=text=>[...text.matchAll(/(?:Desired|Observed) (\d+)/g)].map(match=>BigInt(match[1]));
    assert.equal(revisions(before)[0],revisions(failed)[0]);assert(revisions(failed)[1]>revisions(before)[1]);
    assert(!(await panel.innerText()).includes('PRIVATE_FAILURE_MARKER'));assert(!(await panel.innerText()).includes(profile));
    await page.screenshot({path:join(directory,'failed-wide.png')});await page.setViewportSize({width:420,height:900});
    assert(await panel.evaluate(element=>element.scrollWidth<=element.clientWidth+1));await page.screenshot({path:join(directory,'failed-narrow.png')});
    await writeFile(profile,original);await refresh(/Last update #\d+: (?:unchanged|applied)\./);
    const recovered=await panel.locator('.plugin-revisions').innerText();assert(revisions(recovered)[1]>revisions(failed)[1]);
    await page.screenshot({path:join(directory,'recovered.png')});assert.equal(service.provider.requests.length,0);assert.deepEqual(errors,[]);
    results.push({browser:name,version:browser.version(),before,failed,recovered,model_requests:0});
  } catch(error) {if(page)await page.screenshot({path:join(directory,'failure.png')}).catch(()=>{});throw error;}
  finally {await cleanupAll(() => browser?.close(), () => service?.close());}
}
await writeFile(join(report,'result.json'),JSON.stringify({ok:true,results},null,2));
