import assert from 'node:assert/strict';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {chromium,firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';
const report=resolve(process.env.RSI_REPORT_DIR);await mkdir(report,{recursive:true});
const source=await readFile(new URL('./document-scene.tsx',import.meta.url),'utf8')+`
fixture.view.surfaces.main=null;
fixture.view.setup.agent.default_model=null;
fixture.view.catalog.models=[];
fixture.view.catalog.workspaces[0].coordinates.path='/Documents/RSIversi/default-workspace';
fixture.setReply('command', async text=>{const command=JSON.parse(text);if(command.action==='create')await fixture.surface('created',{workspace:command.workspace,transcript:{blocks:[],status:'Ready'}});return '{}';});
fixture.setReply('prepare_submission',text=>JSON.stringify({kind:'message',id:'first-message',opaque:text,text_bytes:new TextEncoder().encode(JSON.parse(text).text).length,images:0,references:0}));
fixture.setReply('dispatch_submission',()=>JSON.stringify({status:'complete',receipt:'{}'}));
`;
const server=await sourceFixture({name:'starting-transcript',source}),results=[];
try {
 for(const [name,engine] of Object.entries({chromium,firefox})) {
  const browser=await engine.launch(),context=await browser.newContext({viewport:{width:1710,height:940},colorScheme:'dark'}),page=await context.newPage(),errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  try {
   await page.goto(server.url);await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   // An open resource preference belongs to a conversation, not the starting page.
   await page.evaluate(()=>fixture.surface('resource-preference'));
   await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
   await page.locator('aside.resources').waitFor({state:'visible'});
   await page.evaluate(async()=>{fixture.view.surfaces.main=null;await fixture.frame();});
   await page.locator('aside.resources').waitFor({state:'hidden'});
   assert.equal(await page.getByRole('button',{name:'Toggle resources',exact:true}).count(),0);
   await page.reload();await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   await page.locator('aside.resources').waitFor({state:'hidden'});
   await page.evaluate(()=>fixture.surface('resource-preference'));
   await page.locator('aside.resources').waitFor({state:'visible'});
   await page.getByRole('button',{name:'Toggle resources',exact:true}).click();
   await page.evaluate(async()=>{fixture.view.surfaces.main=null;await fixture.frame();});
   const input=page.getByRole('textbox',{name:'Main message',exact:true});
   await page.getByTestId('new-conversation').click();
   assert(await input.isEnabled());
   await input.fill('A saved starting message 世界');await page.evaluate(()=>fixture.runtime.panes.get('main').flush());
   assert.equal(await page.evaluate(()=>fixture.calls.some(call=>call.method==='command'&&JSON.parse(call.payload).action==='create')),false);
   await page.reload();await page.waitForFunction(()=>window.ready);await page.evaluate(()=>fixture.frame());
   await page.waitForFunction(()=>document.querySelector('[aria-label="Main message"]').value==='A saved starting message 世界');
   await page.getByRole('button',{name:'Choose model',exact:true}).click();
   assert.equal(await input.inputValue(),'A saved starting message 世界');
   await page.evaluate(()=>fixture.input.dismissSettings());
   await page.screenshot({path:join(report,name+'-starting.png')});
   await page.evaluate(async()=>{fixture.view.setup.agent.default_model={deployment:'fixture',model:'deterministic'};await fixture.frame();});
   await page.getByTestId('composer-send').click();
   await page.waitForFunction(()=>fixture.calls.some(call=>call.method==='dispatch_submission'));
   const submitted=await page.evaluate(()=>JSON.parse(fixture.calls.find(call=>call.method==='prepare_submission').payload).text);
   assert.equal(submitted,'A saved starting message 世界');
   assert.equal(await page.evaluate(()=>fixture.runtime.panes.get('main').starting.text),'');
   await page.evaluate(async()=>{
    fixture.view.surfaces.main.transcript={status:'Completed',blocks:[
      {key:'u',role:'user',title:'User',text:'say hi',sources:0},
      {key:'r',role:'reasoning',title:'Thinking',text:'A short reasoning step.',sources:0},
      {key:'a',role:'assistant',title:'Assistant',text:'Hi! Ready when you are — just let me know what you would like to work on.',sources:0}, {key:'terminal',role:'status',title:'Completed',text:'',sources:1}
    ],turns:{entries:{turn:{id:'turn',status:'Completed',running:false,foldable:true,partial:false,blocks:['u','r','a','terminal'],terminal:'terminal',process:['r'],candidate:[],answer:['a'],elapsed_ms:2000,timestamps:{u:Date.now()-2000,a:Date.now()},usage:{input_tokens:10000,output_tokens:1300,cache_read_tokens:8700,cache_write_tokens:null,reasoning_tokens:null}}}}};
    await fixture.frame();
   });
   const composer=page.locator('.pane:not([hidden]) > .composer');
   const beforeMetrics=await composer.boundingBox();
   await page.screenshot({path:join(report,name+'-before-metrics.png')});
   await page.evaluate(async()=>{
    fixture.view.surfaces.main.metrics={complete:true,watermark:4,summary:{through_seq:4,attempts:1,reported_attempts:1,tokens:{input_tokens:10000,output_tokens:1300,cache_read_tokens:8700,cache_write_tokens:null,reasoning_tokens:null},timing:{turns:1,timed_turns:1,total_ms:3000,timed_attempts:1,model_ms:2000,first_token_attempts:1,first_token_ms:1000,decode_ms:1000,decode_output_tokens:1300},last_context:{input_tokens:10000}}};
    fixture.view.surfaces.main.context_capacity_tokens=100000;
    await fixture.frame();
   });
   await page.getByTestId('session-metrics').waitFor();
   const afterMetrics=await composer.boundingBox();
   await page.screenshot({path:join(report,name+'-after-metrics.png')});
   await writeFile(join(report,name+'-metrics-arrival.json'),JSON.stringify({beforeMetrics,afterMetrics},null,2));
   assert(Math.abs(beforeMetrics.y-afterMetrics.y)<1,`Metrics moved composer by ${afterMetrics.y-beforeMetrics.y}px`);
   const metricsGeometry=[{width:1710,state:'arrival',before:beforeMetrics,after:afterMetrics}];
   await page.getByRole('tab',{name:'Chat',exact:true}).waitFor();
   assert.equal(await page.locator('.message.reasoning').isVisible(),false);
   assert.equal(await page.locator('.message.status').isVisible(),false);
   await page.getByRole('tab',{name:'Chat',exact:true}).press('ArrowRight');
   assert.equal(await page.getByRole('tab',{name:'Trajectory',exact:true}).getAttribute('aria-selected'),'true');
   await page.locator('.message.reasoning').waitFor({state:'visible'});
   await page.locator('.message.status').waitFor({state:'visible'});
   await page.getByRole('tab',{name:'Trajectory',exact:true}).press('ArrowLeft');
   await page.getByText('Completed in 2s', {exact:false}).waitFor();
   assert.equal(await page.locator('.message-clock').count(),2);
   await page.locator('.message-usage button').click();
   assert.equal(await page.getByRole('dialog',{name:'Token usage',exact:true}).isVisible(),true);
   await page.locator('.message-usage button').click();
   await page.getByTestId('session-metrics').getByText('11.3K tok · Cache hit 87%',{exact:true}).waitFor();
   const stats=page.getByTestId('session-metrics');
   for(const [button,title] of [[/1 turn 1 step/, 'Session statistics'],[/11.3K tok/, 'Token usage'],[/10%/, 'Context usage']]) {
     await stats.getByRole('button',{name:button}).click();
     const panel=page.getByRole('dialog',{name:title,exact:true});await panel.waitFor();
     assert.equal(await page.getByRole('dialog').count(),1);
     const box=await panel.boundingBox();assert(box.x>=12 && box.x+box.width<=1710-12 && box.y>=12);
     if(title==='Session statistics') {
       await panel.getByText('Avg time to first token (TTFT)',{exact:true}).waitFor();
       assert.match(await panel.innerText(),/1s/);
     }
     if(title==='Token usage') assert.match(await panel.innerText(),/Uncached input[\s\S]*1,300 tok/);
     if(title==='Context usage') assert.equal(await panel.getByRole('progressbar').getAttribute('value'),'10000');
     await page.screenshot({path:join(report,name+'-'+title.replaceAll(' ','-')+'.png')});
     await page.keyboard.press('Escape');await panel.waitFor({state:'detached'});
     assert(await stats.getByRole('button',{name:button}).evaluate(node=>node===document.activeElement));
   }
   await stats.getByRole('button',{name:/1 turn 1 step/}).click();
   await stats.getByRole('button',{name:/11.3K tok/}).click();
   assert.equal(await page.getByRole('dialog').count(),1);
   await page.locator('.message.user .message-text').click();
   await page.getByRole('dialog').waitFor({state:'detached'});

   const geometry=await page.evaluate(()=>{const bubble=document.querySelector('.message.user > .message-text').getBoundingClientRect(),answer=document.querySelector('.message.assistant').getBoundingClientRect();return {rightAligned:bubble.left>answer.left,alignedRight:Math.abs(bubble.right-answer.right)<2};});
   assert.deepEqual(geometry,{rightAligned:true,alignedRight:true});
   assert(await page.locator(".composer").evaluate(node=>node.getBoundingClientRect().bottom>innerHeight-60));
   for(const theme of ['dark','light']) {await page.emulateMedia({colorScheme:theme});await page.evaluate(()=>fixture.tick());await page.locator(".message.assistant").click();await page.screenshot({path:join(report,name+'-'+theme+'-transcript.png')});}
   for(const width of [1710,390,320]) {
    await page.setViewportSize({width,height:844});
    await page.evaluate(async()=>{
     fixture.completeMetrics=structuredClone(fixture.view.surfaces.main.metrics);
     fixture.view.surfaces.main.metrics=null;
     await fixture.frame();await fixture.tick();
    });
    assert.equal(await stats.count(),0,'Pending metrics must not fabricate readings');
    const baseline=await composer.boundingBox();
    const stable=async state=>{
     await page.evaluate(()=>fixture.tick());
     const box=await composer.boundingBox();
     metricsGeometry.push({width,state,before:baseline,after:box});
     for(const dimension of ['y','height']) assert(Math.abs(baseline[dimension]-box[dimension])<1,`${width}px ${state} changed composer ${dimension}`);
     assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
     const buttons=await stats.getByRole('button').evaluateAll(nodes=>nodes.map(node=>node.getBoundingClientRect().toJSON()));
     assert(buttons.every(button=>Math.abs(button.y-buttons[0].y)<1),'Statistics must stay on one line');
    };
    await page.evaluate(async()=>{fixture.view.surfaces.main.metrics=structuredClone(fixture.completeMetrics);await fixture.frame();});
    await stats.waitFor();await stable('complete');
    assert(await stats.getByRole('button',{name:'10%',exact:true}).evaluate(node=>node.lastElementChild.scrollWidth<=node.lastElementChild.clientWidth),'Context percentage must remain fully visible');
    await page.evaluate(async()=>{fixture.view.surfaces.main.metrics.complete=false;await fixture.frame();});
    await stats.getByText('Partial statistics',{exact:true}).waitFor();await stable('partial');
    await stats.getByRole('button',{name:/1 turn 1 step/}).click();
    await page.evaluate(()=>fixture.setReply('command',()=>{throw new Error('Fixture statistics unavailable');}));
    const statisticsPanel=page.getByRole('dialog',{name:'Session statistics',exact:true});
    await statisticsPanel.getByRole('button',{name:'Refresh statistics',exact:true}).click();
    await statisticsPanel.getByRole('status').getByText('Fixture statistics unavailable',{exact:false}).waitFor();
    await stable('failure');
    await page.evaluate(async()=>{fixture.view.surfaces.main.metrics=null;await fixture.frame();});
    await stable('failure-without-readings');
    await page.evaluate(()=>fixture.setReply('command',async()=>{
     fixture.view.surfaces.main.metrics=structuredClone(fixture.completeMetrics);await fixture.frame();return '{}';
    }));
    await statisticsPanel.getByRole('button',{name:'Refresh statistics',exact:true}).click();
    await stats.getByRole('button',{name:/1 turn 1 step/}).waitFor();await stable('retry');
    await page.keyboard.press('Escape');await statisticsPanel.waitFor({state:'detached'});
    await page.screenshot({path:join(report,`${name}-metrics-stable-${width}.png`)});
   }
   await page.setViewportSize({width:390,height:844});await page.screenshot({path:join(report,name+'-narrow.png')});
   assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth));
   await stats.getByRole('button',{name:/10%/}).click();
   const contextPanel=page.getByRole('dialog',{name:'Context usage',exact:true});
   const contextBox=await contextPanel.boundingBox();assert(contextBox.x>=12 && contextBox.x+contextBox.width<=378);
   await page.screenshot({path:join(report,name+'-context-narrow.png')});
   await page.setViewportSize({width:320,height:600});
   await page.waitForFunction(()=>{const panel=document.querySelector('[role=dialog][aria-label="Context usage"]');const box=panel.getBoundingClientRect();return box.x>=12&&box.right<=innerWidth-12;});
   await page.keyboard.press('Escape');
   const storage=await page.evaluate(async()=>{
    const {StartingDraft}=await import('/starting-draft.js');
    const a=await StartingDraft.load(['isolated','local'],'main'), b=await StartingDraft.load(['isolated','local'],'main');
    a.edit('first');await a.flush();b.edit('second');let conflict=false;try{await b.flush()}catch{conflict=true}
    await b.resolve(true);const saved=b.text;const old=b.serial;b.edit('newer');await b.clearCaptured(old);await b.flush();
    const other=await StartingDraft.load(['other','local'],'main');return {conflict,saved,newer:b.text,other:other.text};
   });
   assert.deepEqual(storage,{conflict:true,saved:'first',newer:'newer',other:''});
   assert.deepEqual(errors,[]);results.push({browser:name,modelFreeDraft:true,reload:true,firstSend:true,tabKeyboard:true,messageAlignment:true,conflict:true,narrow:true,metricsGeometry});
  } finally {await browser.close()}
 }
 await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally {await server.close()}
