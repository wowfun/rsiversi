// Explicit opt-in only: no key is read by the deterministic suite.
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,copyFile,chmod,readdir,lstat} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';

const envPath=process.env.RSI_LIVE_ENV_FILE, report=process.env.RSI_WEB_REPORT;
if(!envPath || !report || !process.env.RSI_WEB_ASSETS || !process.env.RSI_LIVE_MODEL) throw new Error('Set explicit RSI_LIVE_ENV_FILE, RSI_LIVE_MODEL, RSI_WEB_REPORT and RSI_WEB_ASSETS');
const source=await readFile(envPath);assert(source.length<=65536,'live environment file exceeds its bound');
const match=source.toString('utf8').match(/^\s*(?:export\s+)?DEEPSEEK_API_KEY\s*=\s*(.*?)\s*$/m);
assert(match,'authorized DeepSeek key is absent');
const key=match[1].trim().replace(/^(["'])(.*)\1$/,'$2');assert(key.length>0);
await mkdir(report,{recursive:true});
const binary=join(report,'rsi');await copyFile(process.env.RSI_WEB_BINARY ?? resolve('target/debug/rsi'),binary);await chmod(binary,0o700);
const browser=await chromium.launch();let service,page;
const errors=[];const started=Date.now();
try {
  service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report,deepseekKey:key,configure:async({config})=>{
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{}}));
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),'format = 1\nsteps = []\n');
  }});
  const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
  page=await context.newPage();page.on('pageerror',error=>errors.push(error.message));
  await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register('isolated live GUI')));
  await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible'});
  await page.locator('.workspace-add summary').click();await page.getByLabel('Server directory').fill(service.workspace);
  await page.locator('#workspace-form').getByRole('button',{name:'Add workspace',exact:true}).click();
  await page.getByRole('button',{name:'Settings',exact:true}).click();
  await page.getByRole('button',{name:'Check credential',exact:true}).click();
  await page.getByText('configured · read only',{exact:true}).waitFor();
  await page.getByLabel('Deployment name').fill('live-deepseek');await page.getByLabel('Provider endpoint').fill('https://api.deepseek.com');
  await page.getByLabel('DeepSeek protocol').selectOption('chat-completions');await page.getByLabel('Model identifier 1',{exact:true}).fill(process.env.RSI_LIVE_MODEL);
  await page.getByRole('button',{name:'Apply provider',exact:true}).click();await page.getByText('Desired 1 · Applied 1',{exact:true}).waitFor();
  await page.getByLabel('Default model',{exact:true}).selectOption({label:`${process.env.RSI_LIVE_MODEL} · live-deepseek`});
  await page.getByText('default_model · confirmed',{exact:true}).waitFor();
  // The receipt is published before the remaining setup readback finishes.
  await page.waitForFunction(()=>!document.querySelector('select[aria-label="Default model"]').disabled);
  await page.getByRole('button',{name:'Close settings',exact:true}).click();await page.locator('#workspaces .nav-item').click();
  await page.getByRole('button',{name:'Trajectory',exact:true}).click();
  const input=page.getByLabel('Main message',{exact:true});
  await input.fill('This is an isolated GUI integration test. Use the available bash tool to write the UTF-8 line "rsi-live-ok" to milestone.txt in the current workspace, then use bash to read that file back. Do not modify any other file. Reply LIVE_GUI_VERIFIED only after the tool has read the file successfully.');
  await page.getByRole('button',{name:'Send ↗',exact:true}).click();
  const deadline=Date.now()+150000;let approvals=0;
  while(Date.now()<deadline) {
    const review=page.locator('.pending button').filter({hasText:'Review:'});
    if(await review.count() && await review.first().isVisible()) {await review.first().click();await page.getByRole('button',{name:'Allow once',exact:true}).click();approvals++;}
    const status=await page.locator('.pane-status').innerText();
    if(status==='Completed' || status==='Failed') break;
    await page.waitForTimeout(100);
  }
  const transcript=await page.locator('.transcript').innerText();
  const status=await page.locator('.pane-status').innerText();
  assert.equal(status,'Completed',transcript.slice(-4096));
  const file=join(service.workspace,'milestone.txt');assert((await lstat(file)).isFile());
  const bytes=await readFile(file);assert.equal(bytes.toString().trim(),'rsi-live-ok');
  assert.match(transcript,/bash/);assert.match(transcript,/LIVE_GUI_VERIFIED/);assert.equal(service.provider.requests.length,0,'live scenario must not use the fixture provider');
  await page.waitForFunction(()=>document.querySelectorAll('#sessions .session-row').length===1);
  await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  await page.screenshot({path:join(report,'live-conversation.png'),fullPage:true});
  await writeFile(join(report,'transcript.txt'),transcript);
  await page.locator('#sign-out').click();await page.locator('#login').waitFor({state:'visible'});assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),model:process.env.RSI_LIVE_MODEL,protocol:'chat-completions',elapsed_ms:Date.now()-started,approvals,file_bytes:bytes.length,file_text:bytes.toString(),mock_requests:0,clean_sign_out:true},null,2));
} catch(error) {
  if(page) {await page.screenshot({path:join(report,'failure.png'),fullPage:true}).catch(()=>{});await writeFile(join(report,'failure.txt'),String(error).replaceAll(key,'[REDACTED]'));}
  throw new Error(String(error).replaceAll(key,'[REDACTED]'));
} finally {
  await browser.close();await service?.close();
  for(const name of await readdir(report)) if(/\.(json|log|txt)$/.test(name)) {
    const path=join(report,name),text=await readFile(path,'utf8');
    if(text.includes(key)) {await writeFile(path,text.replaceAll(key,'[REDACTED]'));throw new Error('Live evidence contained a credential and was redacted');}
  }
}
