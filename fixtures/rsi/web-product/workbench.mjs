import assert from 'node:assert/strict';
import {mkdir,writeFile,copyFile,chmod,mkdtemp,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {tmpdir} from 'node:os';
import {chromium,firefox} from 'playwright';
import {startService} from './service.mjs';

const root=resolve(import.meta.dirname,'../../..');
const report=process.env.RSI_WEB_REPORT;
if(!report || !process.env.RSI_WEB_ASSETS) throw new Error('Set explicit RSI_WEB_REPORT and RSI_WEB_ASSETS');
await mkdir(report,{recursive:true});
const temporary=await mkdtemp(join(tmpdir(),'rsi-workbench-'));
const binary=join(temporary,'rsi');await copyFile(process.env.RSI_WEB_BINARY ?? join(root,'target/debug/rsi'),binary);await chmod(binary,0o700);
try {
  for(const [name,engine] of Object.entries({chromium,firefox})) {
    if(process.env.RSI_WEB_BROWSER && process.env.RSI_WEB_BROWSER!==name) continue;
    const directory=join(report,name);await mkdir(directory,{recursive:true});
    let endpoint;
    const service=await startService({binary,assets:process.env.RSI_WEB_ASSETS,report:directory,configure:async({config,provider})=>{
      endpoint=provider.origin;
      await writeFile(join(config,'settings.json'),JSON.stringify({'rsi.agent':{}}));
      await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),'format = 1\nsteps = []\n');
    }});
    const browser=await engine.launch();
    const errors=[]; let page;
    try {
      const context=await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1440,height:980}});
      page=await context.newPage();page.on('pageerror',error=>{errors.push(error.stack ?? error.message);console.error('page error',error.message)});
      await page.goto(service.origin);
      await page.getByLabel('Device registration receipt').fill(JSON.stringify(service.register(`${name}-workbench`)));
      await page.getByRole('button',{name:'Connect',exact:true}).click();
      await page.locator('#workbench').waitFor({state:'visible',timeout:30000});
      await page.locator('.setup-prompt').waitFor({state:'visible'});
      await page.getByText('Add workspace',{exact:true}).first().click();
      await page.getByLabel('Server directory').fill(service.workspace);
      await page.locator('#workspace-form').getByRole('button',{name:'Add workspace',exact:true}).click();
      await page.locator('#workspaces .nav-item').waitFor();
      await page.getByRole('button',{name:'Settings',exact:true}).click();
      await page.getByLabel('Provider',{exact:true}).selectOption('openai-compatible');
      await page.getByRole('button',{name:'Check credential',exact:true}).click();
      await page.getByText('configured · read only',{exact:true}).waitFor();
      await page.getByLabel('Deployment name').fill('first-provider');
      await page.getByLabel('Provider endpoint').fill(endpoint);
      await page.getByLabel('Request path').fill('/v1/chat/completions');
      await page.getByLabel('Model identifier 1',{exact:true}).fill('fixture-model');
      await page.getByRole('button',{name:'Apply provider',exact:true}).click();
      await page.getByText('Desired 1 · Applied 1',{exact:true}).waitFor({timeout:30000});
      await page.getByLabel('Default model',{exact:true}).selectOption({label:'fixture-model · first-provider'});
      await page.getByText('default_model · confirmed',{exact:true}).waitFor();
      await page.getByLabel('Default model',{exact:true}).waitFor({state:'visible'});
      await page.waitForFunction(()=>!document.querySelector('[aria-label="Default model"]').disabled);
      await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
      await page.screenshot({path:join(directory,'setup.png'),fullPage:true});
      await page.getByRole('button',{name:'Close settings',exact:true}).click();
      await page.locator('#workspaces .nav-item').click();
      const input=page.getByLabel('Main message',{exact:true});
      await input.waitFor();
      await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
      const emptyIdentity=await page.locator('.pane-session').innerText();
      const emptyEditor=await input.elementHandle();
      await page.locator('#workspaces .nav-item').click();
      await page.waitForFunction(()=>!document.querySelector('[aria-label="Main message"]').disabled);
      assert.equal(await page.locator('.pane-session').innerText(),emptyIdentity,'matching owned empty draft is reused');
      assert(await emptyEditor.evaluate(node=>node===document.querySelector('[aria-label="Main message"]')),'reuse keeps the editor DOM identity');
      await emptyEditor.dispose();
      await input.fill('First conversation from an empty configuration.');
      await page.getByRole('button',{name:'Send ↗',exact:true}).click();
      await page.locator('.message.assistant').filter({hasText:'Reviewed:'}).waitFor({timeout:30000});
      await page.getByRole('button',{name:'Refresh workspaces and conversations',exact:true}).click();
      await page.locator('#sessions .session-row').waitFor();
      await page.locator('#sessions .session-options summary').click();
      await page.getByRole('button',{name:'Rename',exact:true}).click();
      await page.getByLabel('Conversation title',{exact:true}).fill('First milestone');
      await page.getByRole('button',{name:'Save title',exact:true}).click();
      await page.locator('#sessions strong').filter({hasText:'First milestone'}).waitFor();
      await page.getByRole('button',{name:'Archive',exact:true}).click();
      await page.locator('#sessions .session-row').waitFor({state:'detached'});
      assert(await input.isEnabled(),'archive must leave the attached conversation usable');
      await page.getByLabel('Show archived').check();
      await page.locator('#sessions strong').filter({hasText:'First milestone'}).waitFor();
      await page.getByRole('button',{name:'+ Compare',exact:true}).click();
      await page.locator('#pane-tab-compare').waitFor();
      await page.locator('#sessions .nav-item').click();
      await page.getByLabel('Compare message',{exact:true}).fill('Separate saved compare draft');
      await page.getByRole('button',{name:'Close compare',exact:true}).click();
      await page.getByLabel('Main message',{exact:true}).waitFor({state:'visible'});
      await page.getByRole('button',{name:'Trajectory',exact:true}).click();
      await page.locator('.mode-trajectory').waitFor();
      await page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
      await page.screenshot({path:join(directory,'workbench.png'),fullPage:true});
      for(const width of [1440,1024,768,390]){
        await page.setViewportSize({width,height:900});
        const geometry=await page.evaluate(()=>{const input=document.querySelector('.pane.selected textarea');const send=[...document.querySelectorAll('.pane.selected button')].find(node=>node.textContent==='Send ↗');const rect=send.getBoundingClientRect();return {overflow:document.documentElement.scrollWidth>innerWidth,input:input.getBoundingClientRect().width,send:rect.width,hit:send.contains(document.elementFromPoint(rect.x+rect.width/2,rect.y+rect.height/2))}});
        assert(!geometry.overflow && geometry.input>100 && geometry.send>30 && geometry.hit,JSON.stringify({width,geometry}));
        await page.screenshot({path:join(directory,`responsive-${width}.png`)});
      }
      await page.getByRole('button',{name:'Sign out',exact:true}).click();
      await page.locator('#login').waitFor({state:'visible'});
      assert.deepEqual(errors,[]);
      await writeFile(join(directory,'result.json'),JSON.stringify({browser:browser.version(),emptyConfiguration:true,providerApply:true,defaultModel:true,conversation:true,rename:true,archiveDoesNotDetach:true,keyedCompare:true,responsiveHitTesting:true,errors}));
    } catch(error) {if(page){await page.screenshot({path:join(directory,'failure.png')}).catch(()=>{});await writeFile(join(directory,'failure.html'),await page.content()).catch(()=>{})}await writeFile(join(directory,'errors.json'),JSON.stringify({error:String(error),pageErrors:errors}));throw error}
    finally {await browser.close();await service.close()}
  }
} finally {await rm(temporary,{recursive:true,force:true})}
