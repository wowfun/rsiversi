import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.base=structuredClone(fixture.view.surfaces.main);fixture.view.surfaces.main=null;
fixture.efforts={supported:['off','low','high','max'],default:'high'};
fixture.view.setup.default_model_description={model:fixture.view.setup.agent.default_model,profile:{reasoning_efforts:fixture.efforts}};
fixture.creates=0;
fixture.setReply('command',async payload=>{
 const command=JSON.parse(payload);
 if(command.action==='initialize_workspace')return '{}';
 if(command.action==='create'){
  fixture.creates++;fixture.view.surfaces.main={...structuredClone(fixture.base),generation:'2',selection:'2',sandbox:'workspace-write',transcript:{blocks:[],status:'Ready'},effort_profile:fixture.efforts};
 }
 await fixture.frame();return '{}';
});
`;
const scene=await sourceFixture({name:'composer-initialization',source}),results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})){
 const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900},colorScheme:'dark'});
 const errors=[];page.on('pageerror',error=>errors.push(error.message));
 try {
  await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  const model=page.getByRole('button',{name:/^Select model/}),composer=page.locator('.composer');
  const initialLabel=await model.getAttribute('aria-label');
  const input=page.locator('.composer textarea');
  await input.fill('Retained across reconnect');
  await page.evaluate(async()=>{
    await fixture.runtime.disconnect();
    await fixture.runtime.attach(fixture.makeTransport(),{async render(){return {}},close(){}},{},async()=>({drafts:await fixture.DraftStore.open('a'.repeat(32),{kind:'local'})}));
    await fixture.frame();
  });
  await page.waitForFunction(()=>!document.querySelector('.composer textarea')?.disabled);
  assert.equal(await input.inputValue(),'Retained across reconnect');
  await input.fill('');
  await page.reload();await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  const reloadedLabel=await model.getAttribute('aria-label');
  await page.evaluate(async()=>{fixture.view.setup.agent.reasoning_effort='off';await fixture.frame();});
  assert.match(await model.getAttribute('aria-label'),/reasoning effort Off/);
  await page.evaluate(async()=>{delete fixture.view.setup.agent.reasoning_effort;fixture.view.setup.default_model_description.model={deployment:'other',model:'unknown'};await fixture.frame();});
  assert.doesNotMatch(await model.getAttribute('aria-label'),/reasoning effort/);
  await page.evaluate(async()=>{fixture.view.setup.default_model_description.model=fixture.view.setup.agent.default_model;await fixture.frame();});
  const before=await composer.boundingBox();
  await model.click();await page.getByRole('menuitem',{name:/^Effort/}).waitFor();
  await page.keyboard.press('Escape');
  const after=await composer.boundingBox();
  await page.screenshot({path:join(report,name+'-home.png')});
  await page.evaluate(async()=>{
   fixture.view.surfaces.main.transcript={status:'Completed',blocks:[
    {key:'u',role:'user',title:'User',text:'say hi',sources:0},
    {key:'r',role:'reasoning',title:'Thinking',text:'Short thought.',sources:0},
    {key:'a',role:'assistant',title:'Assistant',text:'Hi!',sources:0}
   ],turns:{entries:{turn:{id:'turn',status:'Completed',running:false,foldable:true,partial:false,blocks:['u','r','a'],process:['r'],candidate:[],answer:['a'],elapsed_ms:2000}}}};
   await fixture.frame();
  });
  await page.locator('.turn-summary').click();await page.evaluate(()=>fixture.tick());
  const shortBottom=await page.getByRole('button',{name:'Back to bottom',exact:true}).isVisible();
  const shortGeometry=await page.locator('.transcript').evaluate(node=>({height:node.clientHeight,scrollHeight:node.scrollHeight,top:node.scrollTop}));
  await page.screenshot({path:join(report,name+'-short.png')});
  await page.evaluate(async()=>{fixture.view.surfaces.main.transcript.blocks.find(block=>block.key==='a').text='Long reply\n\n'.repeat(100);await fixture.frame();});
  const bottom=page.getByRole('button',{name:'Back to bottom',exact:true});
  assert.equal(await bottom.isVisible(),false,'Tail-following does not show the button');
  await page.locator('.transcript').evaluate(node=>{node.scrollTop=0;});await bottom.waitFor();
  assert.equal(await bottom.innerText(),'');assert.equal(await bottom.locator('svg').count(),1);
  await page.screenshot({path:join(report,name+'-away.png')});
  await bottom.click();await bottom.waitFor({state:'hidden'});
  await page.locator('.transcript').evaluate(node=>{node.scrollTop=0;});await bottom.waitFor();
  await page.evaluate(async()=>{fixture.view.surfaces.main.transcript.blocks.find(block=>block.key==='a').text='Short again';await fixture.frame();});
  await bottom.waitFor({state:'hidden'});
  results.push({browser:name,initialLabel,reloadedLabel,before,after,shortBottom,shortGeometry,scrollReturn:true,contentShrink:true,errors});
 }catch(error){
  await page.screenshot({path:join(report,name+'-failure.png')}).catch(()=>{});
  await writeFile(join(report,name+'-failure.json'),JSON.stringify(await page.evaluate(()=>({text:document.body.innerText,phase:fixture.runtime.connection.phase,panes:[...fixture.runtime.panes].map(([key,pane])=>({key,staged:pane.staged,starting:!!pane.starting,binding:!!pane.startBindingOwner,view:pane.composerView}))})),null,2));
  throw error;
 }finally{await browser.close();}
}}finally{await scene.close();}
await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
for(const result of results){
 assert.match(result.initialLabel,/reasoning effort High/);
 assert.match(result.reloadedLabel,/reasoning effort High/);
 assert.deepEqual(result.before,result.after,'Opening the model menu must not move the composer');
 assert.equal(result.shortBottom,false,'Expanding a short turn must not show Back to bottom');
 assert.deepEqual(result.errors,[]);
}
