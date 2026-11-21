import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report = resolve(process.env.RSI_REPORT_DIR);
await mkdir(report, {recursive:true});
const fixture = await sourceFixture({name:'schedule-view', source:`
import React,{useState} from 'react';
import {createRoot} from 'react-dom/client';
import {source,installActions,useView} from '/src/bridge.ts';
import {ScheduledTasks} from '/src/session/scheduled-tasks.tsx';
import {Transcript} from '/src/session/transcript.tsx';
import {ResourceDocks} from '/src/resource-dock.tsx';
import {IconSprite} from '/src/icon-sprite.tsx';
import {ResourceOwner} from '/src/resource-owner.mjs';
import {presentationIdentity,LayoutContext,useViewport} from '/src/presentation.tsx';
import {defaultLayout} from '/presentation-layout.js';
import {installDisclosures} from '/disclosure.js';
import scope from '/src/session/chat.module.css';
import '/styles.css';
const time = Date.UTC(2026,10,20,9,30);
const tasks = ['Review open pull requests','Prepare the weekly summary'].map((prompt,index)=>({
  id:'reminder-' + index, prompt, active:true, consumed:false, due_ms:time-index*3600_000, interval_ms:index?3600_000:null,
}));
const blocks = [
  {key:'request',role:'status',trigger:'schedule',title:'Automation task',text:"Review today's two tasks.",sources:1},
  ...tasks.map((task,index)=>({key:task.id,role:'tool',title:'schedule_create',text:'',
    created_schedule:{id:task.id,title:'Original '+index,interval_ms:task.interval_ms},
    tool:{name:'schedule_create',phase:{settled:'completed'},result:{seq:index?'9':'19'}}})),
  {key:'reply',role:'assistant',title:'Assistant',text:'Both tasks are ready.'},
  {key:'done',role:'metadata',title:'Completed',text:''},
];
const transcript = {blocks,turns:{entries:{turn:{id:'turn',status:'Completed',running:false,partial:false,foldable:true,
  blocks:blocks.map(block=>block.key),process:tasks.map(task=>task.id),answer:['reply'],candidate:[],closing_prose:[3],terminal:'done',
  timestamps:{request:time}}}}};
const transcriptOwner = {blocks:new Map(),setReadingSummary(){},action:async()=>{}};
window.scene = (mode) => {
  const cards = ['cards','stale','deleted'].includes(mode);
  const reminders = mode==='held'?[tasks[0],{...tasks[1],last_delivery:{available_ms:time+55000}}]:
    mode==='many'||mode==='cards'?tasks:mode==='one'?tasks.slice(0,1):mode==='ended'?[{...tasks[0],active:false,consumed:true}]:[];
  const schedule = {reminders,allocated_rounds:mode==='ended'?1:0,reservation:mode==='ended'?{reminder_id:'reminder-0',occurrence_ms:time,settled:true}:null};
  source.set({surfaces:{main:{kind:'native',session:'research',generation:'1',ui_surfaces:[],transcript:cards?transcript:undefined,projections:{
    cursor:{kind:'durable',fact_seq:mode==='stale'?8:19},entries:[{producer:'rsi.schedule.projection',content:{kind:'value',value:schedule}}]}}},application_surfaces:[]});
};
const calls=[];
window.calls=calls;
const owner=new ResourceOwner(async(method,text)=>{calls.push(method);return JSON.stringify({value:[]})});
installActions({resources:()=>owner});
presentationIdentity.set('schedule-summary-fixture');
installDisclosures(document);
window.scene('one');
function App(){
  const viewport=useViewport();
  const [layout,setLayout]=useState({...defaultLayout,resourcesClosed:true});
  const surface=useView(view=>view.surfaces.main);
  return <LayoutContext.Provider value={{layout,update:patch=>setLayout(value=>({...value,...patch})),expand(){}}}>
    <main className={scope.scope} style={{width:'100%',maxWidth:900,margin:'0 auto',padding:16}}><IconSprite/>
      <header className="pane-heading" style={{display:'flex',alignItems:'center'}}><strong>Research</strong>
        <ScheduledTasks pane="main" surface={surface}/><details className="session-menu"><summary aria-label="Session tools">…</summary></details>
      </header>
      {surface.transcript && <Transcript pane="main" owner={transcriptOwner} transcript={surface.transcript} generation="1" mode="standard" cards={false} register={()=>{}}/>}
      <ResourceDocks viewport={viewport}/>
    </main>
  </LayoutContext.Provider>;
}
createRoot(document.getElementById('root')).render(<App/>);
`});
const results = [];
try {
  for (const [name,engine] of Object.entries({chromium,firefox})) {
    const browser = await engine.launch();
    try {
      const page = await browser.newPage({viewport:{width:1000,height:720}});
      await page.clock.install({time:new Date(Date.UTC(2026,10,20,8,29,30))});
      const errors = [];
      page.on('pageerror', error => errors.push(error.message));
      await page.goto(fixture.url);
      await page.getByRole('button',{name:'1 automation task',exact:true}).click();
      const detail = page.getByRole('region',{name:'Automation task details'});
      await detail.getByText('Review open pull requests').waitFor();
      await page.screenshot({path:join(report,name+'-detail.png')});
      await page.evaluate(()=>window.scene('ended'));
      await detail.getByText('Ended',{exact:true}).waitFor();
      assert.equal(await page.getByRole('button',{name:/automation task$/}).count(),0);
      await page.evaluate(()=>window.scene('empty'));
      await page.getByText('This task is unavailable.').waitFor();
      await page.evaluate(()=>window.scene('many'));
      await page.setViewportSize({width:420,height:760});
      await page.getByRole('button',{name:'Hide resources'}).click();
      const trigger = page.getByLabel('2 automation tasks',{exact:true});
      await trigger.click();
      const list = page.getByRole('list',{name:'Enabled automation tasks'});
      const first = list.getByRole('button').first();
      assert.match(await first.innerText(),/Prepare the weekly summary/);
      const box = await list.boundingBox();
      await page.screenshot({path:join(report,name+'-narrow-list.png')});
      assert(box && box.x >= 0 && box.x + box.width <= 420, JSON.stringify(box));
      assert.match(await first.innerText(), /in \d+ seconds/);
      await page.clock.fastForward(45_000);
      await first.getByText(/seconds overdue/).waitFor();
      await first.focus();
      await page.keyboard.press('Escape');
      assert(await list.isHidden());
      assert(await trigger.evaluate(node=>document.activeElement===node));
      await trigger.click();
      await page.evaluate(()=>window.scene('one'));
      await page.getByRole('button',{name:'1 automation task',exact:true}).waitFor();
      await page.evaluate(()=>window.scene('many'));
      await trigger.waitFor();
      assert(await list.isHidden());
      await trigger.click();
      await first.click();
      await detail.getByText('Prepare the weekly summary').waitFor();
      await page.evaluate(()=>window.scene('held'));
      assert.equal(await detail.locator('time').getAttribute('datetime'),'2026-11-20T09:30:55.000Z');
      await page.getByRole('button',{name:'Hide resources'}).click();
      await trigger.click();
      assert.match(await first.innerText(),/Review open pull requests/);
      assert.equal(await list.locator('time').last().getAttribute('datetime'),'2026-11-20T09:30:55.000Z');
      await first.press('Escape');
      await page.evaluate(()=>window.scene('cards'));
      const transcript = page.getByRole('region',{name:'Conversation transcript'});
      const tail = transcript.locator('[data-turn-tail="turn"]');
      const open = tail.getByRole('button',{name:/^Open automation task:/});
      await open.first().waitFor();
      assert.deepEqual(await open.evaluateAll(nodes=>nodes.map(node=>node.getAttribute('aria-label'))),[
        'Open automation task: Prepare the weekly summary','Open automation task: Review open pull requests']);
      assert.equal(await transcript.locator('.message.tool:visible').count(),0);
      const notification = transcript.locator('.turn-trigger');
      const wake = notification.getByRole('button',{name:/Automation task/});
      assert.equal(await wake.getAttribute('aria-expanded'),'false');
      assert.equal(await notification.locator('.message-text').count(),0);
      await wake.focus();await page.keyboard.press('Enter');
      assert.equal(await notification.locator('.message-text').innerText(),"Review today's two tasks.");
      const bodyId = await wake.getAttribute('aria-controls');
      assert(bodyId && await page.evaluate(id=>!!document.getElementById(id),bodyId));
      await notification.getByRole('button',{name:'Copy message'}).waitFor();
      await page.screenshot({path:join(report,name+'-notification-open.png')});
      await page.keyboard.press('Space');
      assert.equal(await notification.locator('.message-text').count(),0);
      await page.screenshot({path:join(report,name+'-turn-cards.png')});
      await open.last().click();
      await detail.getByText('Review open pull requests').waitFor();
      await page.evaluate(()=>window.scene('stale'));
      await tail.getByRole('button',{name:'Open automation task: Original 0'}).waitFor();
      await page.evaluate(()=>window.scene('deleted'));
      await tail.getByLabel('Deleted automation task').last().waitFor();
      assert.equal(await tail.getByLabel('Deleted automation task').count(),2);
      assert.equal(await open.count(),0);
      assert.equal(await page.evaluate(()=>window.calls.includes('resource_open')),false);
      assert.deepEqual(errors,[]);
      results.push({name,version:browser.version(),nextRunOrder:true,relativeClock:true,changedSelectionClosesMenu:true,keyboardDismissal:true,narrowBounds:box,
        foldedCreationCards:2,creationResultOrder:['9','19'],opensDetails:true,olderProjectionKeepsReceipt:true,coveredDeletionDisablesAction:true,
        triggerInitiallyFolded:true,originalNotificationOnKeyboardOpen:true});
    } finally { await browser.close(); }
  }
  await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally { await fixture.close(); }
