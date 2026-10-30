// Explicit live-provider acceptance: shipped assets, isolated state, one short reply.
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
import {readDeepseekKey} from './live-limits.mjs';
const {RSI_WEB_BINARY:binary,RSI_WEB_ASSETS:assets,RSI_LIVE_ENV_FILE:env}=process.env;
assert(binary&&assets&&env&&process.env.RSI_WEB_REPORT,'Explicit live configuration is required');
const report=resolve(process.env.RSI_WEB_REPORT);await mkdir(report,{recursive:true});
const key=await readDeepseekKey(env);let service,browser,page;const errors=[];
try{
 service=await startService({binary,assets,report,deepseekKey:key,developmentHttp:true,configure:async({config})=>{
  await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{default_model:{deployment:'live',model:'deepseek-flash'}}}));
  await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),`format=1
[[steps]]
kind="plugin"
id="provider"
plugin="rsi.ai.provider.deepseek"
[steps.config]
deployment="live"
endpoint="https://api.deepseek.com"
protocol="chat-completions"
credential={owner="rsi.ai.provider.deepseek",slot="default"}
[steps.config.default_output_tokens]
deepseek-flash=4096
[steps.config.language_models.deepseek-flash]
context_window_tokens=1048576
default_output_reserve_tokens=4096
max_output_reserve_tokens=256000
`);
 }});
 browser=await chromium.launch();page=await browser.newPage({viewport:{width:1440,height:960},colorScheme:'dark'});page.on('pageerror',error=>errors.push(error.message));
 await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register('composer live acceptance')));await page.locator('#dev-http').check();await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible',timeout:60000});
 const input=page.getByRole('textbox',{name:'Main message',exact:true});
 await input.fill('Reply exactly RSI_COMPOSER_OK. Do not call any tools.');
 await page.getByRole('button',{name:'Add files or run commands'}).click();await page.getByRole('menuitem',{name:'File',exact:true}).waitFor();
 await page.getByRole('menuitem',{name:'File',exact:true}).click();await page.getByRole('dialog',{name:'Insert workspace file path'}).waitFor();await page.screenshot({path:join(report,'file-picker.png')});await page.keyboard.press('Escape');
 await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.getByRole('menuitemradio',{name:'Max',exact:true}).click();await page.getByRole('button',{name:/reasoning effort Max/}).waitFor();
 await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.screenshot({path:join(report,'effort-menu.png')});await page.getByRole('menuitemradio',{name:'Off',exact:true}).click();await page.getByRole('button',{name:/reasoning effort Off/}).waitFor();
 await page.getByRole('button',{name:/^Access mode/}).click();await page.screenshot({path:join(report,'access-menu.png')});await page.getByRole('menuitemradio',{name:'Read Only',exact:true}).click();
 assert.equal(await input.inputValue(),'Reply exactly RSI_COMPOSER_OK. Do not call any tools.');
 await page.screenshot({path:join(report,'composer.png')});await page.getByTestId('composer-send').click();await page.locator('.message.assistant').filter({hasText:'RSI_COMPOSER_OK'}).waitFor({timeout:180000});await page.waitForFunction(()=>document.querySelector('.pane-status')?.textContent==='Completed',{},{timeout:180000});
 const session=await page.locator('.pane').getAttribute('data-session-id'),facts=await service.readSessionFacts(session);
 const values=[];function visit(value,path=''){if(value&&typeof value==='object')for(const [key,item]of Object.entries(value)){const next=path+'.'+key;if(['sandbox','reasoning_effort'].includes(key))values.push({path:next,value:item});visit(item,next);}}visit(facts);
 await writeFile(join(report,'policy-evidence.json'),JSON.stringify(values,null,2));
 assert(facts.some(fact=>['turn_accepted','message_turn_accepted'].includes(fact.type)&&fact.sandbox==='read-only'));
 // A Session model command is a domain default, not a Turn-level override.
 // Inspect the actual provider request rather than expecting it on TurnAccepted.
 const downloadReady=page.waitForEvent('download');
 await input.fill('/export -f json -i h,lpr');await page.getByTestId('composer-send').click();
 const download=await downloadReady;assert.equal(await download.failure(),null);
 const exported=JSON.parse(await readFile(await download.path(),'utf8'));
 assert.equal(exported.last_provider_request.availability,'available');
 visit(exported.last_provider_request,'provider_request');
 await writeFile(join(report,'policy-evidence.json'),JSON.stringify(values,null,2));
 assert(values.some(entry=>entry.path.startsWith('provider_request.')&&entry.value==='off'));
 await page.screenshot({path:join(report,'live-reply.png')});
 await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.getByRole('menuitemradio',{name:'Off',exact:true}).waitFor();await page.keyboard.press('Escape');
 assert.deepEqual(errors,[]);assert.equal(service.provider.requests.length,0);
 await writeFile(join(report,'result.json'),JSON.stringify({ok:true,platform:process.platform,browser:browser.version(),build:JSON.parse(await readFile(join(assets,'rsi-build.json'),'utf8')),checks:['starting controls preserve text without sending','real file picker','Max then Off model commands','Read Only request','real DeepSeek completion'],policyEvidence:values,errors},null,2));
}catch(error){await page?.screenshot({path:join(report,'failure.png')}).catch(()=>{});throw new Error(String(error.stack??error).replaceAll(key,'[REDACTED]'));}
finally{await browser?.close();await service?.close();}
