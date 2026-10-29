import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
import {settingsPanel} from './src/bridge.ts';
import {flushSync} from 'react-dom';
fixture.flushEffects=()=>flushSync(()=>{});
fixture.openModels=()=>settingsPanel.set('models');
fixture.view.catalog.workspaces[0].created_at_ms=1793094120000;
fixture.view.setup={...fixture.view.setup,allowed:true,credentials_available:true,providers_writable:true,settings_writable:true,agent:{default_model:null},providers:{desired_revision:'1',applied_revision:'1',applying:false,diagnostic:null,deployments:[{provider:'deepseek',config:{deployment:'deepseek',endpoint:'https://api.deepseek.com',credential:{owner:'rsi.ai.provider.deepseek',slot:'default'},protocol:'responses',language_models:{'deepseek-flash':{context_window_tokens:128000,default_output_reserve_tokens:4096,max_output_reserve_tokens:16384}},default_output_tokens:{'deepseek-flash':8192}}}]}};
fixture.setReply('command',async text=>{
 const value=JSON.parse(text),command=value.command,setup=fixture.view.setup;
 if(value.action==='rename_workspace'){
  if(fixture.renameFailure)throw new Error('Rename unavailable');
  fixture.view.catalog.workspaces[0].name=value.name;
 }
 if(value.action==='delete_workspace'){fixture.view.catalog.workspaces=[];fixture.view.catalog.workspace_order_seed.records=[];}
 if(value.action==='setup'){
  if(command.kind==='credential_status'){
   if(fixture.holdCredential)await new Promise(resolve=>{fixture.releaseCredential=()=>{fixture.holdCredential=false;fixture.releaseCredential=undefined;resolve()};});
   setup.credential={provider:command.provider,slot:command.slot,status:{editable:true,store_path:null,availability:{kind:'configured',source:{kind:'file'}}}};
  }
  if(command.kind==='providers_replace'){
   if(fixture.providerFailure)throw new Error('Provider rejected');
   if(command.expected_revision!==setup.providers.desired_revision)throw new Error('Revision conflict');
   setup.providers={...setup.providers,deployments:command.deployments,desired_revision:String(Number(setup.providers.desired_revision)+1),applied_revision:String(Number(setup.providers.desired_revision)+1)};
   setup.ticket+='n';
  }
  if(command.kind==='default_model')setup.agent.default_model=command.model;
  if(command.kind==='discover_models')setup.discovery={request:command.input.request,models:[{id:'candidate',name:'Candidate',context_window_tokens:1000000,max_output_tokens:256000}]};
 }
 await fixture.frame();return '{}';
});
`;
const fixture=await sourceFixture({name:'workspace-model-settings',source}),results=[];
try{for(const [name,engine] of Object.entries({chromium,firefox})){
 const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900},colorScheme:'dark'}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 try{
  await page.goto(fixture.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  const group=page.getByTestId('workspace-group').first();
  await group.locator('.workspace-toggle').hover();await page.getByRole('tooltip').waitFor();
  assert.match(await page.getByRole('tooltip').innerText(),/Created/);await page.screenshot({path:join(report,name+'-workspace-info.png')});
  await group.getByRole('button',{name:/Workspace actions/}).click();await page.getByRole('menuitem',{name:'Rename',exact:true}).waitFor();
  assert.equal(await page.getByRole('tooltip').count(),0);await page.screenshot({path:join(report,name+'-workspace-menu.png')});
  await page.getByRole('menuitem',{name:'Rename',exact:true}).click();const rename=page.getByRole('dialog',{name:'Rename workspace'});
  await rename.getByRole('textbox',{name:'Workspace name'}).fill('Project renamed');await page.evaluate(()=>{fixture.renameFailure=true;});
  await rename.getByRole('button',{name:'Save',exact:true}).click();await rename.getByRole('alert').waitFor();assert.equal(await rename.getByRole('textbox').inputValue(),'Project renamed');
  await page.evaluate(()=>{fixture.renameFailure=false;});await rename.getByRole('button',{name:'Save',exact:true}).click();await rename.waitFor({state:'detached'});
  await group.getByRole('button',{name:'Project renamed',exact:true}).waitFor();
  await group.getByRole('button',{name:/Workspace actions/}).click();await page.getByRole('menuitem',{name:'Delete workspace',exact:true}).click();
  const deletion=page.getByRole('dialog',{name:'Delete workspace'});assert.match(await deletion.innerText(),/folder and session logs will be kept/);
  assert.equal(await deletion.getByRole('button',{name:'Cancel'}).evaluate(node=>node===document.activeElement),true);
  await page.screenshot({path:join(report,name+'-workspace-delete.png')});await page.keyboard.press('Escape');await deletion.waitFor({state:'detached'});
  await group.getByRole('button',{name:/Workspace actions/}).click();await page.getByRole('menuitem',{name:'Delete workspace',exact:true}).click();await deletion.getByRole('button',{name:'Delete workspace',exact:true}).click();await deletion.waitFor({state:'detached'});
  assert.equal(await page.getByRole('button',{name:'Project renamed',exact:true}).count(),0);
  await page.evaluate(()=>fixture.openModels());await page.getByRole('heading',{name:'Models',exact:true}).waitFor();
  await page.evaluate(()=>{fixture.view.setup.diagnostic='Provider configuration could not be applied';fixture.view.setup.providers_writable=false;return fixture.frame()});
  await page.getByRole('alert').filter({hasText:'Provider configuration could not be applied'}).waitFor();
  await page.getByRole('status').filter({hasText:'Provider configuration is read only'}).waitFor();
  await page.evaluate(()=>{fixture.view.setup.diagnostic=null;fixture.view.setup.providers_writable=true;return fixture.frame()});
  await page.screenshot({path:join(report,name+'-models.png')});await page.getByRole('button',{name:'Edit',exact:true}).click();
  const editor=page.getByRole('form',{name:'Edit model provider'});await editor.getByRole('textbox',{name:'API key'}).waitFor();
  assert.equal(await editor.getByRole('textbox',{name:'API key'}).inputValue(),'');
  await editor.getByRole('textbox',{name:'API key'}).fill('fixture-unsaved-key');
  await page.getByRole('button',{name:'General',exact:true}).click();
  await page.getByRole('button',{name:'Models',exact:true}).click();
  assert.equal(await editor.getByRole('textbox',{name:'API key'}).inputValue(),'fixture-unsaved-key');
  await editor.getByRole('textbox',{name:'API key'}).fill('');
  assert.equal(await editor.getByRole('textbox',{name:'Base URL'}).isVisible(),false);
  await page.screenshot({path:join(report,name+'-provider-key.png')});await editor.getByText('Customized settings',{exact:true}).click();
  await editor.getByText('Connection identifiers',{exact:true}).click();
  const slot=editor.getByRole('textbox',{name:'Credential slot',exact:true});
  await page.waitForFunction(()=>fixture.view.setup.credential?.slot==='default');
  assert.equal(await editor.getByRole('textbox',{name:'API key'}).getAttribute('placeholder'),'Leave blank to use saved credentials');
  await page.clock.install();await page.clock.pauseAt(new Date());
  const checks=()=>page.evaluate(()=>fixture.calls.filter(call=>call.method==='command'&&JSON.parse(call.payload).command?.kind==='credential_status').length);
  const beforeChecks=await checks();
  await slot.fill('');await slot.pressSequentially('new-slot',{delay:10});
  await page.evaluate(()=>fixture.flushEffects());
  assert.equal(await checks(),beforeChecks,'Slot typing must not issue status checks');
  await page.clock.runFor(350);assert.equal(await checks(),beforeChecks+1);
  await slot.fill('cancelled-slot');await editor.getByRole('button',{name:'Cancel',exact:true}).click();
  await page.clock.runFor(350);assert.equal(await checks(),beforeChecks+1,'Closing cancels a pending status check');
  await page.clock.resume();await page.evaluate(()=>{fixture.holdCredential=true;});
  await page.getByRole('button',{name:'Edit',exact:true}).click();
  await page.waitForFunction(()=>!!fixture.releaseCredential);
  const writes=()=>page.evaluate(()=>fixture.calls.filter(call=>call.method==='command'&&JSON.parse(call.payload).command?.kind==='providers_replace').length);
  const beforeWrites=await writes();
  await editor.getByRole('button',{name:'Apply',exact:true}).click();
  await page.waitForFunction(()=>document.querySelector('form[aria-label="Edit model provider"] fieldset')?.disabled);
  assert.equal(await writes(),beforeWrites,'Apply waits for the in-flight status read');
  await page.evaluate(()=>fixture.releaseCredential());await editor.waitFor({state:'detached'});
  await page.getByRole('button',{name:'Edit',exact:true}).click();
  await editor.getByText('Customized settings',{exact:true}).click();
  await editor.getByRole('button',{name:'Model settings 1'}).click();await editor.getByRole('textbox',{name:'Context window 1'}).fill('1M');
  await editor.getByRole('button',{name:'Fetch available models'}).click();await editor.getByRole('button',{name:'Candidate Add'}).click();
  assert.equal(await editor.getByRole('textbox',{name:'Model ID 2'}).inputValue(),'candidate');
  await page.screenshot({path:join(report,name+'-provider-models.png')});
  await editor.getByRole('button',{name:'Apply',exact:true}).click();await editor.waitFor({state:'detached'});
  const saved=await page.evaluate(()=>fixture.view.setup.providers.deployments[0]);assert.equal(saved.config.language_models['deepseek-flash'].context_window_tokens,1000000);assert.deepEqual(saved.config.default_output_tokens,{'deepseek-flash':8192,candidate:4096});
  assert.equal((await page.evaluate(()=>fixture.calls.filter(call=>call.method==='command').map(call=>JSON.parse(call.payload)).filter(value=>value.command?.kind==='credential_set'))).length,0);
  await page.getByRole('button',{name:'Add model provider',exact:true}).click();const add=page.getByRole('form',{name:'Add model provider'});
  await add.getByRole('button',{name:'Custom model API',exact:true}).click();await add.getByRole('textbox',{name:'Provider ID',exact:true}).first().fill('gateway');await add.getByRole('textbox',{name:'Base URL'}).fill('http://127.0.0.1:9999/v1');await add.getByRole('textbox',{name:'API key'}).fill('fixture-only-key');await add.getByText('Customized settings',{exact:true}).click();await add.getByRole('button',{name:'Fetch available models'}).click();await add.getByRole('button',{name:'Candidate Add'}).click();
  await page.evaluate(()=>{fixture.providerFailure=true;});await add.getByRole('button',{name:'Create provider'}).click();await add.getByRole('alert').waitFor();assert.equal(await add.getByRole('textbox',{name:'API key'}).inputValue(),'');assert.match(await add.innerText(),/API key saved/);
  await page.evaluate(()=>{fixture.providerFailure=false;});await add.getByRole('button',{name:'Create provider'}).click();await add.waitFor({state:'detached'});
  await page.getByRole('button',{name:'Add model provider',exact:true}).click();
  await page.setViewportSize({width:390,height:844});await page.screenshot({path:join(report,name+'-narrow.png')});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth),false);
  await page.emulateMedia({colorScheme:'light'});await page.screenshot({path:join(report,name+'-light.png')});
  assert.deepEqual(errors,[]);results.push({browser:name,workspace_hover_rename_delete:true,provider_blank_key_preserved:true,partial_save_retry:true,discovery_candidates:true,model_limits:true,responsive:true});
 }finally{await browser.close();}
}}finally{await fixture.close();}
await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
