import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR??'.local/notes/1027/review-remediation-1029/composer');
await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.base=structuredClone(fixture.view.surfaces.main);
fixture.view.surfaces.main=null;fixture.view.setup.agent.sandbox='workspace-write';
fixture.createEntered=false;fixture.releaseCreate=()=>{};
fixture.setReply('command',async payload=>{
 const command=JSON.parse(payload);
 if(command.action==='create') {
  fixture.createEntered=true;await new Promise(resolve=>fixture.releaseCreate=resolve);
  fixture.view.surfaces.main={...structuredClone(fixture.base),generation:'2',selection:'2',sandbox:'workspace-write',path:'C:/projects/review-workspace/',transcript:{blocks:[],status:'Ready'}};
 }
 await fixture.frame();return '{}';
});
`;
const scene=await sourceFixture({name:'review-composer',source});const results=[];
try {for(const [name,engine] of Object.entries({chromium,firefox})) {
 const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1440,height:900},colorScheme:'dark'}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 try {
  await page.goto(scene.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
  await page.getByRole('textbox',{name:'Main message',exact:true}).fill('Keep the starting draft');
  await page.getByRole('button',{name:/^Access mode/}).click();await page.waitForFunction(()=>fixture.createEntered);
  assert.equal(await page.getByRole('menuitemradio',{name:'Read Only',exact:true}).isDisabled(),true,'Access choices must wait for the draft editor');
  await page.screenshot({path:join(report,name+'-preparing.png')});
  await page.evaluate(()=>fixture.releaseCreate());
  await page.waitForFunction(()=>!document.querySelector('[role="menuitemradio"][aria-checked="false"]')?.disabled);
  await page.getByRole('menuitemradio',{name:'Read Only',exact:true}).click();
  await page.getByRole('button',{name:'Access mode, current: Read Only',exact:true}).waitFor();
  assert.equal(await page.getByRole('textbox',{name:'Main message',exact:true}).inputValue(),'Keep the starting draft');
  await page.getByRole('button',{name:'review-workspace',exact:true}).waitFor();
  await page.evaluate(async()=>{fixture.view.catalog.workspaces[0].name='Named workspace';await fixture.frame();});
  await page.getByRole('region',{name:'Conversations',exact:true}).getByRole('button',{name:'Named workspace',exact:true}).waitFor();
  await page.screenshot({path:join(report,name+'-ready.png')});
  await page.evaluate(async()=>{fixture.view.surfaces.main=null;fixture.view.setup.agent.default_model=null;await fixture.frame();});
  await page.getByRole('button',{name:/^Access mode/}).click();
  assert.equal(await page.getByRole('menuitemradio',{name:'Read Only',exact:true,includeHidden:true}).isDisabled(),true,'Unavailable preparation must not enable permission choices');
  assert.deepEqual(errors,[]);results.push({browser:name,checks:['preparation gates permission changes','unavailable preparation remains disabled','draft retained','access choice applied','trailing-separator conversation title','custom workspace name'],errors});
 } catch(error){await page.screenshot({path:join(report,name+'-failure.png')});throw error;}
 finally{await browser.close();}
}}finally{await scene.close();}
await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));console.log(JSON.stringify(results));
