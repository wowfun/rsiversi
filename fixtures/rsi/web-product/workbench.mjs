import {openSettings} from './controls.mjs';
import {disconnect} from './controls.mjs';
import {addWorkspace,addModelProvider,navigationOptions} from './controls.mjs';
import {detailMode,navigationFilter,selectSurface} from './controls.mjs';
import './paired-env.mjs';
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';

const root=resolve(import.meta.dirname,'../../..');
const report=process.env.RSI_WEB_REPORT;
if(!report || !process.env.RSI_WEB_ASSETS || !process.env.RSI_WEB_BINARY) throw new Error('Set explicit RSI_WEB_REPORT, RSI_WEB_ASSETS and RSI_WEB_BINARY');
await mkdir(report,{recursive:true});
const binary=process.env.RSI_WEB_BINARY;
const staleDefault=process.env.RSI_WEB_STALE_DEFAULT==='1';
for(const [name,engine] of Object.entries({chromium,firefox})) {
  if(process.env.RSI_WEB_BROWSER && process.env.RSI_WEB_BROWSER!==name) continue;
  const directory=join(report,name);await mkdir(directory,{recursive:true});
  let endpoint;
  const service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report:directory,configure:async({config,provider})=>{
    endpoint=provider.origin;
    await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':staleDefault?{default_model:{deployment:'removed-provider',model:'unavailable-model'}}:{}}));
    await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),'format = 1\nsteps = []\n');
  }});
  const browser=await engine.launch();
  const errors=[]; let page;
  try {
    const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
    await context.addInitScript(()=>{
      window.navigationQueries=0;const Original=Worker;
      window.Worker=class extends Original {
        constructor(...args){super(...args);this.queries=new Set();this.addEventListener('message',({data})=>{if(data.kind==='reply'&&this.queries.delete(data.id)&&!data.error)window.navigationQueries++})}
        postMessage(data,...rest){if(data.kind==='call'&&data.method==='command'){const command=JSON.parse(data.payload);if(command.action==='navigate'&&command.command.kind==='query')this.queries.add(data.id)}super.postMessage(data,...rest)}
      };
    });
    page=await context.newPage();page.on('pageerror',error=>{errors.push(error.stack ?? error.message);console.error('page error',error.message)});
    await page.goto(service.origin);
    await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register(`${name}-workbench`)));
    await page.getByRole('button',{name:'Connect',exact:true}).click();
    await page.locator('#workbench').waitFor({state:'visible',timeout:30000});
    if(!staleDefault)await page.getByRole('button',{name:'Choose model',exact:true}).waitFor();
    await addWorkspace(page,service.workspace);
    await openSettings(page);
    await page.getByRole('button',{name:'Models',exact:true}).click();
    await addModelProvider(page,{id:'first-provider',endpoint,model:'fixture-model'});
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    await page.screenshot({path:join(directory,'setup.png'),fullPage:true});
    await page.getByRole('button',{name:'Close settings',exact:true}).click();
    await page.getByRole('button',{name:'New conversation in workspace',exact:true}).click();
    const input=page.getByLabel('Main message',{exact:true});
    await input.waitFor();
    await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
    const emptyIdentity=await page.locator('.pane.selected').getAttribute('data-session-id');
    assert(emptyIdentity);
    const emptyEditor=await input.elementHandle();
    await page.getByRole('button',{name:'New conversation in workspace',exact:true}).click();
    await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
    assert.equal(await page.locator('.pane.selected').getAttribute('data-session-id'),emptyIdentity,'matching owned empty draft is reused');
    assert(await emptyEditor.evaluate(node=>node===document.querySelector('[aria-label="Main message"]')),'reuse keeps the editor DOM identity');
    await emptyEditor.dispose();
    await input.fill('First conversation from an empty configuration.');
    await page.getByTestId('composer-send').click();
    await page.locator('.message.assistant').filter({hasText:'Reviewed:'}).waitFor({timeout:30000});
    await page.waitForFunction(()=>document.querySelector('.pane.selected .pane-status')?.textContent==='Completed');
    const queries=await page.evaluate(()=>window.navigationQueries);
    await navigationOptions(page);
    await page.getByRole('menuitem',{name:'Refresh',exact:true}).click();
    await page.waitForFunction(before=>window.navigationQueries>before,queries);
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    await page.locator('#sessions .session-row').waitFor();
    await page.locator('#sessions .session-options summary').click();
    await page.getByRole('button',{name:'Rename',exact:true}).click();
    await page.getByLabel('Conversation title',{exact:true}).fill('First milestone');
    await page.getByRole('button',{name:'Save title',exact:true}).click();
    await page.getByLabel('Conversation title',{exact:true}).waitFor({state:'hidden'});
    await page.getByRole('button',{name:'Refresh conversations in workspace',exact:true}).click();
    await page.locator('#sessions strong').filter({hasText:'First milestone'}).waitFor();
    const options=page.locator('#sessions .session-options');
    if(!await options.evaluate(node=>node.open))await options.locator('summary').click();
    await page.getByRole('button',{name:'Archive',exact:true}).click();
    await page.locator('#sessions .session-row').waitFor({state:'detached'});
    assert(await input.isEnabled(),'archive must leave the attached conversation usable');
    await navigationFilter(page,'archived');
    await page.locator('#sessions strong').filter({hasText:'First milestone'}).waitFor();
    await page.locator('.conversation-menu>summary').click();await page.locator('#add-surface').click();await page.locator('.conversation-menu>summary').click();
    await page.locator('#pane-tab-compare').waitFor({state:'attached'});
    await page.locator('#sessions .nav-item').click();
    await page.getByLabel('Compare message',{exact:true}).fill('Separate saved compare draft');
    // A wide viewport can still have a narrow conversation column beside resources.
    await page.setViewportSize({width:1024,height:768});
    await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
    await page.locator('#pane-tab-compare').waitFor({state:'hidden'});
    await selectSurface(page,'main');
    await selectSurface(page,'compare');
    await page.locator('.conversation-menu>summary').click();
    await page.getByRole('button',{name:'Close compare',exact:true}).click();
    await page.locator('.conversation-menu>summary').click();
    await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
    await page.getByLabel('Main message',{exact:true}).waitFor({state:'visible'});
    await detailMode(page,'verbose');
    await page.waitForFunction(()=>document.documentElement.dataset.detail==='verbose');
    await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
    await page.screenshot({path:join(directory,'workbench.png'),fullPage:true});
    for(const width of [1440,1024,768,390]){
      await page.setViewportSize({width,height:900});
      const geometry=await page.evaluate(()=>{const input=document.querySelector('.pane.selected textarea');const send=[...document.querySelectorAll('.pane.selected button')].find(node=>node.dataset.testid==='composer-send');const rect=send.getBoundingClientRect();return {overflow:document.documentElement.scrollWidth>innerWidth,input:input.getBoundingClientRect().width,send:rect.width,hit:send.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2))}});
      assert(!geometry.overflow && geometry.input>100 && geometry.send>30 && geometry.hit,JSON.stringify({width,geometry}));
      await page.screenshot({path:join(directory,`responsive-${width}.png`)});
    }
    await disconnect(page);
    await page.locator('#login').waitFor({state:'visible'});
    assert.deepEqual(errors,[]);
    await writeFile(join(directory,'result.json'),JSON.stringify({browser:browser.version(),emptyConfiguration:!staleDefault,staleDefaultRecovery:staleDefault,providerApply:true,defaultModel:true,conversation:true,rename:true,archiveDoesNotDetach:true,keyedCompare:true,responsiveHitTesting:true,errors}));
  } catch(error) {if(page){await page.screenshot({path:join(directory,'failure.png')}).catch(()=>{});await writeFile(join(directory,'failure.html'),await page.content()).catch(()=>{})}await writeFile(join(directory,'errors.json'),JSON.stringify({error:String(error),pageErrors:errors}));throw error}
  finally {await browser.close();await service.close()}
}
