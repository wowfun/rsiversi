import {openSettings} from './controls.mjs';
// Opt-in real provider onboarding through the shipped Web/Worker and isolated Service.
import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium} from 'playwright';
import {startService} from './service.mjs';
import {readDeepseekKey} from './live-limits.mjs';
import {workspaceForm} from './controls.mjs';
const report=resolve(process.env.RSI_WEB_REPORT),binary=process.env.RSI_WEB_BINARY,assets=process.env.RSI_WEB_ASSETS;
assert(binary&&assets&&process.env.RSI_LIVE_ENV_FILE,'Explicit live configuration is required');await mkdir(report,{recursive:true});
const key=await readDeepseekKey(process.env.RSI_LIVE_ENV_FILE);let service,browser,page;const errors=[];
try{
 service=await startService({binary,assets,report,developmentHttp:true,configure:async({config})=>{
  await writeFile(join(config,'settings.json'),'{}');await writeFile(join(config,'host-profiles/fixture/host.profile.toml'),'format=1\nsteps=[]\n');
 }});
 await writeFile(join(service.workspace,'keep.txt'),'workspace data must remain');
 browser=await chromium.launch();page=await browser.newPage({viewport:{width:1440,height:960},colorScheme:'dark'});page.on('pageerror',error=>errors.push(error.message));
 const receipt=service.register('model settings acceptance');
 const connect=async()=>{await page.goto(service.origin);await page.getByLabel('Device registration receipt').fill(JSON.stringify(receipt));await page.locator('#dev-http').check();await page.getByRole('button',{name:'Connect',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible',timeout:60000});};
 await connect();
 await workspaceForm(page);await page.getByLabel('Directory path').fill(service.workspace);await page.locator('.directory-picker header').getByRole('button',{name:'Go',exact:true}).click();await page.getByRole('navigation',{name:'Directory breadcrumbs'}).waitFor();await page.locator('.directory-picker footer').getByRole('button',{name:'Open',exact:true}).click();
 const workspace=page.getByTestId('workspace-group').filter({has:page.getByRole('button',{name:'workspace',exact:true})});
 await workspace.getByRole('button',{name:/Workspace actions/}).click();await page.getByRole('menuitem',{name:'Rename',exact:true}).click();await page.getByRole('textbox',{name:'Workspace name'}).fill('Live workspace');await page.getByRole('dialog',{name:'Rename workspace'}).getByRole('button',{name:'Save',exact:true}).click();await page.getByRole('dialog',{name:'Rename workspace'}).waitFor({state:'detached'});
 await openSettings(page);await page.getByRole('button',{name:'Models',exact:true}).click();await page.getByRole('button',{name:'Add model provider',exact:true}).click();
 let editor=page.getByRole('form',{name:'Add model provider'});
 await editor.getByRole('textbox',{name:'API key'}).fill(key);await editor.getByText('Customized settings',{exact:true}).click();await editor.getByRole('button',{name:'Fetch available models'}).click();
 await editor.locator('[aria-label="Available models"]').waitFor({timeout:45000});assert((await editor.locator('[aria-label="Available models"] button').count())>0);
 await editor.getByRole('button',{name:'Create provider',exact:true}).click();
 await editor.waitFor({state:'detached',timeout:60000});
 assert.match(await page.getByRole('combobox',{name:'Default model'}).inputValue(),/deepseek-flash/);
 await page.screenshot({path:join(report,'configured-models.png')});
 await page.getByRole('button',{name:'Edit',exact:true}).click();editor=page.getByRole('form',{name:'Edit model provider'});assert.equal(await editor.getByRole('textbox',{name:'API key'}).inputValue(),'');
 await editor.getByRole('button',{name:'Apply',exact:true}).click();await editor.waitFor({state:'detached',timeout:60000});
 await page.getByRole('button',{name:'Close settings'}).click();
 await page.locator('.workspace-choice > summary').click();await page.getByRole('menuitemradio',{name:'Live workspace',exact:true}).click();
 await page.getByRole('textbox',{name:'Main message',exact:true}).fill('Reply exactly RSI_MODEL_SETUP_OK. Do not use tools.');await page.getByTestId('composer-send').click();await page.locator('.message.assistant').filter({hasText:'RSI_MODEL_SETUP_OK'}).waitFor({timeout:180000});await page.waitForFunction(()=>document.querySelector('.pane-status')?.textContent==='Completed',{},{timeout:180000});
 await page.screenshot({path:join(report,'live-conversation.png')});
 // Settlement invalidates the paged navigation; settle that explicit refresh before hovering a row.
 await page.getByRole('button',{name:'Refresh conversations',exact:true}).click();
 await page.getByRole('button',{name:'Refresh conversations',exact:true}).waitFor({state:'hidden'});
 const renamed=page.getByTestId('workspace-group').filter({has:page.getByRole('button',{name:'Live workspace',exact:true})});await renamed.locator('.workspace-toggle').hover();await page.getByRole('tooltip').waitFor();assert.match(await page.getByRole('tooltip').innerText(),/Created/);await page.screenshot({path:join(report,'workspace-info.png')});
 await service.restart();await page.reload();await page.locator('#dev-http').check();await page.getByRole('button',{name:'Reconnect with this browser',exact:true}).click();await page.locator('#workbench').waitFor({state:'visible',timeout:60000});
 await page.getByRole('button',{name:'Live workspace',exact:true}).waitFor();await openSettings(page);await page.getByRole('button',{name:'Models',exact:true}).click();await page.getByRole('button',{name:'Edit',exact:true}).click();editor=page.getByRole('form',{name:'Edit model provider'});assert.equal(await editor.getByRole('textbox',{name:'API key'}).inputValue(),'');await editor.getByRole('button',{name:'Cancel',exact:true}).click();await page.getByRole('button',{name:'Close settings'}).click();
 await renamed.getByRole('button',{name:/Workspace actions/}).click();await page.getByRole('menuitem',{name:'Delete workspace',exact:true}).click();const dialog=page.getByRole('dialog',{name:'Delete workspace'});await page.screenshot({path:join(report,'delete-confirmation.png')});await dialog.getByRole('button',{name:'Delete workspace',exact:true}).click();await dialog.waitFor({state:'detached'});
 const ungrouped=page.getByTestId('workspace-group').filter({has:page.getByRole('button',{name:'Ungrouped',exact:true})});await ungrouped.getByTestId('conversation-row').first().waitFor({timeout:30000});await ungrouped.getByTestId('conversation-row').first().locator('.nav-item').click();await page.locator('.message.assistant').filter({hasText:'RSI_MODEL_SETUP_OK'}).waitFor();
 assert.equal(await readFile(join(service.workspace,'keep.txt'),'utf8'),'workspace data must remain');await page.screenshot({path:join(report,'ungrouped-preserved.png')});
 assert.deepEqual(errors,[]);assert.equal(service.provider.requests.length,0);
 const build=JSON.parse(await readFile(join(assets,'rsi-build.json'),'utf8'));
 await writeFile(join(report,'result.json'),JSON.stringify({ok:true,platform:process.platform,browser:browser.version(),build,checks:['ephemeral key discovery','one-form provider save and first default','blank key preserves credential','real DeepSeek reply','Service restart preserves provider and workspace name','registration deletion preserves file and transcript in Ungrouped'],errors},null,2));
}catch(error){await page?.screenshot({path:join(report,'failure.png')}).catch(()=>{});throw new Error(String(error.stack??error).replaceAll(key,'[REDACTED]'));}
finally{await browser?.close();await service?.close();}
