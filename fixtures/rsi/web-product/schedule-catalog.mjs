import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {chromium, firefox} from 'playwright';
import {sourceFixture} from './source-fixture.mjs';

const report = resolve(process.env.RSI_REPORT_DIR);
await mkdir(report, {recursive:true});
const fixture = await sourceFixture({name:'schedule-catalog',source:`
import {createRoot} from 'react-dom/client';
import {loadScheduleCatalog,installActions,publish} from '/src/bridge.ts';
import ScheduleCatalog from '/src/schedule-catalog.tsx';
import {IconSprite} from '/src/icon-sprite.tsx';
import '/styles.css';

const events = window.events = [];
window.renameLinkedSession = title => publish({navigation:{entries:[{session:'research',metadata:{title}}]}});
window.renameLinkedSession('Research notes');
const group = (session_id, rules) => ({position:{session_id}, archived:session_id==='archived', exhausted:false, reminders:rules.map(rule=>({rule,executing:session_id==='research'}))});
function reminder(prompt,active=true,consumed=false) {
  return {id:'task',prompt,active,consumed,interval_ms:null,due_ms:Date.UTC(2026,10,active?20:19,9,30)};
}
const pages = location.search==='?empty' ? [] : [
  [group('research',[reminder('Review open issues')])],
  [group('writing',[reminder('Prepare release notes',false)]),group('archived',[reminder('Past release',false,true)])],
];
if(location.search==='?floor') {
  const held={...reminder('Held task'),last_delivery:{available_ms:Date.UTC(2026,10,20,9,30,55)}};
  const other={...reminder('Other task'),id:'other',due_ms:Date.UTC(2026,10,20,9,30,20)};
  pages.splice(0,pages.length,[group('writing',[held,other])]);
}
const updates=pages.map((sessions,index)=>({kind:'page',index,sessions}));
updates.push({kind:'complete',pages:pages.length});
let waiting, attempts=0, deletes=0, runs=0;
window.changeCatalog = value => {
  if (waiting) { const take=waiting; waiting=undefined; take(JSON.stringify(value)); }
  else updates.push(value);
};
installActions({
  async call(method,{action}) {
    if (method !== 'scheduled_tasks') throw new Error('unexpected method '+method);
    if (action === 'close') { waiting?.(); waiting=undefined; return; }
    if (action === 'open' && ++attempts===1) throw new Error('Catalog temporarily unavailable');
    if (updates.length) return JSON.stringify(updates.shift());
    return new Promise(resolve=>{waiting=resolve});
  },
  async open(command,text) { events.push({command,text}); },
  async command(command) {
    events.push(command);
    if (command.action==='run_scheduled_task') {
      if (++runs===1) throw new Error('Run temporarily unavailable');
      window.changeCatalog({kind:'page',index:1,sessions:[{position:{session_id:'writing'},archived:false,exhausted:true,
        reminders:[{rule:{...reminder('Prepare release notes again',false),last_delivery:{available_ms:Date.now()+60000}},executing:false}]}]});
      window.changeCatalog({kind:'complete',pages:2});
      return;
    }
    if (++deletes === 1) throw new Error('Could not delete task');
    window.changeCatalog({kind:'page',index:1,sessions:[]});
    window.changeCatalog({kind:'complete',pages:2});
  }
});
void loadScheduleCatalog();
createRoot(document.getElementById('root')).render(<><IconSprite/><ScheduleCatalog onClose={()=>events.push('closed')} onDeleted={()=>events.push('deleted')}/></>);
`});
const results = [];
try {
  for (const [name, engine] of Object.entries({chromium,firefox})) {
    const browser = await engine.launch();
    try {
      const page = await browser.newPage({viewport:{width:1280,height:850},locale:'zh-CN',timezoneId:'Asia/Shanghai'});
      const errors = [];
      page.on('pageerror',error=>errors.push(error.message));
      await page.clock.install({time:new Date('2026-11-20T09:29:15Z')});
      await page.clock.pauseAt(new Date('2026-11-20T09:29:40Z'));
      await page.goto(fixture.url);
      const manager = page.getByRole('region',{name:'Automation tasks',exact:true});
      await manager.getByRole('alert').getByRole('button',{name:'Retry'}).click();
      const list = manager.getByRole('list',{name:'Task catalog'});
      await list.getByRole('button',{name:/Review open issues/}).waitFor();
      assert.equal(await list.getByRole('button').count(),1);
      const stamp = list.locator('time');
      assert.equal(await stamp.getAttribute('datetime'),'2026-11-20T09:30:00.000Z');
      assert.match(await stamp.innerText(),/Nov 20, 2026.*17:30/);
      const activeRow = list.getByRole('button',{name:'Review open issues',exact:true});
      assert.equal(await activeRow.getAttribute('aria-expanded'),'false');
      assert.match(await activeRow.innerText(),/in 20 seconds/);
      await activeRow.click();
      const panel = manager.getByRole('complementary',{name:'Selected task'});
      const detailRatio = () => panel.evaluate(el=>el.getBoundingClientRect().width/el.closest('section[aria-label="Automation tasks"]').getBoundingClientRect().width);
      assert(Math.abs(await detailRatio()-.47)<.002,'Desktop detail should take 47% of the task manager');
      await page.setViewportSize({width:760,height:850});
      assert(await list.isHidden());
      assert(Math.abs(await detailRatio()-1)<.002);
      await page.setViewportSize({width:761,height:850});
      assert(await list.isVisible());
      assert(Math.abs(await detailRatio()-.47)<.002);
      await page.setViewportSize({width:1280,height:850});
      await panel.getByRole('button',{name:'Automation task actions'}).click();
      const heldDelete = page.getByRole('menuitem',{name:'Delete task'});
      assert(await heldDelete.isDisabled());
      assert(await page.getByRole('menuitem',{name:'Run now'}).isDisabled());
      await page.keyboard.press('Escape');
      await heldDelete.waitFor({state:'hidden'});
      assert.equal(await activeRow.getAttribute('aria-controls'),await panel.getAttribute('id'));
      assert.equal(await activeRow.getAttribute('aria-expanded'),'true');
      const namedLink = panel.getByRole('button',{name:'Linked session Research notes: open linked session',exact:true});
      assert.equal(await namedLink.innerText({timeout:2000}),'Research notes');
      await page.evaluate(()=>window.renameLinkedSession('Research journal'));
      assert.equal(await panel.getByRole('button',{name:'Linked session Research journal: open linked session',exact:true}).innerText(),'Research journal');
      await page.clock.fastForward(30_000);
      const description = await activeRow.getAttribute('aria-describedby');
      assert.match(await page.locator('[id="'+description+'"]').innerText(),/10 seconds overdue/);
      assert.match(await panel.innerText(),/10 seconds overdue/);
      await manager.getByRole('button',{name:'Close details'}).click();
      await manager.getByRole('button',{name:'All',exact:true}).click();
      assert.equal(await list.getByRole('button').count(),3);
      assert.match(await list.getByRole('button').last().innerText(),/Review open issues/);
      await page.screenshot({path:join(report,name+'-wide.png')});
      await manager.getByRole('button',{name:'Ended',exact:true}).click();
      await list.getByRole('button',{name:/Past release/}).click();
      await panel.getByText('The linked session is archived.',{exact:true}).waitFor();
      const archiveLink = panel.getByRole('button',{name:'Linked session: open linked session',exact:true});
      assert(await archiveLink.isDisabled());
      assert.equal(await archiveLink.innerText(),'archived');
      await panel.getByRole('button',{name:'Automation task actions'}).click();
      assert(await page.getByRole('menuitem',{name:'Run now'}).isDisabled());
      await page.keyboard.press('Escape');
      await page.screenshot({path:join(report,name+'-archived.png')});
      await page.evaluate(() => {
        window.changeCatalog({kind:'page',index:1,sessions:['writing','archived'].map(session_id => ({
          position:{session_id},archived:false,exhausted:false,reminders:[{executing:false,rule:{
            id:'task',prompt:session_id==='writing'?'Prepare release notes':'Past release',active:false,
            consumed:session_id==='archived',interval_ms:null,due_ms:Date.UTC(2026,10,19,9,30),
          }}],
        }))});
        window.changeCatalog({kind:'complete',pages:2});
      });
      await panel.getByText('The linked session is archived.',{exact:true}).waitFor({state:'hidden'});
      assert(await archiveLink.isEnabled());
      await manager.getByRole('button',{name:'Close details'}).click();
      await manager.getByRole('button',{name:'Paused',exact:true}).click();
      const search = manager.getByRole('searchbox');
      await search.fill('absent');
      await manager.getByText('No matching tasks').waitFor();
      await manager.getByRole('button',{name:'Clear search'}).click();
      assert.equal(await search.inputValue(),'');
      assert(await search.evaluate(field=>document.activeElement===field));
      const row = list.getByRole('button',{name:/Prepare release notes/});
      for (const term of ['writing','ｒｅｌｅａｓｅ WRI','Prp',' RELEASE ']) {
        await search.fill(term);
        assert(await row.isVisible(), 'Cannot find the writing task with '+term);
      }
      await page.setViewportSize({width:420,height:850});
      await row.click();
      const detail = manager.getByRole('region',{name:'Automation task details'});
      await detail.getByText('Paused',{exact:true}).waitFor();
      const close = manager.getByRole('button',{name:'Close details'});
      assert(await close.evaluate(button=>button===document.activeElement));
      assert(await list.isHidden());
      await page.screenshot({path:join(report,name+'-narrow.png')});
      const linked = manager.getByRole('button',{name:'Linked session: open linked session',exact:true});
      await linked.focus();
      await page.evaluate(()=>window.changeCatalog({kind:'page',index:1,sessions:[{
        position:{session_id:'writing'},archived:false,exhausted:false,reminders:[{executing:false,rule:{id:'task',prompt:'Prepare release notes today',active:false,consumed:false,interval_ms:null,due_ms:Date.UTC(2026,10,20,9,30)}}],
      }]}));
      await detail.getByText('Prepare release notes',{exact:true}).waitFor();
      await page.evaluate(()=>window.changeCatalog({kind:'complete',pages:2}));
      await detail.getByText('Prepare release notes today',{exact:true}).waitFor();
      assert(await linked.evaluate(button=>button===document.activeElement));
      await close.press('Escape');
      assert(await row.evaluate(button=>button===document.activeElement));
      await row.click();
      await page.evaluate(()=>window.changeCatalog({kind:'complete',pages:1}));
      await detail.waitFor({state:'hidden'});
      assert(await manager.getByRole('heading',{name:'Automation tasks'}).evaluate(title=>title===document.activeElement));
      await page.evaluate(()=>{
        window.changeCatalog({kind:'page',index:1,sessions:[{position:{session_id:'writing'},archived:false,exhausted:false,reminders:[
          {executing:false,rule:{id:'task',prompt:'Prepare release notes again',active:false,consumed:false,interval_ms:null,due_ms:Date.UTC(2026,10,20,9,30)}}
        ]}]});
        window.changeCatalog({kind:'complete',pages:2});
      });
      const restored = list.getByRole('button',{name:/Prepare release notes again/});
      await restored.waitFor();
      assert(await detail.isHidden());
      await restored.click();
      const actions = panel.getByRole('button',{name:'Automation task actions'});
      await actions.click();
      const runNow = page.getByRole('menuitem',{name:'Run now'});
      await runNow.click();
      await panel.getByRole('alert').filter({hasText:'Run temporarily unavailable'}).waitFor();
      await actions.click();
      await runNow.click();
      await actions.click();
      await page.waitForFunction(()=>Array.from(document.querySelectorAll('[role="menuitem"]')).some(el=>el.textContent==='Run now'&&(el.matches(':disabled')||el.getAttribute('aria-disabled')==='true')));
      await page.screenshot({path:join(report,name+'-run-now-floor.png')});
      await page.clock.fastForward(60000);
      assert(await runNow.isEnabled());
      await page.keyboard.press('Escape');
      async function requestDeletion() {
        await actions.click();
        await page.getByRole('menuitem',{name:'Delete task'}).click();
      }
      await requestDeletion();
      const confirm = page.getByRole('dialog',{name:'Delete this task?'});
      const cancel = confirm.getByRole('button',{name:'Cancel'});
      assert(await cancel.evaluate(button=>button===document.activeElement));
      await page.screenshot({path:join(report,name+'-delete-confirm.png')});
      await cancel.press('Escape');
      await confirm.waitFor({state:'hidden'});
      assert(await actions.evaluate(button=>button===document.activeElement));
      await requestDeletion();
      const submit = confirm.getByRole('button',{name:'Confirm deletion'});
      await submit.click();
      await confirm.getByText('Could not delete task',{exact:true}).waitFor();
      assert.equal(await page.evaluate(()=>window.events.includes('deleted')),false);
      await cancel.click();
      await detail.getByText('Prepare release notes again',{exact:true}).waitFor();
      await requestDeletion();
      await submit.click();
      await confirm.waitFor({state:'hidden'});
      await restored.waitFor({state:'detached'});
      assert(await manager.getByRole('heading',{name:'Automation tasks'}).evaluate(title=>title===document.activeElement));
      await manager.getByRole('button',{name:'Enabled',exact:true}).click();
      await manager.getByRole('button',{name:'Clear search'}).click();
      await activeRow.click();
      await panel.getByRole('button',{name:/open linked session$/i}).click();
      await manager.getByRole('button',{name:'Create in a session'}).click();
      await manager.getByRole('button',{name:'Close',exact:true}).click();
      const events = await page.evaluate(()=>window.events);
      assert.deepEqual(events,[
        {action:'run_scheduled_task',task:{session_id:'writing',id:'task'}},
        {action:'run_scheduled_task',task:{session_id:'writing',id:'task'}},
        {action:'delete_scheduled_task',task:{session_id:'writing',id:'task'}},
        {action:'delete_scheduled_task',task:{session_id:'writing',id:'task'}},
        'deleted',
        {command:{action:'open',session:'research'},text:undefined},
        {command:{action:'new_conversation'},text:'Please create an automation task based on the requirements.'},
        'closed',
      ]);
      await page.goto(fixture.url+'?empty');
      await manager.getByRole('alert').getByRole('button',{name:'Retry'}).click();
      await manager.getByText('No active automation tasks').waitFor();
      const suggestions = manager.getByRole('region',{name:'Recommended'});
      assert.equal(await suggestions.getByRole('button').count(),8);
      await page.screenshot({path:join(report,name+'-recommendations.png')});
      await manager.getByRole('searchbox').fill('absent');
      assert(await suggestions.isHidden());
      await manager.getByRole('searchbox').fill('');
      await suggestions.getByRole('button',{name:'Weekly review',exact:true}).click();
      const proposal=await page.evaluate(()=>window.events.at(-1));
      assert.deepEqual(proposal.command,{action:'new_conversation'});
      assert.match(proposal.text,/Weekly review/);
      assert.match(proposal.text,/Friday/);
      await page.goto(fixture.url+'?floor');
      await manager.getByRole('alert').getByRole('button',{name:'Retry'}).click();
      await page.setViewportSize({width:1280,height:850});
      const held = list.getByRole('button',{name:'Held task',exact:true});
      await held.waitFor();
      assert.deepEqual(await list.locator('strong').allTextContents(),['Other task','Held task']);
      await held.click();
      for(const stamp of [held.locator('time'),panel.locator('time')])
        assert.equal(await stamp.getAttribute('datetime'),'2026-11-20T09:30:55.000Z');
      assert.deepEqual(errors,[]);
      results.push({name,version:browser.version(),statusFilters:4,focusReturned:true,updatePreservedFocus:true,
        searchBySessionAndFuzzyTitle:true,removedSelectionRetired:true,recommendations:8,executingTaskProtected:true,
        deletionCancelReturnedFocus:true,deletionFailureRetainedTask:true,retryRemovedTask:true,
        taskExpandedAndDescribed:true,sharedCountdownCrossedDeadline:true,archivedSessionLinkDisabled:true,
        linkedTitleUpdates:true,linkedIdFallback:true,deliveryFloorDeterminesNextRun:true});
    } finally { await browser.close(); }
  }
  await writeFile(join(report,'result.json'),JSON.stringify(results,null,2));
} finally { await fixture.close(); }
