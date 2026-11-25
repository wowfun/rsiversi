import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR??'.local/notes/1027/composer-controls');await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.base=structuredClone(fixture.view.surfaces.main);fixture.view.surfaces.main=null;
fixture.view.setup.agent.default_reasoning_effort='high';
fixture.view.setup.default_model_description={model:fixture.view.setup.agent.default_model,profile:{reasoning_efforts:{supported:['off','high'],default:'off'}}};
fixture.view.catalog.workspaces[0].id='f'.repeat(64);
fixture.creates=0;fixture.commandReads=0;fixture.modelWrites=0;fixture.prepared=[];fixture.view.has_media=true;
fixture.view.catalog.models.push({deployment:'fixture',model:'other'});
fixture.setReply('command',async payload=>{
 const command=JSON.parse(payload);
 if(command.action==='commands')fixture.commandReads++;
 if(command.action==='create'){
  fixture.creates++;fixture.view.surfaces.main={...structuredClone(fixture.base),generation:'2',selection:'2',workspace:fixture.attachedWorkspace(command.workspace),creation:{workspace_id:command.workspace,session_id:fixture.base.session,agent_preset_id:null},sandbox:'workspace-write',transcript:{blocks:[],status:'Ready'},effort_profile:{supported:['off','low','high','max'],default:'high'},commands:{revision:{kind:'draft'},commands:[{name:'plan',description:'Plan mode',draft_safe:true,composer_input:{kind:'text',hint:'[off|message]'}}]}};
 }
 const data=fixture.view.surfaces.main;
 if(command.action==='model'){fixture.modelWrites++;data.model=command.model;data.reasoning_effort=command.reasoning_effort;}
 await fixture.frame();return '{}';
});
fixture.setReply('prepare_submission',payload=>{const input=JSON.parse(payload);fixture.prepared.push(input);return JSON.stringify({kind:'message',id:'request-'+fixture.prepared.length,opaque:'opaque',text_bytes:input.text.length,images:0,captures: []});});
fixture.setReply('dispatch_submission',()=>JSON.stringify({status:'not_admitted',error:'fixture retained request'}));
fixture.permissionCommands(fixture.base);
`;
const scene=await sourceFixture({name:'composer-controls',source});const results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})){
 const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900},colorScheme:'dark'}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 try{
  await page.goto(scene.url,{waitUntil:'domcontentloaded',timeout:90000});await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  const startingModel=page.getByRole('button',{name:/^Select model.*reasoning effort High/});
  await startingModel.waitFor();
  await page.evaluate(()=>{delete fixture.view.setup.agent.default_reasoning_effort;return fixture.frame();});
  await page.getByRole('button',{name:/^Select model.*reasoning effort Off/}).waitFor();
  await page.evaluate(()=>{fixture.view.setup.agent.default_reasoning_effort='high';return fixture.frame();});
  await startingModel.waitFor();
  const input=page.locator('.composer textarea');await input.fill('Draft survives opening controls');await input.press('End');
  const add=page.getByRole('button',{name:'Add files or run commands'});
  const menu=page.locator('[role="listbox"][aria-label="Add files or run commands"]');
  assert.equal(await menu.count(),0);
  await add.focus();await add.press('Enter');await page.getByRole('option',{name:'File',exact:true}).waitFor();
  assert(await input.evaluate(node=>node===document.activeElement));
  const card=await page.locator('.composer').boundingBox(),overlay=await menu.boundingBox();
  assert(Math.abs(overlay.x-card.x)<2 && Math.abs(overlay.width-card.width)<2 && overlay.y+overlay.height<=card.y);
  await input.press('ArrowDown');
  assert.equal(await menu.getByRole('option',{selected:true}).getAttribute('id'),await input.getAttribute('aria-activedescendant'));
  assert.deepEqual(await page.evaluate(()=>[fixture.creates,fixture.commandReads]),[0,0]);
  assert.equal(await input.getAttribute('placeholder'),'Describe what you want to build');
  assert.equal(await input.inputValue(),'Draft survives opening controls');assert.equal(await page.evaluate(()=>fixture.prepared.length),0);
  await page.screenshot({path:join(report,name+'-plus.png')});
  await page.keyboard.press('Escape');assert.equal(await menu.count(),0);
  assert(await input.evaluate(node=>node===document.activeElement));
  await add.click();
  await input.press('Shift+Tab');assert.equal(await menu.count(),0);assert(await input.evaluate(node=>node===document.activeElement));
  await add.click();await input.press('ArrowUp');
  assert.equal(await menu.getByRole('option',{selected:true}).getAttribute('aria-label'),'Commands');
  await input.press('Tab');
  await page.getByRole('option',{name:'/plan',exact:true}).click();assert.equal(await input.inputValue(),'/plan Draft survives opening controls');
  assert.deepEqual(await page.evaluate(()=>[fixture.creates,fixture.commandReads]),[1,1]);
  await page.screenshot({path:join(report,name+'-command.png')});
  await page.getByRole('button',{name:'Leave command input'}).click();assert.equal(await input.inputValue(),'Draft survives opening controls');
  for(const theme of ['light','dark']){
   await page.evaluate(theme=>document.documentElement.dataset.theme=theme,theme);await input.focus();await input.press('Home');
   assert.equal(await page.locator('.composer').evaluate(node=>getComputedStyle(node).outlineWidth),'2px');
   assert.equal(await input.evaluate(node=>getComputedStyle(node).outlineStyle),'none');
   await page.screenshot({path:join(report,name+'-focus-'+theme+'.png')});
  }
  await page.emulateMedia({forcedColors:'active'});await input.focus();assert.equal(await page.locator('.composer').evaluate(node=>getComputedStyle(node).outlineWidth),'2px');
  await page.screenshot({path:join(report,name+'-focus-forced.png')});await page.emulateMedia({forcedColors:'none'});
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();
  await page.getByRole('menuitemradio',{name:'High',exact:true}).click();
  assert.equal(await page.evaluate(()=>fixture.view.surfaces.main.reasoning_effort),'high');
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();
  await page.getByRole('menuitemradio',{name:/^Default/}).click();
  assert.equal(await page.evaluate(()=>fixture.view.surfaces.main.reasoning_effort),null);
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.getByRole('menuitemradio',{name:'Max',exact:true}).click();
  await page.getByRole('button',{name:/reasoning effort Max/}).waitFor();
  const currentModel=page.getByRole('button',{name:/^Select model.*reasoning effort Max/});
  const writes=await page.evaluate(()=>fixture.modelWrites);
  await currentModel.click();await page.getByRole('menuitem',{name:/^Model /}).click();
  await page.getByRole('menuitemradio',{checked:true}).click();
  await currentModel.click();await page.getByRole('menuitem',{name:/^Effort/}).click();
  await page.getByRole('menuitemradio',{name:'Max',exact:true}).click();
  assert.equal(await page.evaluate(()=>fixture.modelWrites),writes);
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.screenshot({path:join(report,name+'-effort.png')});
  await page.keyboard.press('Escape');assert.equal(await page.getByRole('button',{name:/^Select model/}).evaluate(node=>node===document.activeElement),true);
  const access=page.getByRole('button',{name:/^Access mode/}), confirmation=page.getByRole('dialog',{name:'Enable Full access?'});
  await access.click();await page.getByRole('menuitemradio',{name:'Full access',exact:true}).click();
  const enable=confirmation.getByRole('button',{name:'Enable Full access',exact:true}),acknowledgement=confirmation.getByRole('checkbox');
  assert(await enable.isDisabled());assert(await acknowledgement.evaluate(node=>node===document.activeElement));
  await acknowledgement.check();assert(await enable.isEnabled());
  await page.screenshot({path:join(report,name+'-full-access.png')});
  await confirmation.getByRole('button',{name:'Cancel',exact:true}).click();
  assert(await access.evaluate(node=>node===document.activeElement));assert.match(await access.getAttribute('aria-label'),/Workspace Write/);
  await access.click();await page.getByRole('menuitemradio',{name:'Full access',exact:true}).click();assert(!(await acknowledgement.isChecked()));await page.keyboard.press('Escape');
  await access.click();await page.getByRole('menuitemradio',{name:'Read Only',exact:true}).click();
  await page.getByRole('button',{name:'Access mode, current: Read Only',exact:true}).waitFor();
  await access.click();await page.getByRole('menuitemradio',{name:'Full access',exact:true}).click();
  await acknowledgement.check();
  await page.evaluate(async()=>{
    const surface=fixture.view.surfaces.main;
    fixture.savedPermissions=surface.projections;
    surface.projections={...surface.projections,entries:[]};
    await fixture.frame();
  });
  await access.waitFor({state:'detached',timeout:5000});
  assert.equal(await confirmation.count(),0);
  await page.screenshot({path:join(report,name+'-permission-absent.png')});
  await page.evaluate(async()=>{fixture.view.surfaces.main.projections=fixture.savedPermissions;await fixture.frame();});
  await page.getByRole('button',{name:'Access mode, current: Read Only',exact:true}).waitFor();
  await access.click();await page.getByRole('menuitemradio',{name:'Full access',exact:true}).click();
  assert.equal(await acknowledgement.isChecked(),false);
  await page.keyboard.press('Escape');
  await input.fill('permission request');await page.getByTestId('composer-send').click();await page.waitForFunction(()=>fixture.prepared.length===1);
  assert.equal(await page.evaluate(()=>Object.hasOwn(fixture.prepared[0],'sandbox')),false);
  assert.equal(await page.getByRole('button',{name:/^Access mode/}).isDisabled(),true);
  // A pending request disables changes without hiding the selected model or access.
  await page.screenshot({path:join(report,name+'-pending.png')});
  await page.getByRole('button',{name:/^Select model/}).click();await page.screenshot({path:join(report,name+'-model.png')});await page.keyboard.press('Escape');
  for(const width of [390,1440]){await page.setViewportSize({width,height:900});
   const pendingButton=await page.getByTestId('composer-send').evaluate(node=>({width:node.clientWidth,scroll:node.scrollWidth,height:node.clientHeight}));
   assert(pendingButton.width>80&&pendingButton.scroll<=pendingButton.width&&pendingButton.height===34,'Pending action fits one readable line');
   assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
   await page.getByRole('button',{name:/^Select model/}).click();
   const bounds=await page.getByRole('menu',{name:/^Select model/}).boundingBox();assert(bounds.x>=0&&bounds.x+bounds.width<=width);
   await page.screenshot({path:join(report,name+'-'+width+'.png')});await page.keyboard.press('Escape');}
  assert.deepEqual(errors,[]);results.push({browser:name,errors,checks:['starting draft handoff without send','plus commands','reasoning Max','full access confirmation cancel','permission withdrawal removes the control and resets confirmation','ordinary messages follow Host permission','pending access lock','Escape focus','narrow menu bounds']});
 }catch(error){await page.screenshot({path:join(report,name+'-failure.png')}).catch(()=>{});await writeFile(join(report,name+'-failure.txt'),String(error.stack));throw error;}finally{await browser.close();}
}}finally{await scene.close();}
await writeFile(join(report,'document-results.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
