import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
import {openSettings} from './controls.mjs';

const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+String.raw`
const row={session:'fixture-session',created_at_ms:'1',last_activity_ms:String(Date.now()),path:'/fixture',location:{kind:'local'},workspace:'fixture',metadata:{title:'UI verification',archived:false,pinned:false}};
fixture.view.navigation={ticket:'nav',metadata_revision:'0',newer_activity:false,filter:{query:'',archived:false,workspace:{kind:'all'}},pins:[],groups:{},entries:[row],more:false,scanned:1,diagnostic:null,attention:{host_epoch:'00000000000000000000000000000001',entries:[{position:{conversation:{kind:'native',id:'fixture-session'},epoch:'0',sequence:'0'},reading:{manual:false,unseen:false,failed:false},status:'running',targets:[]}],truncated:false}};
fixture.view.navigation.groups.fixture={entries:[row],more:false,continued:false,stale:false};
fixture.view.surfaces.main.transcript={status:'Running',blocks:[
 {key:'u',role:'user',title:'User',text:'Inspect this isolated workspace and verify the tools.',sources:0},
 {key:'ctx',role:'context',title:'Runtime context',text:'Active workspace /fixture',sources:1},
 {key:'m',role:'metadata',title:'Model call details',text:'Live call metadata',sources:0},
 {key:'r',role:'reasoning',title:'Reasoning',text:'**Inspect the workspace** before making changes.\n\nRead the fixture and verify its contents.',sources:0},
 {key:'intro',role:'assistant',title:'Assistant',text:'Reading the workspace now.',sources:0},
 {key:'t',role:'tool',title:'file_read · completed',text:'{"path":"README.md"}\n\nUI fixture contents',sources:1,tool:{name:'file_read',argument_summary:'README.md',intent_present:true,phase:{settled:'completed'}}},
 {key:'f',role:'tool',title:'bash · process failed',text:'A controlled command failure',sources:1,tool:{name:'bash',argument_summary:'Inspect fixture status',intent_present:true,phase:{settled:'process_failed'}}}
],turns:{entries:{turn:{id:'turn',status:'Running',running:true,foldable:true,partial:false,blocks:['u','ctx','m','r','intro','t','f'],process:['m','r','t','f'],candidate:[],answer:[],timestamps:{u:Date.now()-2000}}}}};
`;
const server=await sourceFixture({name:'process-transcript',source}),results=[];
async function checkMessageActions(page, touch) {
  await page.evaluate(async()=>{
    const blocks=[], entries={};
    for(const id of ['one','two']) {
      const keys=['user','assistant','status'].map(role=>id+'-'+role);
      blocks.push({key:keys[0],role:'user',title:'User',text:'Question '+id,sources:1},
        {key:keys[1],role:'assistant',title:'Assistant',text:'Answer '+id,sources:1},
        {key:keys[2],role:'status',title:'Completed',text:'',sources:1});
      entries[id]={id,blocks:keys,status:'Completed',running:false,foldable:true,partial:false,
        process:[],candidate:[],answer:[keys[1]],closing_prose:[1],terminal:keys[2]};
    }
    fixture.view.surfaces.main.transcript={blocks,turns:{entries}};
    await fixture.frame();
  });
  const transcript=page.getByRole('region',{name:'Conversation transcript'});
  const users=transcript.locator('.message.user'),tails=transcript.locator('.turn-tail');
  const controls=row=>row.locator(':scope > .message-actions');
  async function opacity(row,value) {
    const element=await controls(row).elementHandle();
    await page.waitForFunction(([node,expected])=>getComputedStyle(node).opacity===expected,[element,value]);
  }
  await tails.last().waitFor();
  assert.equal(await page.evaluate(()=>matchMedia('(hover: hover)').matches),!touch);
  await page.mouse.move(0,0);
  await opacity(users.last(),'1');
  await opacity(tails.last(),'1');
  await opacity(users.first(),touch?'1':'0');
  await opacity(tails.first(),touch?'1':'0');
  assert.equal(await transcript.locator('details:not([open]) .source-actions').count(),0);
  if(touch)return;
  const initial=await controls(users.first()).boundingBox();
  await users.first().hover();
  await opacity(users.first(),'1');
  assert.deepEqual(await controls(users.first()).boundingBox(),initial);
  await page.mouse.move(0,0);
  const options=users.first().getByLabel('Message options',{exact:true});
  await options.focus();
  await opacity(users.first(),'1');
  await options.press('Enter');
  const inspect=users.first().getByRole('button',{name:'Inspect sources'});
  await inspect.waitFor();
  await inspect.focus();
  await inspect.press('Escape');
  await inspect.waitFor({state:'detached'});
  assert(await options.evaluate(node=>node===document.activeElement));
  const fork=tails.first().getByLabel('Fork conversation',{exact:true});
  await fork.focus();
  await opacity(tails.first(),'1');
  await fork.press('Enter');
  const variants=tails.first().getByRole('combobox');
  await variants.selectOption('last');
  await variants.press('Escape');
  await variants.waitFor({state:'detached'});
  await fork.press('Enter');
  assert.equal(await variants.inputValue(),'last');
  await fork.press('Escape');
  await variants.waitFor({state:'detached'});
  await page.getByRole('textbox',{name:'Main message'}).focus();
  await opacity(tails.first(),'0');
}
async function checkReadingPosition(page) {
  await page.evaluate(async()=>{
    const blocks=Array.from({length:32},(_,i)=>({key:'line-'+i,role:'user',title:'User',text:'Reading line '+i,sources:0}));
    fixture.view.surfaces.main.transcript={blocks};
    await fixture.frame();
  });
  const port=page.getByRole('region',{name:'Conversation transcript'});
  const back=page.getByRole('button',{name:'Back to bottom',exact:true});
  await port.getByText('Reading line 31',{exact:true}).waitFor();
  const bounds=await port.boundingBox();
  await port.evaluate(node=>{node.scrollTop=node.scrollHeight;});
  await back.waitFor({state:'hidden'});
  await page.mouse.move(bounds.x+bounds.width/2,bounds.y+bounds.height/2);
  await page.mouse.wheel(0,-40);
  await back.waitFor();
  const geometry=await back.evaluate(node=>[node,node.querySelector('svg')].map(element=>{
    const rect=element.getBoundingClientRect();
    return [rect.width,rect.height];
  }));
  assert.deepEqual(geometry,[[34,34],[16,16]]);
  const placement=await back.evaluate(node=>{
    const button=node.getBoundingClientRect(),column=node.offsetParent.getBoundingClientRect();
    const width=parseFloat(getComputedStyle(node).getPropertyValue('--conversation-width')) || 880;
    return Math.abs(column.right-button.right-Math.max(32,(column.width-width)/2))<1 &&
      Math.abs(column.bottom-button.bottom-16)<1;
  });
  assert(placement,'return button follows the end of the conversation column');
  await back.focus();await back.press('Enter');
  await back.waitFor({state:'hidden'});
  assert(await port.evaluate(node=>document.activeElement===node&&Math.abs(node.scrollHeight-node.clientHeight-node.scrollTop)<=1));
  await page.setViewportSize({width:1920,height:960});
  const handle=page.getByRole('separator',{name:'Resize conversation (right)'});
  await page.waitForFunction(()=>document.querySelector('[aria-label="Resize conversation (right)"]')?.getAttribute('aria-valuenow')==='880');
  const strip=await handle.boundingBox();
  assert.equal(strip.width,10);
  await page.mouse.move(strip.x+strip.width/2,strip.y+strip.height/2);
  await page.mouse.wheel(0,-100);
  await back.waitFor();
  assert(await port.evaluate(node=>node.scrollTop<node.scrollHeight-node.clientHeight-25));
  await back.click();
  await back.waitFor({state:'hidden'});
}
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),page=await browser.newPage({viewport:{width:1567,height:945},colorScheme:'dark'}),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(server.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   const rows=page.locator('.message:is(.reasoning,.tool)'),titles=rows.locator('.summary-toggle');
   await titles.first().waitFor();
   assert.equal(await titles.count(),3);
   await page.getByText('Reading the workspace now.',{exact:true}).waitFor();
   assert.equal(await page.locator('.message.assistant').getByRole('button',{name:'Copy message'}).count(),0);
   assert.equal(await page.locator('[data-turn-tail]').count(),0);
   const injected=page.getByRole('button',{name:'Context injection Runtime context'});
   await injected.waitFor();assert.equal(await injected.getAttribute('aria-expanded'),'false');
   assert.equal(await page.getByText('Active workspace /fixture',{exact:true}).count(),0);
   await injected.focus();await injected.press('Enter');
   await page.getByText('Active workspace /fixture',{exact:true}).waitFor();
   await injected.press('Space');
   assert.equal(await page.getByText('Active workspace /fixture',{exact:true}).count(),0);
   assert.equal(await rows.locator('.message-text:visible').count(),0,'Running process bodies are independently collapsed');
   const collapsedBodies = await rows.locator('.message-text').evaluateAll(nodes=>({
    bodies:nodes.length,textChars:nodes.reduce((sum,node)=>sum+node.textContent.length,0),
   }));
   assert.equal(collapsedBodies.bodies,0,'closed process rows leave full bodies unmounted');
   assert.equal(await page.locator('.message .inline-card').count(),0,'a transcript without card renderers allocates no inline mounts');
   assert.equal(await page.locator('.turn-summary:visible').count(),0);
   assert.equal(await page.locator('.message.metadata').count(),0,'Running Chat keeps model metadata in Trajectory');
   await page.getByRole('status').filter({hasText:'Working…'}).waitFor();
   await page.getByTestId('conversation-row').getByRole('img',{name:'Running',exact:true}).waitFor();
   const measurements=await titles.evaluateAll(nodes=>nodes.map(node=>({height:node.getBoundingClientRect().height,text:node.textContent})));
   assert(measurements.every(row=>row.height===24),JSON.stringify(measurements));
   assert.match(measurements[1].text,/ReadREADME.md/);assert.match(measurements[2].text,/Failed/);
   await page.screenshot({path:join(report,name+'-running.png')});
   await titles.first().press('Enter');
   await rows.first().locator('.message-text').waitFor({state:'visible'});
   assert.match(await rows.first().locator('.message-text').innerText(),/verify its contents/);
   const nodeRetained=await page.evaluate(async()=>{
    const node=document.querySelector('.message.reasoning');
    fixture.view.surfaces.main.transcript.blocks.find(block=>block.key==='r').text='Long streamed reasoning preview. '.repeat(100)+'\nbody';await fixture.frame();await fixture.tick();
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
    Object.assign(transcript.turns.entries.turn,{status:'Completed',running:false,foldable:true,blocks:['u','ctx','m','r','intro','t','f','a','end'],answer:['a'],closing_prose:[7],terminal:'end',elapsed_ms:2000});
    transcript.status='Completed';fixture.view.navigation.attention.entries=[];await fixture.frame();
   });
   await page.locator('.turn-running').waitFor({state:'detached'});
   const tail=page.locator('[data-turn-tail="turn"]');
   await tail.getByRole('button',{name:'Copy message'}).waitFor();
   assert.equal(await page.locator('.message.assistant').getByRole('button',{name:'Copy message'}).count(),0);
   assert.equal(await tail.getByLabel('Fork conversation').count(),1);
   assert.equal(await rows.first().isVisible(),false);
   await page.locator('.turn-summary').click();await titles.first().waitFor();
   assert.equal(await rows.locator('.message-text:visible').count(),0,'Turn expansion reveals rows, not all bodies');
   assert.equal(await page.locator('.message.metadata').count(),0,'Completed Chat keeps model metadata in Trajectory');
   await page.locator('.turn-summary').hover();
   assert.equal(await page.locator('.turn-summary').evaluate(node=>getComputedStyle(node).backgroundColor),'rgba(0, 0, 0, 0)');
   await titles.first().hover();
   assert.equal(await titles.first().evaluate(node=>getComputedStyle(node).backgroundColor),'rgba(0, 0, 0, 0)');
   await page.getByRole('tab',{name:'Trajectory',exact:true}).click();
   const ledger=page.getByRole('table',{name:'Trajectory records'});
   await ledger.getByText('Live call metadata',{exact:true}).waitFor();
   const search=page.getByRole('searchbox',{name:'Search trajectory'});
   await search.fill('README FILE_READ');
   assert.equal(await ledger.locator('tr[data-trajectory-row-key]').count(),1);
   const timeline=page.getByRole('region',{name:'Trajectory timeline',exact:true});
   assert.deepEqual(await timeline.locator('[aria-hidden=true] > span').allTextContents().then(items=>items.filter(Boolean).slice(0,3)),['Input','Model','Tools']);
   await timeline.locator('[data-timeline-span="tool"][data-search-match="true"]').click();
   const details=page.getByRole('complementary',{name:'Event details'});
   await details.getByText(/UI fixture contents/).waitFor();
   await page.screenshot({path:join(report,name+'-trajectory-search.png')});
   await page.setViewportSize({width:1440,height:960});
   assert.equal(await page.getByRole('separator',{name:/Resize conversation/}).count(),0);
   await details.getByRole('button',{name:'Inspect sources'}).click({trial:true});
   await page.getByRole('tab',{name:'Chat',exact:true}).click();
   await page.getByRole('tab',{name:'Trajectory',exact:true}).click();
   assert.equal(await search.inputValue(),'README FILE_READ');
   await details.getByText(/UI fixture contents/).waitFor();
   await details.getByRole('button',{name:'Close details'}).click();
   await search.fill('no such record in fixture');
   await page.getByRole('status').filter({hasText:'No matching records'}).waitFor();
   await search.fill('');
   await page.getByRole('button',{name:'Collapse turns'}).click();
   assert.equal(await ledger.locator('tr[data-trajectory-row-key]').count(),0);
   await page.getByRole('button',{name:'Expand turns'}).click();
   await ledger.getByText('Live call metadata',{exact:true}).waitFor();
   const track=timeline.getByLabel(/Timeline overview/), bounds=await track.boundingBox();
   const y=bounds.y+bounds.height-2;
   await page.mouse.move(bounds.x+2,y);
   await page.mouse.down();
   await page.mouse.move(bounds.x+bounds.width*.28,y,{steps:8});
   await page.mouse.up();
   const narrowed=await ledger.locator('tr[data-trajectory-row-key]').count();
   assert(narrowed>0 && narrowed<5);
   await track.focus();
   await page.keyboard.press('Escape');
   assert((await ledger.locator('tr[data-trajectory-row-key]').count())>narrowed);
   await page.getByRole('tab',{name:'Chat',exact:true}).click();
   await page.evaluate(async()=>{Object.assign(fixture.view.surfaces.main.transcript.turns.entries.turn,{status:'Failed',foldable:false});await fixture.frame();});
   assert.equal(await page.locator('.message.metadata').isVisible(),true,'Failed history retains diagnostic model metadata');
   await page.evaluate(async()=>{fixture.view.surfaces.main.history_more=true;await fixture.frame();});
   const earlier=page.getByRole('region',{name:'Conversation transcript'}).getByRole('button',{name:'Load earlier'});
   await earlier.focus();await earlier.press('Enter');
   await page.waitForFunction(()=>fixture.calls.some(call=>call.method==='command'&&JSON.parse(call.payload).action==='history'));
   await checkMessageActions(page,false);
   await page.screenshot({path:join(report,name+'-action-rows.png')});
   await checkReadingPosition(page);
   const touchPage=await browser.newPage({hasTouch:true,viewport:{width:390,height:844}});
   try {
     await touchPage.goto(server.url);await touchPage.waitForFunction(()=>window.ready);
     await checkMessageActions(touchPage,true);
   } finally {await touchPage.close();}
   assert.deepEqual(errors,[]);results.push({browser:name,measurements,collapsedBodies,nodeRetained,more:true,reducedMotion:true,narrow:true,settlement:true,actionRows:{mouse:true,keyboard:true,touch:true},reading:{wheel:true,keyboardReturn:true}});
  } finally {await browser.close();}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await server.close();}
