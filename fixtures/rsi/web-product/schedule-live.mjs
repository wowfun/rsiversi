import {openBrowserPage, connectWorkbench, openWorkspace} from './browser-fixture.mjs';
import {cleanupAll} from './cleanup.mjs';
// Real provider reminder lifecycle; never included in default validation.
import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {chromium} from 'playwright';
import {liveFixture} from './live-fixture.mjs';
import {assertNoNotices} from './task-checks.mjs';
const fixture=await liveFixture(),{service,report,model}=fixture;
let page,browser;const errors=[],started=Date.now();
try {
  ({browser, page} = await openBrowserPage(chromium, errors));
  await connectWorkbench(page, service, 'live finite reminder');
  await openWorkspace(page, service);
  const pane=page.getByRole('region',{name:'Main conversation',exact:true}),input=pane.getByRole('textbox',{name:'Main message',exact:true});
  await input.fill('This is an isolated integration test. Call schedule_create exactly once, with rule {"kind":"after","delay_ms":3000} and prompt "This is the one authorized reminder. Call schedule_list to inspect the spent allowance, then answer LIVE_SCHEDULE_ONCE. Do not create, resume or delete any reminder. Do not modify files." After creation succeeds, immediately finish your current turn with REMINDER_CREATED. Do not use any waiting tool or wait for the due time.');
  await pane.getByRole('button',{name:'Send ↗',exact:true}).click();
  const deadline=Date.now()+180000;
  while(Date.now()<deadline) {
    const text=await pane.locator('.transcript').innerText();
    if(text.includes('Automatic parent round 1/100.') && text.includes('LIVE_SCHEDULE_ONCE') && await pane.locator('.pane-status').innerText()==='Completed')break;
    await page.waitForTimeout(100);
  }
  const transcript=await pane.locator('.transcript').innerText();await writeFile(join(report,'transcript.txt'),transcript);
  assert.equal(await pane.locator('.pane-status').innerText(),'Completed',transcript.slice(-4096));
  const session=(await pane.locator('.pane-session').innerText()).split(' · ').at(-1);
  const records=service.run(['--profile','evidence-cli','--resume',session,'--output','jsonl'],{input:':history\n'.repeat(64)+':exit\n'}).stdout.trim().split('\n').map(line=>JSON.parse(line));
  const facts=records.flatMap(record=>record.fact?[record.fact]:[]).sort((a,b)=>a.seq-b.seq);await writeFile(join(report,'facts.json'),JSON.stringify(facts,null,2));
  const creates=facts.filter(f=>f.type==='tool_intent' && f.name==='schedule_create');assert.equal(creates.length,1);
  const created=facts.find(f=>f.type==='tool_result' && f.effect_id===creates[0].effect_id);assert.equal(created?.result.value.status,'created_armed');
  const automatic=facts.filter(f=>f.type==='input_message_entered' && f.source?.type==='continuation');
  assert.equal(automatic.length,1,'one automatic input');assert.equal(automatic[0].source.source.domain.id,'rsi.schedule');
  const list=facts.find(f=>f.type==='tool_intent' && f.name==='schedule_list' && f.turn_id===automatic[0].turn_id);assert(list,'automatic Turn must call ordinary Tool');
  const listed=facts.find(f=>f.type==='tool_result' && f.effect_id===list.effect_id);assert.equal(listed?.result.value.state.allocated_rounds,1);assert.equal(listed.result.value.state.reminders.length,1);assert.equal(listed.result.value.state.reminders[0].consumed,true);
  const text=facts.filter(f=>f.type==='model_event' && f.turn_id===automatic[0].turn_id).map(f=>f.event?.type==='content_delta' && f.event.delta?.type==='text' ? f.event.delta.value : '').join('');assert.match(text,/LIVE_SCHEDULE_ONCE/);
  assert.equal(service.provider.requests.length,0);await page.getByRole('region',{name:'Needs attention',exact:true}).getByText('Running',{exact:true}).waitFor({state:'hidden'});await page.screenshot({path:join(report,'schedule-wide.png')});
  await page.setViewportSize({width:420,height:900});await pane.locator('.transcript').evaluate(element=>{element.scrollTop=element.scrollHeight;});await page.screenshot({path:join(report,'schedule-narrow.png')});await assertNoNotices(page);assert.deepEqual(errors,[]);
  await writeFile(join(report,'result.json'),JSON.stringify({ok:true,browser:browser.version(),model,reasoning_effort:'off',mock_model_requests:0,automatic_rounds:1,tool_calls:['schedule_create','schedule_list'],elapsed_ms:Date.now()-started},null,2));
}catch(error){if(page)await page.screenshot({path:join(report,'failure.png')}).catch(()=>{});await writeFile(join(report,'failure.txt'),fixture.redact(error));throw new Error(fixture.redact(error));}
finally {await cleanupAll(() => browser?.close(), () => fixture.close());}
