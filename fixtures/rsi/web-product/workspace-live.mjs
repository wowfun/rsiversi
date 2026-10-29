// Opt-in full Worker/HTTP/Service verification. No key is read without explicit inputs.
import assert from 'node:assert/strict';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
import {readDeepseekKey,deepseekProfile,liveLimits} from './live-limits.mjs';
import {workspaceForm,disconnect} from './controls.mjs';
const report=resolve(process.env.RSI_WEB_REPORT), binary=process.env.RSI_WEB_BINARY, assets=process.env.RSI_WEB_ASSETS;
assert(binary && assets && process.env.RSI_LIVE_ENV_FILE && process.env.RSI_LIVE_MODEL,'Explicit live configuration required');
await mkdir(report,{recursive:true});
const key=await readDeepseekKey(process.env.RSI_LIVE_ENV_FILE),model=process.env.RSI_LIVE_MODEL;
let service,browser;const errors=[];
try {
 service=await startService({binary,assets,report,deepseekKey:key,developmentHttp:true,configure:async({config})=>{
  await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model},default_reasoning_effort:'off'}}));
  await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),deepseekProfile(model,liveLimits()));
 }});
 browser=await chromium.launch();const page=await browser.newPage({viewport:{width:1710,height:940},colorScheme:'dark'});
 page.on('pageerror',error=>errors.push(error.message));
 await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register('workspace UI verification')));
 await page.locator('#dev-http').check();await page.getByRole('button',{name:'Connect',exact:true}).click();
 await page.locator('#workbench').waitFor({state:'visible',timeout:60000});
 const input=page.getByRole('textbox',{name:'Main message',exact:true});
 await page.getByTestId('new-conversation').click();
 await input.fill('Reply exactly RSI_WORKSPACE_UI_OK. Do not call any tools.');
 await workspaceForm(page);await page.getByLabel('Directory path').fill(service.workspace);
 await page.locator('.directory-picker header').getByRole('button',{name:'Go',exact:true}).click();
 await page.getByRole('navigation',{name:'Directory breadcrumbs'}).waitFor();
 await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();
 await page.locator('.workspace-choice > summary').click();
 await page.getByRole('menuitemradio',{name:'workspace',exact:true}).click();
 assert.match(await input.inputValue(),/RSI_WORKSPACE_UI_OK/);
 await page.getByTestId('composer-send').click();
 await page.locator('.message.assistant').filter({hasText:'RSI_WORKSPACE_UI_OK'}).waitFor({timeout:180000});
 await page.waitForFunction(()=>document.querySelector('.pane-status')?.textContent==='Completed',{},{timeout:180000});
 assert.equal(await page.getByRole('tab',{name:'Trajectory',exact:true}).count(),1);
 await page.locator('.message.assistant .message-clock').waitFor();
 await page.locator('.message-usage').waitFor();
 await page.getByTestId('session-metrics').waitFor({timeout:60000});
 assert.doesNotMatch(await page.getByTestId('session-metrics').innerText(),/unavailable|Partial statistics/);
 assert.match(await page.getByTestId('session-metrics').innerText(),/tok/);
 assert.equal(await page.locator('.message.status:visible').count(),0);
 await page.screenshot({path:join(report,'conversation.png')});
 await page.locator('.message-usage button').click();
 await page.screenshot({path:join(report,'message-usage.png')});
 await page.locator('.message-usage button').click();
 const statistics=page.getByTestId('session-metrics');
 const readings={};
 for(const [selector,title] of [[/1 turn 1 step/,'Session statistics'],[/Cache hit/,'Token usage'],[/^\d+%$/,'Context usage']]) {
   await statistics.getByRole('button',{name:selector}).click();
   const panel=page.getByRole('dialog',{name:title,exact:true});await panel.waitFor();
   readings[title]=await panel.innerText();
   if(title==='Session statistics') {
     assert.match(readings[title],/LLM time/);assert.match(readings[title],/Total time/);
     assert.match(readings[title],/TTFT/);assert.match(readings[title],/TPS/);
   }
   await page.screenshot({path:join(report,title.replaceAll(' ','-')+'.png')});
   await page.keyboard.press('Escape');await panel.waitFor({state:'detached'});
 }
 await writeFile(join(report,'stat-readings.json'),JSON.stringify(readings,null,2));
 await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
 if(await page.getByRole('button',{name:'Toggle resources',exact:true}).getAttribute('aria-expanded')==='true')await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
 await page.setViewportSize({width:390,height:844});
 assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
 await page.screenshot({path:join(report,'conversation-narrow.png')});
 await page.setViewportSize({width:1710,height:940});
 await input.fill('A retained follow-up');
 await page.getByTestId('new-conversation').click();
 await input.waitFor({state:'visible'});await page.locator('.welcome').waitFor({state:'visible'});
 await input.fill('A new starting draft');
 const resources=page.getByRole('button',{name:'Toggle resources',exact:true});
 if(await resources.getAttribute('aria-expanded')!=='true')await resources.click();
 await page.getByRole('region',{name:'Resource workbench',exact:true}).getByRole('heading',{name:'Session resources'}).waitFor();
 await page.screenshot({path:join(report,'new-conversation.png')});
 await disconnect(page);await page.locator('#login').waitFor({state:'visible'});
 assert.deepEqual(errors,[]);assert.equal(service.provider.requests.length,0);
 const build=JSON.parse(await readFile(join(assets,'rsi-build.json'),'utf8'));
 await writeFile(join(report,'result.json'),JSON.stringify({ok:true,platform:process.platform,browser:browser.version(),model,build,checks:['Windows directory listing','workspace registration','starting text retained on workspace change','real provider first send','transcript tabs','new conversation without model request','resource launcher without Session','clean disconnect'],errors},null,2));
} catch(error) {throw new Error(String(error.stack??error).replaceAll(key,'[REDACTED]'));}
finally {await browser?.close();await service?.close();}
