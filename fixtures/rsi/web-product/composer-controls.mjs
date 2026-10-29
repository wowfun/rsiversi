import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR??'.local/notes/1027/composer-controls');await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.base=structuredClone(fixture.view.surfaces.main);fixture.view.surfaces.main=null;
fixture.creates=0;fixture.prepared=[];fixture.view.has_media=true;
fixture.view.catalog.models.push({deployment:'fixture',model:'other'});
fixture.setReply('command',async payload=>{
 const command=JSON.parse(payload);
 if(command.action==='create'){
  fixture.creates++;fixture.view.surfaces.main={...structuredClone(fixture.base),generation:'2',selection:'2',sandbox:'workspace-write',transcript:{blocks:[],status:'Ready'},effort_profile:{supported:['off','low','high','max'],default:'high'},commands:{revision:{kind:'draft'},commands:[{name:'plan',description:'Plan mode',draft_safe:true}]}};
 }
 const data=fixture.view.surfaces.main;
 if(command.action==='model'){data.model=command.model;data.reasoning_effort=command.reasoning_effort;}
 await fixture.frame();return '{}';
});
fixture.setReply('prepare_submission',payload=>{const input=JSON.parse(payload);fixture.prepared.push(input);return JSON.stringify({kind:'message',id:'request-'+fixture.prepared.length,opaque:'opaque',text_bytes:input.text.length,images:0,references:0});});
fixture.setReply('dispatch_submission',()=>JSON.stringify({status:'not_admitted',error:'fixture retained request'}));
`;
const scene=await sourceFixture({name:'composer-controls',source});const results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})){
 const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900},colorScheme:'dark'}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 try{
  await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  const input=page.getByRole('textbox',{name:'Main message',exact:true});await input.fill('Draft survives opening controls');
  await page.getByRole('button',{name:'Add files or run commands'}).click();await page.getByRole('menuitem',{name:'File',exact:true}).waitFor();
  await page.waitForFunction(()=>fixture.creates===1&&document.querySelector('[role=menu] button:not(:disabled)'));
  assert.equal(await input.inputValue(),'Draft survives opening controls');assert.equal(await page.evaluate(()=>fixture.prepared.length),0);
  await page.screenshot({path:join(report,name+'-plus.png')});
  await page.getByRole('menuitem',{name:'Commands',exact:true}).click();await page.getByRole('menuitem',{name:'/plan',exact:true}).click();assert.equal(await input.inputValue(),'/plan ');
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.getByRole('menuitemradio',{name:'Max',exact:true}).click();
  await page.getByRole('button',{name:/reasoning effort Max/}).waitFor();
  await page.getByRole('button',{name:/^Select model/}).click();await page.getByRole('menuitem',{name:/^Effort/}).click();await page.screenshot({path:join(report,name+'-effort.png')});
  await page.keyboard.press('Escape');assert.equal(await page.getByRole('button',{name:/^Select model/}).evaluate(node=>node===document.activeElement),true);
  await page.getByRole('button',{name:/^Access mode/}).click();await page.getByRole('menuitemradio',{name:'Full access',exact:true}).click();await page.getByText('Allow full access?',{exact:true}).waitFor();
  await page.getByRole('menuitem',{name:'Cancel',exact:true}).click();await page.getByRole('menuitemradio',{name:'Read Only',exact:true}).click();
  await page.getByRole('button',{name:'Access mode, current: Read Only',exact:true}).waitFor();
  await input.fill('permission request');await page.getByTestId('composer-send').click();await page.waitForFunction(()=>fixture.prepared.length===1);
  assert.equal(await page.evaluate(()=>fixture.prepared[0].sandbox),'read-only');
  assert.equal(await page.getByRole('button',{name:/^Access mode/}).isDisabled(),true);
  // A pending request disables changes without hiding the selected model or access.
  await page.screenshot({path:join(report,name+'-pending.png')});
  await page.getByRole('button',{name:/^Select model/}).click();await page.screenshot({path:join(report,name+'-model.png')});await page.keyboard.press('Escape');
  for(const width of [390,1440]){await page.setViewportSize({width,height:900});await page.getByRole('button',{name:/^Select model/}).click();
   const bounds=await page.getByRole('menu',{name:/^Select model/}).boundingBox();assert(bounds.x>=0&&bounds.x+bounds.width<=width);
   await page.screenshot({path:join(report,name+'-'+width+'.png')});await page.keyboard.press('Escape');}
  assert.deepEqual(errors,[]);results.push({browser:name,errors,checks:['starting draft handoff without send','plus commands','reasoning Max','full access confirmation cancel','sandbox frozen into request','pending access lock','Escape focus','narrow menu bounds']});
 }catch(error){await page.screenshot({path:join(report,name+'-failure.png')});throw error;}finally{await browser.close();}
}}finally{await scene.close();}
await writeFile(join(report,'document-results.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));

