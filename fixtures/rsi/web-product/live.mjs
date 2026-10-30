import {openSettings} from './controls.mjs';
import {disconnect} from './controls.mjs';
import {addWorkspace,addModelProvider} from './controls.mjs';
import {detailMode} from './controls.mjs';
import './paired-env.mjs';
// Explicit opt-in only: no key is read by the deterministic suite.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,readdir,lstat} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';
import {assertNoNotices} from './task-checks.mjs';
import {liveLimits,readDeepseekKey} from './live-limits.mjs';

const envPath=process.env.RSI_LIVE_ENV_FILE, report=process.env.RSI_WEB_REPORT;
if(!envPath || !report || !process.env.RSI_WEB_ASSETS || !process.env.RSI_LIVE_MODEL) throw new Error('Set explicit RSI_LIVE_ENV_FILE, RSI_LIVE_MODEL, RSI_WEB_REPORT and RSI_WEB_ASSETS');
const limits=liveLimits();
const key=await readDeepseekKey(envPath);
await mkdir(report,{recursive:true});
const binary=process.env.RSI_WEB_BINARY;
const browserName=process.env.RSI_WEB_BROWSER??'chromium';
assert(['chromium','firefox'].includes(browserName),'Unknown live browser');
const browser=await ({chromium,firefox})[browserName].launch();let service,page;
const errors=[],networkFailures=[];let omittedNetworkFailures=0;const started=Date.now();
try {
  service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report,deepseekKey:key,configure:async({config})=>{
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{output_budgets:[{model:{deployment:'live-deepseek',model:process.env.RSI_LIVE_MODEL},max_output_tokens:4096}]}}));
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),'format = 1\nsteps = []\n');
  }});
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
  page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  page.on('requestfailed',request=>{
    if(networkFailures.length===64){omittedNetworkFailures++;return;}
    networkFailures.push({method:request.method().slice(0,32),path:new URL(request.url()).pathname.slice(0,2048),error:request.failure()?.errorText?.slice(0,1024),elapsed_ms:Date.now()-started});
  });
  await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register('isolated live GUI')));
  await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await addWorkspace(page,service.workspace);
  await openSettings(page);
  await page.getByRole('button',{name:'Models',exact:true}).click();
  await addModelProvider(page,{id:'live-deepseek',endpoint:'https://api.deepseek.com',provider:'deepseek',model:process.env.RSI_LIVE_MODEL,limits});
  await page.getByRole('button',{name:'Close settings',exact:true}).click();await page.getByRole('button',{name:'New conversation in workspace',exact:true}).click();
  await detailMode(page,'verbose');
  const input=page.getByLabel('Main message',{exact:true});
  await input.fill('This is an isolated GUI integration test. Use the available bash tool to write exactly 12 UTF-8 bytes: "rsi-live-ok" followed by one LF newline to milestone.txt in the current workspace, then use bash to read that file back. Do not modify any other file. Reply LIVE_GUI_VERIFIED only after the tool has read the file successfully.');
  await page.getByTestId('composer-send').click();
  const deadline=Date.now()+420000;let approvals=0;
  while(Date.now()<deadline) {
    assert(await page.locator('#workbench').isVisible(),'Live browser connection was lost');
    const review=page.locator('.pending button').filter({hasText:'Review:'});
    if(await review.count() && await review.first().isVisible()) {await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approvals++;}
    const status=await page.locator('.pane-status').innerText();
    if(['Completed','Failed','Interrupted','Cancelled'].includes(status)) break;
    await page.waitForTimeout(100);
  }
  const transcript=await page.locator('.transcript').innerText();
  const status=await page.locator('.pane-status').innerText();
  assert.equal(status,'Completed',transcript.slice(-4096));
  const file=join(service.workspace,'milestone.txt');assert((await lstat(file)).isFile());
  const bytes=await readFile(file);assert.equal(bytes.toString(),'rsi-live-ok\n');
  assert.match(transcript,/bash/);assert.match(transcript,/LIVE_GUI_VERIFIED/);assert.equal(service.provider.requests.length,0,'live scenario must not use the fixture provider');
  await page.waitForFunction(()=>document.querySelectorAll('#sessions .session-row').length===1);
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  await page.screenshot({path:join(report,'live-conversation.png'),fullPage:true});
  await writeFile(join(report,'transcript.txt'),transcript);
  await assertNoNotices(page);
  await disconnect(page);await page.locator('#login').waitFor({state:'visible'});assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browserName,version:browser.version(),model:process.env.RSI_LIVE_MODEL,limits,protocol:'chat-completions',elapsed_ms:Date.now()-started,approvals,file_bytes:bytes.length,file_text:bytes.toString(),mock_requests:0,clean_sign_out:true},null,2));
} catch(error) {
  if(page) {await page.screenshot({path:join(report,'failure.png'),fullPage:true}).catch(()=>{});await writeFile(join(report,'failure.txt'),String(error).replaceAll(key,'[REDACTED]'));await writeFile(join(report,'failure-network.json'),JSON.stringify({errors,networkFailures,omittedNetworkFailures},null,2).replaceAll(key,'[REDACTED]'));}
  throw new Error(String(error).replaceAll(key,'[REDACTED]'));
} finally {
  await browser.close();await service?.close();
  for(const name of await readdir(report)) if(/\.(json|log|txt)$/.test(name)) {
    const path=join(report,name),text=await readFile(path,'utf8');
    if(text.includes(key)) {await writeFile(path,text.replaceAll(key,'[REDACTED]'));throw new Error('Live evidence contained a credential and was redacted');}
  }
}
