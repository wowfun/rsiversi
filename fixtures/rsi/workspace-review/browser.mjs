import {addWorkspace} from '../web-product/controls.mjs';
import {paired} from '../web-product/paired-env.mjs';
import {liveLimits, readDeepseekKey, deepseekProfile} from '../web-product/live-limits.mjs';
import {resources,details} from '../web-product/controls.mjs';
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startService,boundedRun,waitUntil} from '../web-product/service.mjs';
const require=createRequire(new URL('../web-product/package.json',import.meta.url));const {chromium,firefox}=require('playwright');
const liveFile=process.env.RSI_LIVE_ENV_FILE,model=process.env.RSI_LIVE_MODEL;assert(Boolean(liveFile)===Boolean(model),'live requires both explicit inputs');let key;
const profile=liveFile?deepseekProfile(model,liveLimits()):undefined;
if(liveFile)key=await readDeepseekKey(liveFile);
const binary=paired.binary,assets=paired.assets;assert(assets);const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});const results=[];
for(const [name,engine]of(liveFile?[['chromium',chromium]]:[['chromium',chromium],['firefox',firefox]])){
 const report=join(output,name);await mkdir(report,{recursive:true});const service=await startService({binary,assets,report,deepseekKey:key,configure:async({config,workspace})=>{
  if(liveFile){
   await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'}}));
   await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),profile);
  }

  const git=(...args)=>boundedRun('/usr/bin/git',args,{cwd:workspace});git('init','--quiet');await writeFile(join(workspace,'card.txt'),'committed\n');git('add','.');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture');await writeFile(join(workspace,'card.txt'),'before\n');
 }});const browser=await engine.launch({headless:true});let page;const errors=[];
 try{
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});page=await context.newPage();page.setDefaultTimeout(30_000);page.on('pageerror',e=>errors.push(e.message));
  await page.goto(service.origin);await page.locator('#receipt').fill(JSON.stringify(service.register(`${name} workspace review`)));await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await addWorkspace(page,service.workspace);await page.getByRole('button',{name:'New conversation in workspace',exact:true}).click();
  const pane=page.getByRole('region',{name:'Main conversation',exact:true});await pane.getByRole('textbox',{name:'Main message',exact:true}).fill(liveFile?'This is a workspace review integration test. Use apply_patch exactly once to replace the only line before in card.txt with after · 界 followed by a newline. Do not use any other tool and do not create other files. Then reply REVIEW_LIVE_DONE.':'Please record an inline patch');await pane.getByTestId('composer-send').click();
  await waitUntil(async()=>{const review=page.locator('.pending button').filter({hasText:'Review:'});if(await review.count()){await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();}return (await pane.locator('.pane-status').innerText())==='Completed';},'native patch settlement',liveFile?180_000:30_000);
  assert.equal(await readFile(join(service.workspace,'card.txt'),'utf8'),'after · 界\n');
  await resources(page);await page.getByRole('button',{name:'Workspace changes',exact:true}).click();const detail=details(page);await detail.getByRole('button',{name:'Review workspace changes',exact:true}).click();
  await waitUntil(async()=>{if((await detail.innerText()).includes('Complete · 1 files'))return true;await detail.getByRole('button',{name:'Review workspace changes',exact:true}).click();return false;},'interval end capture');
  await page.screenshot({path:join(report,'summary.png')});await detail.getByRole('button',{name:'Open interval files',exact:true}).click();await detail.getByText('card.txt:',{exact:true}).waitFor();await detail.getByRole('button',{name:'Open file diff',exact:true}).click();const diff=await detail.locator('pre').innerText();assert.match(diff,/-before\n/);assert.match(diff,/\+after · 界/);assert.doesNotMatch(diff,/committed/);
  await page.screenshot({path:join(report,'diff.png')});await page.setViewportSize({width:430,height:900});
  // Wait for React's narrow layout before opening its independently closed dock.
  await page.locator('#workbench.navigation-hidden').waitFor({state:'visible'});
  await resources(page);await detail.locator('pre').waitFor();await page.screenshot({path:join(report,'diff-narrow.png')});assert(await detail.evaluate(e=>e.scrollWidth<=e.clientWidth+1));assert.equal(service.provider.requests.length,liveFile?0:2);assert.deepEqual(errors,[]);results.push({browser:name,version:browser.version(),status:'passed',model:model??null,mock_provider_requests:service.provider.requests.length,dirty_baseline:true,diff});
 }catch(error){if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});throw error;}finally{await browser.close();await service.close();}
}
await writeFile(join(output,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
