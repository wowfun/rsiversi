import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {openSettings} from './controls.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+String.raw`
const row={session:'fixture-session',created_at_ms:'1',last_activity_ms:String(Date.now()),path:'/fixture',location:{kind:'local'},workspace:'fixture',metadata:{title:'UI verification',archived:false,pinned:false}};
fixture.view.navigation={ticket:'nav',metadata_revision:'0',newer_activity:false,filter:{query:'',archived:false,workspace:{kind:'all'}},pins:[],groups:{},entries:[row],more:false,scanned:1,diagnostic:null,attention:{entries:[{position:{conversation:{kind:'native',id:'fixture-session'}},status:'running',targets:[]}],truncated:false}};
fixture.view.navigation.groups.fixture={entries:[row],more:false,continued:false,stale:false};
fixture.view.surfaces.main.transcript={status:'Running',blocks:[
 {key:'u',role:'user',title:'User',text:'Inspect this isolated workspace and verify the tools.',sources:0},
 {key:'m',role:'metadata',title:'Model call details',text:'Live call metadata',sources:0},
 {key:'r',role:'reasoning',title:'Reasoning',text:'**Inspect the workspace** before making changes.\n\nRead the fixture and verify its contents.',sources:0},
 {key:'t',role:'tool',title:'file_read · completed',text:'{"path":"README.md"}\n\nUI fixture contents',sources:1,tool:{name:'file_read',argument_summary:'README.md',intent_present:true,phase:{settled:'completed'}}},
 {key:'f',role:'tool',title:'bash · process failed',text:'A controlled command failure',sources:1,tool:{name:'bash',argument_summary:'Inspect fixture status',intent_present:true,phase:{settled:'process_failed'}}}
],turns:{entries:{turn:{id:'turn',status:'Running',running:true,foldable:false,partial:false,blocks:['u','m','r','t','f'],process:['m','r','t','f'],candidate:[],answer:[],timestamps:{u:Date.now()-2000}}}}};
`;
const server=await sourceFixture({name:'process-transcript',source}),results=[];
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1567,height:945},colorScheme:'dark'}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(server.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   const rows=page.locator('.message:is(.reasoning,.tool)'),titles=rows.locator('.summary-toggle');
   await titles.first().waitFor();
   assert.equal(await titles.count(),3);
   assert.equal(await rows.locator('.message-text:visible').count(),0,'Running process bodies are independently collapsed');
   assert.equal(await page.locator('.turn-summary:visible').count(),0);
   assert.equal(await page.locator('.message.metadata').isVisible(),false,'Running Chat keeps model metadata in Trajectory');
   await page.getByRole('status').filter({hasText:'Working…'}).waitFor();
   await page.getByTestId('conversation-row').getByRole('img',{name:'Running',exact:true}).waitFor();
   const measurements=await titles.evaluateAll(nodes=>nodes.map(node=>({height:node.getBoundingClientRect().height,text:node.textContent})));
   assert(measurements.every(row=>row.height===24),JSON.stringify(measurements));
   assert.match(measurements[1].text,/ReadREADME.md/);assert.match(measurements[2].text,/Failed/);
   await page.screenshot({path:join(report,name+'-running.png')});
   await titles.first().press('Enter');
   await rows.first().locator('.message-text').waitFor({state:'visible'});
   const nodeRetained=await page.evaluate(async()=>{
    const node=document.querySelector('.message.reasoning');
    fixture.view.surfaces.main.transcript.blocks.find(block=>block.key==='r').text='Long streamed reasoning preview. '.repeat(100);await fixture.frame();await fixture.tick();
    return node===document.querySelector('.message.reasoning');
   });
   assert(nodeRetained);assert.equal(await titles.first().getAttribute('aria-expanded'),'true');
   await titles.first().press('Enter');assert.equal(await rows.first().locator('.message-text').isVisible(),false);
   const more=page.getByLabel('More',{exact:true});await more.press('Enter');
   await page.getByRole('button',{name:'Settings',exact:true}).waitFor();
   await page.screenshot({path:join(report,name+'-more.png')});
   await page.keyboard.press('Escape');assert(await more.evaluate(node=>node===document.activeElement));
   await openSettings(page);await page.getByRole('dialog',{name:'Settings',exact:true}).waitFor();
   await page.getByRole('button',{name:'Close settings',exact:true}).click();
   await page.emulateMedia({reducedMotion:'reduce'});
   assert.equal(await page.locator('.session-state.running').evaluate(node=>getComputedStyle(node).animationName),'none');
   await page.setViewportSize({width:390,height:844});
   assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
   assert((await titles.evaluateAll(nodes=>nodes.map(node=>node.getBoundingClientRect().height))).every(height=>height===24));
   assert(await titles.first().locator('.process-label').evaluate(node=>node.clientWidth>=node.scrollWidth),'Long previews must not squeeze the Think label');
   await page.screenshot({path:join(report,name+'-narrow.png')});
   await page.evaluate(async()=>{
    const transcript=fixture.view.surfaces.main.transcript;
    transcript.blocks.push({key:'a',role:'assistant',title:'Assistant',text:'UI verification complete.',sources:0},{key:'end',role:'status',title:'Completed',text:'',sources:1});
    Object.assign(transcript.turns.entries.turn,{status:'Completed',running:false,foldable:true,blocks:['u','m','r','t','f','a','end'],answer:['a'],terminal:'end',elapsed_ms:2000});
    transcript.status='Completed';fixture.view.navigation.attention.entries=[];await fixture.frame();
   });
   await page.locator('.turn-running').waitFor({state:'detached'});
   assert.equal(await rows.first().isVisible(),false);
   await page.locator('.turn-summary').click();await titles.first().waitFor();
   assert.equal(await rows.locator('.message-text:visible').count(),0,'Turn expansion reveals rows, not all bodies');
   assert.equal(await page.locator('.message.metadata').isVisible(),false,'Completed Chat keeps model metadata in Trajectory');
   await page.locator('.turn-summary').hover();
   assert.equal(await page.locator('.turn-summary').evaluate(node=>getComputedStyle(node).backgroundColor),'rgba(0, 0, 0, 0)');
   await titles.first().hover();
   assert.equal(await titles.first().evaluate(node=>getComputedStyle(node).backgroundColor),'rgba(0, 0, 0, 0)');
   await page.getByRole('tab',{name:'Trajectory',exact:true}).click();
   assert.equal(await page.locator('.message.metadata').isVisible(),true);
   assert.equal(await rows.locator('.message-text:visible').count(),3);
   await page.getByRole('tab',{name:'Chat',exact:true}).click();
   await page.evaluate(async()=>{Object.assign(fixture.view.surfaces.main.transcript.turns.entries.turn,{status:'Failed',foldable:false});await fixture.frame();});
   assert.equal(await page.locator('.message.metadata').isVisible(),true,'Failed history retains diagnostic model metadata');
   assert.deepEqual(errors,[]);results.push({browser:name,measurements,nodeRetained,more:true,reducedMotion:true,narrow:true,settlement:true});
  } finally {await browser.close();}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await server.close();}
